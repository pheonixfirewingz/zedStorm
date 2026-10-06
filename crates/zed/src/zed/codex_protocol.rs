use anyhow::{Context as _, Result, bail};
use futures::{AsyncBufReadExt as _, AsyncWriteExt as _, StreamExt as _};
use serde_json::{Value, json};
use smol::{channel, io::BufReader, process::Command};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio};

pub const MAX_ACTIVITY_TEXT_LEN: usize = 64 * 1024;

pub async fn run_server(
    directory: PathBuf,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) {
    let result = serve(directory, outgoing, incoming.clone()).await;
    if let Err(error) = result {
        if let Err(error) = incoming.send(Err(error)).await {
            log::debug!("Codex panel closed: {error}");
        }
    }
}

async fn serve(
    directory: PathBuf,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) -> Result<()> {
    let mut child = Command::new("codex")
        .arg("app-server")
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Could not start Codex. Install Codex CLI and ensure `codex` is on your PATH.")?;
    let mut stdin = child.stdin.take().context("Codex stdin is unavailable")?;
    let stdout = child.stdout.take().context("Codex stdout is unavailable")?;
    let stderr = child.stderr.take().context("Codex stderr is unavailable")?;
    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next().await {
            let message = serde_json::from_str(&line?).context("Invalid Codex server response")?;
            incoming.send(Ok(message)).await?;
        }
        bail!("Codex disconnected. Reconnect to continue this chat.")
    };
    let write = async {
        while let Ok(message) = outgoing.recv().await {
            let mut bytes = serde_json::to_vec(&message)?;
            bytes.push(b'\n');
            stdin.write_all(&bytes).await?;
            stdin.flush().await?;
        }
        bail!("Codex connection closed")
    };
    let diagnostics = async {
        let mut lines = BufReader::new(stderr).lines();
        while let Some(line) = lines.next().await {
            log::debug!("Codex: {}", line?);
        }
        futures::future::pending::<Result<()>>().await
    };
    let result: Result<((), (), ())> = futures::try_join!(read, write, diagnostics);
    // Retain ownership until the I/O futures finish so dropping the panel kills the process.
    drop(child);
    result.map(|_| ())
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageKind {
    User,
    Assistant,
    Activity,
}

pub struct Message {
    pub id: String,
    pub kind: MessageKind,
    pub text: String,
}

#[derive(Clone)]
pub struct ServerRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Copy)]
enum RequestKind {
    Initialize,
    Account,
    StartThread,
    ResumeThread,
    StartTurn,
    Interrupt,
    Login,
}

pub struct Session {
    pub directory: PathBuf,
    pub ready: bool,
    pub busy: bool,
    pub needs_sign_in: bool,
    pub signing_in: bool,
    pub model: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub messages: Vec<Message>,
    pub requests: Vec<ServerRequest>,
    pub error: Option<String>,
    pub auth_url: Option<String>,
    pub stopped: bool,
    next_id: u64,
    pending: BTreeMap<u64, RequestKind>,
    prompt: Option<String>,
    stop_requested: bool,
}

impl Session {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            ready: false,
            busy: false,
            needs_sign_in: false,
            signing_in: false,
            model: None,
            thread_id: None,
            turn_id: None,
            messages: Vec::new(),
            requests: Vec::new(),
            error: None,
            auth_url: None,
            stopped: false,
            next_id: 0,
            pending: BTreeMap::new(),
            prompt: None,
            stop_requested: false,
        }
    }

    fn request(&mut self, kind: RequestKind, method: &str, params: Value) -> Value {
        self.next_id += 1;
        self.pending.insert(self.next_id, kind);
        json!({"id": self.next_id, "method": method, "params": params})
    }

    pub fn initialize(&mut self) -> Value {
        self.pending.clear();
        self.ready = false;
        self.busy = false;
        self.signing_in = false;
        self.turn_id = None;
        self.requests.clear();
        self.error = None;
        self.request(RequestKind::Initialize, "initialize", json!({
            "clientInfo": {"name": "zedstorm", "title": "ZedStorm", "version": env!("CARGO_PKG_VERSION")}
        }))
    }

    pub fn send_prompt(&mut self, text: String) -> Option<Value> {
        if !self.ready || self.busy || self.needs_sign_in || text.trim().is_empty() {
            return None;
        }
        self.error = None;
        self.stopped = false;
        self.stop_requested = false;
        self.busy = true;
        self.messages.push(Message {
            id: format!("user-{}", self.next_id + 1),
            kind: MessageKind::User,
            text: text.clone(),
        });
        self.prompt = Some(text);
        if self.thread_id.is_some() {
            self.start_turn()
        } else {
            Some(self.request(
                RequestKind::StartThread,
                "thread/start",
                json!({"cwd": self.directory}),
            ))
        }
    }

    fn start_turn(&mut self) -> Option<Value> {
        if self.stop_requested {
            self.busy = false;
            self.prompt = None;
            self.stopped = true;
            return None;
        }
        let thread_id = self.thread_id.clone()?;
        let text = self.prompt.take()?;
        Some(self.request(
            RequestKind::StartTurn,
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": text, "text_elements": []}]
            }),
        ))
    }

    pub fn interrupt(&mut self) -> Option<Value> {
        self.stop_requested = true;
        let thread_id = self.thread_id.clone()?;
        let turn_id = self.turn_id.clone()?;
        Some(self.request(
            RequestKind::Interrupt,
            "turn/interrupt",
            json!({"threadId": thread_id, "turnId": turn_id}),
        ))
    }

    pub fn login(&mut self) -> Option<Value> {
        if !self.ready || self.signing_in {
            return None;
        }
        self.signing_in = true;
        self.error = None;
        Some(self.request(
            RequestKind::Login,
            "account/login/start",
            json!({"type": "chatgpt"}),
        ))
    }

    pub fn disconnected(&mut self, error: String) {
        self.ready = false;
        self.busy = false;
        self.signing_in = false;
        self.turn_id = None;
        self.requests.clear();
        self.error = Some(error);
    }

    pub fn receive(&mut self, message: Value) -> Vec<Value> {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if let Some(id) = message.get("id") {
                return self.server_request(id.clone(), method, params);
            }
            return self.notification(method, params);
        }
        let Some(kind) = message
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| self.pending.remove(&id))
        else {
            return Vec::new();
        };
        if let Some(error) = message.get("error") {
            self.error = Some(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex request failed")
                    .to_owned(),
            );
            match kind {
                RequestKind::Initialize | RequestKind::ResumeThread => self.ready = false,
                RequestKind::StartThread | RequestKind::StartTurn => {
                    self.busy = false;
                    self.prompt = None;
                }
                RequestKind::Login => self.signing_in = false,
                _ => {}
            }
            return Vec::new();
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        match kind {
            RequestKind::Initialize => {
                let mut outgoing = vec![json!({"method": "initialized"})];
                if let Some(thread_id) = self.thread_id.clone() {
                    outgoing.push(self.request(
                        RequestKind::ResumeThread,
                        "thread/resume",
                        json!({"threadId": thread_id}),
                    ));
                } else {
                    self.ready = true;
                }
                outgoing.push(self.request(
                    RequestKind::Account,
                    "account/read",
                    json!({"refreshToken": false}),
                ));
                outgoing
            }
            RequestKind::Account => {
                self.needs_sign_in = result
                    .get("requiresOpenaiAuth")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                    && result.get("account").is_none_or(Value::is_null);
                Vec::new()
            }
            RequestKind::StartThread | RequestKind::ResumeThread => {
                let Some(thread_id) = result.pointer("/thread/id").and_then(Value::as_str) else {
                    self.disconnected("Codex returned a thread without an ID".into());
                    return Vec::new();
                };
                self.thread_id = Some(thread_id.to_owned());
                self.model = result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if matches!(kind, RequestKind::ResumeThread) {
                    self.ready = true;
                    self.messages.clear();
                    if let Some(turns) = result.pointer("/thread/turns").and_then(Value::as_array) {
                        for turn in turns {
                            if let Some(items) = turn.get("items").and_then(Value::as_array) {
                                for item in items {
                                    self.item(item, true);
                                }
                            }
                        }
                    }
                    Vec::new()
                } else {
                    self.start_turn().into_iter().collect()
                }
            }
            RequestKind::StartTurn => {
                // A completion notification can arrive before the response for a very short turn.
                if self.busy {
                    self.turn_id = result
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if self.stop_requested {
                        return self.interrupt().into_iter().collect();
                    }
                }
                Vec::new()
            }
            RequestKind::Login => {
                self.auth_url = result
                    .get("authUrl")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Vec::new()
            }
            RequestKind::Interrupt => Vec::new(),
        }
    }

    fn belongs_to_thread(&self, params: &Value) -> bool {
        params
            .get("threadId")
            .and_then(Value::as_str)
            .is_none_or(|thread_id| self.thread_id.as_deref() == Some(thread_id))
    }

    fn server_request(&mut self, id: Value, method: &str, params: Value) -> Vec<Value> {
        if !self.busy
            || !self.belongs_to_thread(&params)
            || params
                .get("turnId")
                .and_then(Value::as_str)
                .is_some_and(|turn_id| self.turn_id.as_deref() != Some(turn_id))
        {
            return vec![
                json!({"id": id, "error": {"code": -32600, "message": "This request does not belong to the active turn"}}),
            ];
        }
        match method {
            "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/requestUserInput"
            | "tool/requestUserInput" => {
                if !self.requests.iter().any(|request| request.id == id) {
                    self.requests.push(ServerRequest {
                        id,
                        method: method.into(),
                        params,
                    });
                }
                Vec::new()
            }
            _ => {
                self.error = Some(format!("This Codex request is not supported yet: {method}"));
                vec![
                    json!({"id": id, "error": {"code": -32601, "message": "Unsupported client request"}}),
                ]
            }
        }
    }

    pub fn answer(&mut self, id: &Value, result: Value) -> Option<Value> {
        let index = self.requests.iter().position(|request| &request.id == id)?;
        self.requests.remove(index);
        Some(json!({"id": id, "result": result}))
    }

    fn notification(&mut self, method: &str, params: Value) -> Vec<Value> {
        if !self.belongs_to_thread(&params) {
            return Vec::new();
        }
        if method.starts_with("item/") || method.starts_with("turn/") {
            if let Some(turn_id) = params.get("turnId").and_then(Value::as_str) {
                if self.turn_id.as_deref() != Some(turn_id) {
                    return Vec::new();
                }
            }
            if method == "turn/completed" {
                if let Some(turn_id) = params.pointer("/turn/id").and_then(Value::as_str) {
                    if self.turn_id.as_deref() != Some(turn_id) {
                        return Vec::new();
                    }
                }
            }
        }
        match method {
            "turn/started" => {
                self.busy = true;
                self.turn_id = params
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if self.stop_requested {
                    return self.interrupt().into_iter().collect();
                }
            }
            "turn/completed" => {
                self.busy = false;
                self.turn_id = None;
                self.requests.clear();
                self.stopped =
                    params.pointer("/turn/status").and_then(Value::as_str) == Some("interrupted");
                if let Some(error) = params
                    .pointer("/turn/error/message")
                    .and_then(Value::as_str)
                {
                    self.error = Some(error.into());
                }
            }
            "item/agentMessage/delta" | "item/plan/delta" => {
                if let (Some(id), Some(delta)) = (
                    params.get("itemId").and_then(Value::as_str),
                    params.get("delta").and_then(Value::as_str),
                ) {
                    let message = self.message(id, MessageKind::Assistant);
                    message.text.push_str(delta);
                }
            }
            "item/commandExecution/outputDelta" => {
                if let (Some(id), Some(delta)) = (
                    params.get("itemId").and_then(Value::as_str),
                    params.get("delta").and_then(Value::as_str),
                ) {
                    let text = &mut self.message(id, MessageKind::Activity).text;
                    if text.len() + delta.len() > MAX_ACTIVITY_TEXT_LEN {
                        let overflow = (text.len() + delta.len()) - MAX_ACTIVITY_TEXT_LEN;
                        let truncate_point = text
                            .char_indices()
                            .map(|(i, _)| i)
                            .find(|&i| i >= overflow)
                            .unwrap_or(text.len());
                        text.drain(..truncate_point);
                        if !text.starts_with("[... output truncated ...]\n") {
                            text.insert_str(0, "[... output truncated ...]\n");
                        }
                    }
                    text.push_str(delta);
                }
            }
            "item/started" | "item/completed" => {
                if let Some(item) = params.get("item") {
                    self.item(item, method == "item/completed");
                }
            }
            "serverRequest/resolved" => {
                if let Some(id) = params.get("requestId") {
                    self.requests.retain(|request| &request.id != id);
                }
            }
            "error" => {
                if let Some(error) = params.pointer("/error/message").and_then(Value::as_str) {
                    self.error = Some(error.into());
                }
            }
            "account/login/completed" => {
                self.signing_in = false;
                if params.get("success").and_then(Value::as_bool) == Some(true) {
                    self.error = None;
                    return vec![self.request(
                        RequestKind::Account,
                        "account/read",
                        json!({"refreshToken": false}),
                    )];
                }
                self.error = Some(
                    params
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex sign-in did not complete")
                        .into(),
                );
            }
            "account/updated" => {
                return vec![self.request(
                    RequestKind::Account,
                    "account/read",
                    json!({"refreshToken": false}),
                )];
            }
            "warning" | "configWarning" => {
                self.error = params
                    .get("message")
                    .or_else(|| params.get("summary"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            _ => {}
        }
        Vec::new()
    }

    fn message(&mut self, id: &str, kind: MessageKind) -> &mut Message {
        let index = self
            .messages
            .iter()
            .position(|message| message.id == id)
            .unwrap_or_else(|| {
                self.messages.push(Message {
                    id: id.into(),
                    kind,
                    text: String::new(),
                });
                self.messages.len() - 1
            });
        // The index is either an existing position or the element just inserted above.
        &mut self.messages[index]
    }

    fn item(&mut self, item: &Value, completed: bool) {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            return;
        };
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let (message_kind, text) = match kind {
            "agentMessage" | "plan" => (
                MessageKind::Assistant,
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            "userMessage" => {
                // Live user input is inserted immediately; resumed history comes from the server.
                if !completed {
                    return;
                }
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|content| {
                        content
                            .iter()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if self.messages.last().is_some_and(|message| {
                    message.kind == MessageKind::User && message.text == text
                }) {
                    return;
                }
                (MessageKind::User, text)
            }
            "commandExecution" => {
                let command = item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("Command");
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("inProgress");
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                (
                    MessageKind::Activity,
                    format!("{command}\n{status}\n{output}"),
                )
            }
            "fileChange" => {
                let paths = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|changes| {
                        changes
                            .iter()
                            .filter_map(|change| change.get("path").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                (
                    MessageKind::Activity,
                    format!(
                        "File changes: {}\n{paths}",
                        item.get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("inProgress")
                    ),
                )
            }
            "mcpToolCall" => (
                MessageKind::Activity,
                format!(
                    "{} / {}: {}",
                    item.get("server").and_then(Value::as_str).unwrap_or("MCP"),
                    item.get("tool").and_then(Value::as_str).unwrap_or("Tool"),
                    item.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("inProgress")
                ),
            ),
            "webSearch" => (
                MessageKind::Activity,
                format!(
                    "Searching: {}",
                    item.get("query")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ),
            ),
            "contextCompaction" => (MessageKind::Activity, "Compacting conversation…".into()),
            _ => return,
        };
        let message = self.message(id, message_kind);
        if completed || message.text.is_empty() {
            message.text = text;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_session() -> Session {
        let mut session = Session::new("/tmp/project".into());
        let initialize = session.initialize();
        let followups = session.receive(json!({"id": initialize["id"], "result": {}}));
        assert_eq!(
            followups.first().and_then(|message| message.get("method")),
            Some(&json!("initialized"))
        );
        for followup in followups
            .into_iter()
            .filter(|message| message.get("method") == Some(&json!("account/read")))
        {
            session.receive(json!({"id": followup["id"], "result": {"requiresOpenaiAuth": false, "account": null}}));
        }
        assert!(session.ready);
        session
    }

    fn running_session() -> Result<Session> {
        let mut session = ready_session();
        let start = session
            .send_prompt("Explain this project".into())
            .context("thread request")?;
        assert_eq!(start["method"], "thread/start");
        assert_eq!(start["params"]["cwd"], "/tmp/project");
        assert!(start["params"].get("approvalPolicy").is_none());
        assert!(start["params"].get("sandbox").is_none());
        let followups = session.receive(json!({"id": start["id"], "result": {"thread": {"id": "thread-1"}, "model": "configured-model"}}));
        let turn = followups.first().context("turn request")?;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["input"][0]["text"], "Explain this project");
        session.receive(json!({"method": "turn/started", "params": {"threadId": "thread-1", "turn": {"id": "turn-1"}}}));
        session.receive(json!({"id": turn["id"], "result": {"turn": {"id": "turn-1"}}}));
        Ok(session)
    }

    #[test]
    fn codex_streams_and_replaces_final_text_without_duplicates() -> Result<()> {
        let mut session = running_session()?;
        for delta in ["Hello", " world"] {
            session.receive(json!({"method": "item/agentMessage/delta", "params": {"threadId": "thread-1", "turnId": "turn-1", "itemId": "reply-1", "delta": delta}}));
        }
        assert_eq!(
            session.messages.last().context("streamed message")?.text,
            "Hello world"
        );
        session.receive(json!({"method": "item/completed", "params": {"threadId": "thread-1", "turnId": "turn-1", "item": {"id": "reply-1", "type": "agentMessage", "text": "Hello world!"}}}));
        assert_eq!(session.messages.len(), 2);
        assert_eq!(
            session.messages.last().context("final message")?.text,
            "Hello world!"
        );
        session.receive(json!({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}}}));
        assert!(!session.busy);
        let followup = session
            .send_prompt("Follow up".into())
            .context("followup")?;
        assert_eq!(followup["method"], "turn/start");
        assert_eq!(followup["params"]["threadId"], "thread-1");
        Ok(())
    }

    #[test]
    fn codex_approval_is_explicit_scoped_and_cleared_on_completion() -> Result<()> {
        let mut session = running_session()?;
        let request = json!({"id": "approval-1", "method": "item/commandExecution/requestApproval", "params": {"threadId": "thread-1", "turnId": "turn-1", "command": "cargo test"}});
        assert!(session.receive(request.clone()).is_empty());
        assert_eq!(session.requests.len(), 1);
        session.receive(request.clone());
        assert_eq!(session.requests.len(), 1);
        let response = session
            .answer(&json!("approval-1"), json!({"decision": "accept"}))
            .context("approval response")?;
        assert_eq!(
            response,
            json!({"id": "approval-1", "result": {"decision": "accept"}})
        );
        assert!(
            session
                .answer(&json!("approval-1"), json!({"decision": "accept"}))
                .is_none()
        );
        let wrong_thread = session.receive(json!({"id": 7, "method": "item/fileChange/requestApproval", "params": {"threadId": "another-thread", "turnId": "turn-1"}}));
        assert!(
            wrong_thread
                .first()
                .context("rejection")?
                .get("error")
                .is_some()
        );
        session.receive(request);
        session.receive(json!({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"id": "turn-1", "status": "interrupted"}}}));
        assert!(session.requests.is_empty());
        assert!(session.stopped);
        assert!(
            session
                .answer(&json!("approval-1"), json!({"decision": "accept"}))
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn codex_stop_before_thread_start_does_not_start_a_turn() -> Result<()> {
        let mut session = ready_session();
        let request = session
            .send_prompt("Change files".into())
            .context("thread request")?;
        assert!(session.interrupt().is_none());
        let followups =
            session.receive(json!({"id": request["id"], "result": {"thread": {"id": "thread-1"}}}));
        assert!(followups.is_empty());
        assert!(!session.busy);
        assert!(session.stopped);
        Ok(())
    }

    #[test]
    fn codex_stop_while_turn_start_is_pending_interrupts_when_id_arrives() -> Result<()> {
        let mut session = ready_session();
        session.thread_id = Some("thread-1".into());
        let request = session
            .send_prompt("Change files".into())
            .context("turn request")?;
        assert!(session.interrupt().is_none());
        let followups =
            session.receive(json!({"id": request["id"], "result": {"turn": {"id": "turn-1"}}}));
        let interrupt = followups.first().context("interrupt request")?;
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(interrupt["params"]["turnId"], "turn-1");
        Ok(())
    }

    #[test]
    fn codex_reconnect_resumes_history_and_preserves_cli_configuration() -> Result<()> {
        let mut session = running_session()?;
        session.disconnected("broken pipe".into());
        let initialize = session.initialize();
        let followups = session.receive(json!({"id": initialize["id"], "result": {}}));
        assert!(!session.ready);
        let resume = followups
            .iter()
            .find(|request| request["method"] == "thread/resume")
            .context("resume")?;
        assert_eq!(resume["params"]["threadId"], "thread-1");
        session.receive(json!({"id": resume["id"], "result": {"thread": {"id": "thread-1", "turns": [{"items": [
            {"id": "user-1", "type": "userMessage", "content": [{"type": "text", "text": "Earlier prompt"}]},
            {"id": "reply-1", "type": "agentMessage", "text": "Earlier reply"}
        ]}]}, "model": "configured-model"}}));
        assert!(session.ready);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(
            session.messages.first().context("resumed user")?.text,
            "Earlier prompt"
        );
        assert_eq!(session.model.as_deref(), Some("configured-model"));
        Ok(())
    }

    #[test]
    fn codex_authentication_and_request_errors_reach_the_panel() -> Result<()> {
        let mut session = ready_session();
        let initialize = session.initialize();
        let followups = session.receive(json!({"id": initialize["id"], "result": {}}));
        let account = followups
            .iter()
            .find(|request| request["method"] == "account/read")
            .context("account")?;
        session.receive(
            json!({"id": account["id"], "result": {"requiresOpenaiAuth": true, "account": null}}),
        );
        assert!(session.needs_sign_in);
        assert!(session.send_prompt("hello".into()).is_none());
        let login = session.login().context("login")?;
        session.receive(
            json!({"id": login["id"], "result": {"authUrl": "https://auth.openai.com/login"}}),
        );
        assert_eq!(
            session.auth_url.as_deref(),
            Some("https://auth.openai.com/login")
        );
        assert!(session.signing_in);
        session.receive(json!({"method": "account/login/completed", "params": {"success": false, "error": "Sign-in cancelled"}}));
        assert!(!session.signing_in);
        assert_eq!(session.error.as_deref(), Some("Sign-in cancelled"));
        session.needs_sign_in = false;
        let start = session.send_prompt("hello".into()).context("start")?;
        session.receive(json!({"id": start["id"], "error": {"message": "Usage limit reached"}}));
        assert!(!session.busy);
        assert_eq!(session.error.as_deref(), Some("Usage limit reached"));
        Ok(())
    }

    #[test]
    fn codex_late_events_cannot_modify_a_different_turn() -> Result<()> {
        let mut session = running_session()?;
        session.receive(json!({"method": "item/agentMessage/delta", "params": {"threadId": "another-thread", "turnId": "turn-1", "itemId": "reply", "delta": "wrong"}}));
        session.receive(json!({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"id": "old-turn", "status": "completed"}}}));
        assert!(session.busy);
        assert_eq!(session.messages.len(), 1);
        Ok(())
    }

    #[test]
    fn codex_activity_output_is_capped_to_prevent_memory_exhaustion() -> Result<()> {
        let mut session = running_session()?;
        let chunk = "a".repeat(1024);
        for _ in 0..100 {
            session.receive(json!({
                "method": "item/commandExecution/outputDelta",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "itemId": "cmd-1",
                    "delta": chunk
                }
            }));
        }
        let msg = session.messages.iter().find(|m| m.id == "cmd-1").context("cmd msg")?;
        assert!(msg.text.len() <= MAX_ACTIVITY_TEXT_LEN + chunk.len() + 40);
        assert!(msg.text.starts_with("[... output truncated ...]\n"));
        Ok(())
    }
}
