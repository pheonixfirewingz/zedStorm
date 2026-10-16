use anyhow::{Context as _, Result, bail, ensure};
use futures::{AsyncBufReadExt as _, AsyncReadExt as _};
use http_client::{HttpClient, Method, Request};
use serde_json::{Value, json};
use smol::{channel, io::BufReader};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type SharedConversation = Arc<Mutex<Conversation>>;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Conversation {
    messages: Vec<Value>,
    items: Vec<Value>,
    next_turn: usize,
    #[serde(default)]
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(skip)]
    active_turn: Option<String>,
    #[serde(skip)]
    steering: Vec<Value>,
}

pub fn known_context_length(model: &str) -> Option<u64> {
    let lower = model.to_ascii_lowercase();
    if lower.contains("large-4") {
        Some(1_048_576)
    } else if lower.contains("codestral-2405")
        || lower.contains("open-mistral-7b")
        || lower.contains("open-mixtral-8x7b")
        || lower.contains("mistral-small-2402")
        || lower.contains("mistral-small-2409")
    {
        Some(32_768)
    } else if lower.contains("open-mixtral-8x22b") {
        Some(65_536)
    } else if lower.contains("2603")
        || lower.contains("devstral")
        || lower.contains("codestral-25")
        || lower.contains("codestral-latest")
        || lower.contains("mistral-medium-3-5")
        || lower.contains("mistral-large-2411")
        || lower.contains("mistral-large-latest")
    {
        Some(262_144)
    } else if lower.contains("mistral-large")
        || lower.contains("mistral-medium")
        || lower.contains("mistral-small")
        || lower.contains("ministral")
        || lower.contains("pixtral")
    {
        Some(131_072)
    } else {
        None
    }
}

pub fn auto_continue_delay() -> std::time::Duration {
    if let Ok(ms) = std::env::var("MISTRAL_AUTO_CONTINUE_DELAY_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            return std::time::Duration::from_millis(ms);
        }
    }
    std::time::Duration::from_secs(5)
}

fn is_tool_limit_continuation_needed(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("limit")
        || lower.contains("continue")
        || lower.contains("30 tool")
        || lower.contains("30 step")
        || lower.contains("reach")
}

struct Client {
    http: Arc<dyn HttpClient>,
    key: String,
    base: String,
    context_lengths: Mutex<BTreeMap<String, u64>>,
}

impl Client {
    fn new(http: Arc<dyn HttpClient>, key: String, base: String) -> Self {
        Self {
            http,
            key,
            base,
            context_lengths: Mutex::new(BTreeMap::new()),
        }
    }

    fn record_model_context_lengths(&self, data: &Value) {
        if let Some(models) = data.get("data").and_then(Value::as_array) {
            if let Ok(mut map) = self.context_lengths.lock() {
                for m in models {
                    if let Some(len) = m.get("max_context_length").and_then(Value::as_u64) {
                        if let Some(id) = m.get("id").and_then(Value::as_str) {
                            map.insert(id.to_owned(), len);
                        }
                        if let Some(aliases) = m.get("aliases").and_then(Value::as_array) {
                            for alias in aliases {
                                if let Some(a) = alias.as_str() {
                                    map.insert(a.to_owned(), len);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn context_window(&self, model: &str) -> Option<u64> {
        if let Ok(map) = self.context_lengths.lock() {
            if let Some(&len) = map.get(model) {
                return Some(len);
            }
        }
        known_context_length(model)
    }

    async fn request(
        &self,
        endpoint: &str,
        body: Option<Value>,
    ) -> Result<http_client::Response<http_client::AsyncBody>> {
        let request = Request::builder()
            .method(if body.is_some() {
                Method::POST
            } else {
                Method::GET
            })
            .uri(format!("{}/{}", self.base, endpoint))
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .body(match body {
                Some(body) => serde_json::to_vec(&body)?.into(),
                None => Vec::new().into(),
            })?;
        let mut response = self
            .http
            .send(request)
            .await
            .context("Cannot reach Mistral API")?;
        if !response.status().is_success() {
            let status = response.status();
            let mut bytes = Vec::new();
            response
                .body_mut()
                .take(8192)
                .read_to_end(&mut bytes)
                .await?;
            bail!("Mistral API {status}: {}", String::from_utf8_lossy(&bytes));
        }
        Ok(response)
    }
}

pub(crate) fn api_key(
    stored: Option<(String, Vec<u8>)>,
    environment: Option<String>,
) -> Result<String> {
    let saved = stored
        .map(|(_, bytes)| String::from_utf8(bytes))
        .transpose()
        .context("Saved Vibe API key is invalid")?;
    saved
        .or(environment)
        .filter(|key| !key.trim().is_empty())
        .map(|key| key.trim().to_owned())
        .context("Add your Mistral API key in Settings → AI → Vibe, then reconnect chat.")
}

pub async fn serve(
    http: Arc<dyn HttpClient>,
    key: String,
    conversation: SharedConversation,
    directory: PathBuf,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) -> Result<()> {
    let client = Arc::new(Client::new(
        http,
        key,
        "https://api.mistral.ai/v1".into(),
    ));
    let default_model = std::env::var("MISTRAL_MODEL").unwrap_or_else(|_| "devstral-latest".into());
    serve_with_client(
        client,
        default_model,
        conversation,
        directory,
        outgoing,
        incoming,
    )
    .await
}

async fn serve_with_client(
    client: Arc<Client>,
    default_model: String,
    conversation: SharedConversation,
    directory: PathBuf,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) -> Result<()> {
    let mut active: Option<smol::Task<()>> = None;
    let mut current_turn = String::new();
    let (approvals, approval_receiver) = channel::unbounded();
    while let Ok(message) = outgoing.recv().await {
        let id = message["id"].clone();
        let parameters = &message["params"];
        let result = match message["method"].as_str() {
            Some("initialize") => json!({}),
            Some("initialized") => continue,
            Some("config/read") => json!({"config": {"model": default_model}}),
            Some("account/read") => json!({"requiresOpenaiAuth": false}),
            Some("model/list") => {
                let models = async {
                    let mut response = client.request("models", None).await?;
                    let mut bytes = Vec::new();
                    response
                        .body_mut()
                        .take(4 * 1024 * 1024)
                        .read_to_end(&mut bytes)
                        .await?;
                    let data: Value = serde_json::from_slice(&bytes)?;
                    client.record_model_context_lengths(&data);
                    Ok::<_, anyhow::Error>(model_list(&data))
                }
                .await;
                match models {
                    Ok(models) => json!({"data": models}),
                    Err(error) => {
                        send(
                            &incoming,
                            json!({"id": id, "error": {"message": format!("{error:#}")}}),
                        )
                        .await?;
                        continue;
                    }
                }
            }
            Some("thread/start" | "thread/resume") => {
                let (items, context_tokens, context_window) = {
                    let state = conversation
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
                    (
                        state.items.clone(),
                        state.context_tokens,
                        state.context_window.or_else(|| client.context_window(&default_model)),
                    )
                };
                if let Some(tokens) = context_tokens {
                    send(
                        &incoming,
                        json!({
                            "method": "thread/tokenUsage/updated",
                            "params": {
                                "threadId": "mistral-local",
                                "tokenUsage": {
                                    "last": {
                                        "totalTokens": tokens,
                                    },
                                    "modelContextWindow": context_window,
                                }
                            }
                        }),
                    )
                    .await?;
                }
                json!({"thread": {"id": "mistral-local", "turns": [{"items": items}]}, "model": default_model})
            }
            Some("turn/start") => {
                if let Some(task) = active.take() {
                    task.cancel().await;
                }
                finish_turn(&conversation)?;
                while approval_receiver.try_recv().is_ok() {}
                let turn = {
                    let mut state = conversation
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
                    state.next_turn += 1;
                    let turn = format!("mistral-turn-{}", state.next_turn);
                    state.active_turn = Some(turn.clone());
                    turn
                };
                current_turn = turn.clone();
                send(
                    &incoming,
                    json!({"id": id, "result": {"turn": {"id": turn}}}),
                )
                .await?;
                let client = client.clone();
                let conversation = conversation.clone();
                let directory = directory.clone();
                let incoming = incoming.clone();
                let approvals = approval_receiver.clone();
                let parameters = parameters.clone();
                let model = parameters["model"]
                    .as_str()
                    .unwrap_or(&default_model)
                    .to_owned();
                active = Some(smol::spawn(async move {
                    let result = run_turn(
                        client,
                        conversation.clone(),
                        directory,
                        parameters,
                        model,
                        &turn,
                        &incoming,
                        approvals,
                    )
                    .await;
                    let result = finish_turn(&conversation).and(result);
                    let completion = match result {
                        Ok(()) => json!({"id": turn, "status": "completed"}),
                        Err(error) => {
                            json!({"id": turn, "status": "failed", "error": {"message": format!("{error:#}")}})
                        }
                    };
                    if let Err(error) = send(&incoming, json!({"method": "turn/completed", "params": {"threadId": "mistral-local", "turn": completion}})).await {
                        log::debug!("Mistral panel closed: {error}");
                    }
                }));
                continue;
            }
            Some("turn/steer") => {
                let accepted = {
                    let mut state = conversation
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
                    let text = parameters["input"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|item| item["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if parameters["threadId"] == "mistral-local"
                        && state.active_turn.as_deref() == parameters["expectedTurnId"].as_str()
                        && state.active_turn.is_some()
                        && !text.trim().is_empty()
                    {
                        state.steering.push(json!({"id": format!("{current_turn}-steer-{id}"),
                            "type": "userMessage", "content": [{"type": "inputText", "text": text}]}));
                        true
                    } else {
                        false
                    }
                };
                if !accepted {
                    send(&incoming, json!({"id": id, "error": {"message": "Mistral steering requires nonempty input and the active turn ID"}})).await?;
                    continue;
                }
                json!({"turnId": current_turn})
            }
            Some("turn/interrupt") => {
                if parameters["turnId"].as_str() != Some(current_turn.as_str()) {
                    send(&incoming, json!({"id": id, "result": {}})).await?;
                    continue;
                }
                if let Some(task) = active.take() {
                    task.cancel().await;
                }
                finish_turn(&conversation)?;
                send(&incoming, json!({"method": "turn/completed", "params": {"threadId": "mistral-local", "turn": {"id": current_turn, "status": "interrupted"}}})).await?;
                json!({})
            }
            None if message.get("result").is_some() => {
                approvals.send(message).await?;
                continue;
            }
            _ => {
                send(
                    &incoming,
                    json!({"id": id, "error": {"message": "Unsupported Mistral operation"}}),
                )
                .await?;
                continue;
            }
        };
        send(&incoming, json!({"id": id, "result": result})).await?;
    }
    if let Some(task) = active.take() {
        task.cancel().await;
    }
    finish_turn(&conversation)?;
    Ok(())
}

fn reasoning_efforts(model: &str) -> &'static [&'static str] {
    // The models endpoint does not advertise adjustable reasoning; older instruct
    // models must not receive reasoning_effort. These hybrid models accept none/high.
    match model {
        "mistral-small-latest"
        | "mistral-small-2603"
        | "mistral-medium-3-5"
        | "mistral-large-4-0" => &["none", "high"],
        _ => &[],
    }
}

fn model_list(data: &Value) -> Vec<Value> {
    data["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|model| {
            model["capabilities"]["completion_chat"] == true
                && model["capabilities"]["function_calling"] == true
        })
        .filter_map(|model| {
            let id = model["id"].as_str()?;
            let efforts = reasoning_efforts(id);
            Some(json!({
                "model": id,
                "displayName": format!("Mistral · {id}"),
                "defaultReasoningEffort": "none",
                "supportedReasoningEfforts": efforts.iter().map(|effort| {
                    json!({"reasoningEffort": effort})
                }).collect::<Vec<_>>()
            }))
        })
        .collect()
}

async fn send(incoming: &channel::Sender<Result<Value>>, message: Value) -> Result<()> {
    incoming
        .send(Ok(message))
        .await
        .context("Chat panel closed")
}

fn native_request(
    server: &mut context_mcp::Server,
    method: &str,
    parameters: Value,
) -> Result<Value> {
    let response = server
        .respond(json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": parameters}))
        .context("Native tool did not respond")?;
    if let Some(error) = response.get("error") {
        bail!("Native tool: {error}");
    }
    Ok(response["result"].clone())
}

struct Skill {
    name: String,
    description: String,
    path: PathBuf,
}

fn skills(directory: &Path) -> Result<Vec<Skill>> {
    let home = paths::home_dir();
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let mut roots = Vec::new();
    for ancestor in directory.ancestors() {
        roots.push(ancestor.join(".agents/skills"));
    }
    roots.extend([codex_home.join("skills"), home.join(".agents/skills")]);
    let mut discovered = BTreeMap::new();
    for root in roots {
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Cannot read skills in {}", root.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            let mut directories = vec![entry.path()];
            if entry.file_name() == ".system" {
                directories = std::fs::read_dir(entry.path())?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<std::io::Result<Vec<_>>>()?;
            }
            for directory in directories {
                let path = directory.join("SKILL.md");
                if !path.is_file() {
                    continue;
                }
                let text = read_bounded(&path)?.replace("\r\n", "\n");
                let Some(frontmatter) = text
                    .strip_prefix("---\n")
                    .and_then(|text| text.split_once("\n---").map(|(metadata, _)| metadata))
                else {
                    continue;
                };
                let metadata: Value = match serde_yaml::from_str(frontmatter) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        log::warn!("Invalid skill {}: {error}", path.display());
                        continue;
                    }
                };
                if let (Some(name), Some(description)) =
                    (metadata["name"].as_str(), metadata["description"].as_str())
                {
                    discovered.entry(name.to_owned()).or_insert(Skill {
                        name: name.to_owned(),
                        description: description.to_owned(),
                        path,
                    });
                }
            }
        }
    }
    Ok(discovered.into_values().collect())
}

fn read_bounded(path: &Path) -> Result<String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 128 * 1024,
        "Instruction file is too large: {}",
        path.display()
    );
    Ok(String::from_utf8(bytes)?)
}


fn find_compaction_split_point(messages: &[Value]) -> usize {
    if messages.len() <= 3 {
        return messages.len();
    }
    // Try keeping the last 2 messages
    let mut split = messages.len().saturating_sub(2).max(1);
    while split > 1 && messages[split].get("role").and_then(Value::as_str) == Some("tool") {
        split -= 1;
    }
    if split > 1 && messages[split].get("role").and_then(Value::as_str) == Some("assistant") {
        if messages[split].get("tool_calls").is_some() {
            split -= 1;
        }
    }
    if split <= 1 {
        messages.len()
    } else {
        split
    }
}

fn fallback_summary(messages: &[Value]) -> String {
    let mut parts = Vec::new();
    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("unknown");
        let content = content_text(&msg["content"]);
        if !content.is_empty() {
            let snippet = if content.len() > 200 {
                format!("{}…", &content[..200])
            } else {
                content
            };
            parts.push(format!("- {role}: {snippet}"));
        } else if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            let names = calls
                .iter()
                .filter_map(|c| c["function"]["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("- tool calls: {names}"));
        }
    }
    parts.join("\n")
}

async fn summarize_messages(
    client: &Client,
    model: &str,
    messages_to_summarize: &[Value],
) -> String {
    let mut formatted = String::new();
    for msg in messages_to_summarize {
        let role = msg["role"].as_str().unwrap_or("unknown");
        let mut content = content_text(&msg["content"]);
        if content.is_empty() {
            if let Some(text) = msg["content"].as_str() {
                content = text.to_string();
            } else if let Some(calls) = msg["tool_calls"].as_array() {
                let names = calls
                    .iter()
                    .filter_map(|c| c["function"]["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                content = format!("Called tools: {names}");
            }
        }
        if !content.is_empty() {
            let truncated = if content.len() > 2000 {
                format!("{}... [truncated]", &content[..2000])
            } else {
                content
            };
            formatted.push_str(&format!("{role}: {truncated}\n\n"));
        }
    }

    let summary_prompt = format!(
        "Summarize the following interaction history between the user and assistant concisely for a coding assistant context compaction. Retain all key user requirements, completed tasks, findings, code modifications, and current status:\n\n{formatted}"
    );

    let request = json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "You are a concise assistant summarizing past conversation history for context compaction."
            },
            {
                "role": "user",
                "content": summary_prompt
            }
        ],
        "temperature": 0.2
    });

    let api_summary = async {
        let mut response = client.request("chat/completions", Some(request)).await.ok()?;
        let mut bytes = Vec::new();
        response.body_mut().take(64 * 1024).read_to_end(&mut bytes).await.ok()?;
        let result: Value = serde_json::from_slice(&bytes).ok()?;
        let text = result["choices"][0]["message"]["content"].as_str()?.to_owned();
        Some(text)
    }
    .await;

    if let Some(summary) = api_summary.filter(|s| !s.trim().is_empty()) {
        summary
    } else {
        fallback_summary(messages_to_summarize)
    }
}

async fn compact_context(
    client: &Client,
    model: &str,
    conversation: &SharedConversation,
    messages: &mut Vec<Value>,
    items: &mut Vec<Value>,
    turn: &str,
    step: usize,
    incoming: &channel::Sender<Result<Value>>,
) -> Result<()> {
    if messages.len() <= 2 {
        return Ok(());
    }

    let compaction_id = format!("{turn}-compaction-{step}");
    send(
        incoming,
        json!({
            "method": "item/started",
            "params": {
                "threadId": "mistral-local",
                "turnId": turn,
                "item": {
                    "id": &compaction_id,
                    "type": "contextCompaction",
                }
            }
        }),
    )
    .await?;

    let split_point = find_compaction_split_point(messages);
    let to_summarize = if split_point >= messages.len() {
        &messages[1..]
    } else {
        &messages[1..split_point]
    };
    let summary = summarize_messages(client, model, to_summarize).await;

    let mut compacted = Vec::with_capacity(3 + messages.len().saturating_sub(split_point));
    compacted.push(messages[0].clone());
    compacted.push(json!({
        "role": "user",
        "content": format!("[Context Compaction Summary]:\n{summary}")
    }));
    compacted.push(json!({
        "role": "assistant",
        "content": "Understood. I have absorbed the compacted summary and will continue the task from here."
    }));
    if split_point < messages.len() {
        compacted.extend_from_slice(&messages[split_point..]);
    }
    *messages = compacted;

    let compaction_item = json!({
        "id": &compaction_id,
        "type": "contextCompaction",
        "status": "completed",
    });
    send(
        incoming,
        json!({
            "method": "item/completed",
            "params": {
                "threadId": "mistral-local",
                "turnId": turn,
                "item": &compaction_item,
            }
        }),
    )
    .await?;
    items.push(compaction_item);
    save_progress(conversation, messages, items)?;
    Ok(())
}

async fn run_turn(
    client: Arc<Client>,
    conversation: SharedConversation,
    directory: PathBuf,
    parameters: Value,
    model: String,
    turn: &str,
    incoming: &channel::Sender<Result<Value>>,
    approvals: channel::Receiver<Value>,
) -> Result<()> {
    let writable = parameters["sandboxPolicy"]["type"] != "readOnly";
    let auto_approve = parameters["sandboxPolicy"]["type"] == "dangerFullAccess";
    let effort = parameters["effort"].as_str();
    let supported_efforts = reasoning_efforts(&model);
    if let Some(effort) = effort {
        ensure!(
            supported_efforts.contains(&effort)
                || (effort == "none" && supported_efforts.is_empty()),
            "Reasoning level {effort} is not supported by {model}"
        );
    }
    let (mut server, definitions, system, skills) = smol::unblock({
        let directory = directory.clone();
        move || -> Result<_> {
            let mut server = context_mcp::Server::new(&directory, writable)?;
            let instructions = native_request(&mut server, "initialize", json!({}))?;
            let definitions = native_request(&mut server, "tools/list", json!({}))?["tools"].as_array().context("Missing native tools")?.clone();
            let skills = skills(&directory)?;
            let mut system = format!("You are a coding assistant in {}. {}\nUse read_skill before following a listed skill. Skill resources are read through read_skill_resource.\n", directory.display(), instructions["instructions"].as_str().unwrap_or_default());
            for ancestor in directory.ancestors().collect::<Vec<_>>().into_iter().rev() {
                let path = ancestor.join("AGENTS.md");
                if path.is_file() { system.push_str(&format!("\nInstructions from {}:\n{}\n", path.display(), read_bounded(&path)?)); }
            }
            for skill in &skills { system.push_str(&format!("\nSkill {}: {} ({})", skill.name, skill.description, skill.path.display())); }
            ensure!(system.len() <= 512 * 1024, "Project instructions and skill catalog are too large");
            Ok((server, definitions, system, skills))
        }
    }).await?;
    let mut tools: Vec<Value> = definitions.iter().map(|definition| json!({"type": "function", "function": {"name": definition["name"], "description": definition["description"], "parameters": definition["inputSchema"]}})).collect();
    tools.extend([
        json!({"type":"function", "function":{"name":"read_skill", "description":"Read the original shared SKILL.md by catalog name", "parameters":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}}}),
        json!({"type":"function", "function":{"name":"read_skill_resource", "description":"Read a text resource relative to a shared skill directory", "parameters":{"type":"object","properties":{"name":{"type":"string"},"path":{"type":"string"}},"required":["name","path"],"additionalProperties":false}}})
    ]);
    let mut messages = conversation
        .lock()
        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?
        .messages
        .clone();
    if messages.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    } else if let Some(first) = messages.first_mut() {
        *first = json!({"role":"system","content":system});
    }
    let prompt = parameters["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    messages.push(json!({"role": "user", "content": prompt}));
    let mut items = vec![
        json!({"id": format!("{turn}-user"), "type": "userMessage", "content": [{"type":"inputText", "text":prompt}]}),
    ];
    save_progress(&conversation, &messages, &mut items)?;
    let context_window = client.context_window(&model);
    let mut current_tokens = {
        let state = conversation
            .lock()
            .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
        state.context_tokens
    };
    if let Some(tokens) = current_tokens {
        send(
            incoming,
            json!({
                "method": "thread/tokenUsage/updated",
                "params": {
                    "threadId": "mistral-local",
                    "tokenUsage": {
                        "last": {
                            "totalTokens": tokens,
                        },
                        "modelContextWindow": context_window,
                    }
                }
            }),
        )
        .await?;
    }
    let mut tools_in_run: usize = 0;
    let mut tools_since_auto_continue: usize = 0;
    for step in 0..512 {
        if let (Some(tokens), Some(window)) = (current_tokens, context_window) {
            if window > 0 && tokens >= (window * 80) / 100 {
                compact_context(
                    &client,
                    &model,
                    &conversation,
                    &mut messages,
                    &mut items,
                    turn,
                    step,
                    incoming,
                )
                .await?;
                current_tokens = None;
            }
        }
        if apply_steering(&conversation, &mut messages, &mut items, false)? {
            save_progress(&conversation, &messages, &mut items)?;
        }
        let mut request = json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "stream": true,
            "stream_options": {"include_usage": true},
            "parallel_tool_calls": false
        });
        if let Some(effort) = effort.filter(|_| !supported_efforts.is_empty()) {
            request["reasoning_effort"] = json!(effort);
        }
        let mut response = client.request("chat/completions", Some(request)).await?;
        let message_id = format!("{turn}-message-{step}");
        let mut reader = BufReader::new(response.body_mut());
        let mut line = String::new();
        let mut text = String::new();
        let mut content_chunks = Vec::new();
        let mut has_thinking = false;
        let mut calls: BTreeMap<usize, Value> = BTreeMap::new();
        let mut finished = false;
        while reader.read_line(&mut line).await? != 0 {
            ensure!(
                line.len() <= 1024 * 1024,
                "Mistral stream event is too large"
            );
            if let Some(data) = line.trim().strip_prefix("data:").map(str::trim) {
                if data == "[DONE]" {
                    finished = true;
                    break;
                }
                let chunk: Value =
                    serde_json::from_str(data).context("Invalid Mistral stream event")?;
                if let Some(error) = chunk.get("error") {
                    bail!("Mistral stream: {error}");
                }
                if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
                    let total = usage
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .or_else(|| {
                            let prompt = usage.get("prompt_tokens").and_then(Value::as_u64)?;
                            let completion = usage
                                .get("completion_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            Some(prompt + completion)
                        });
                    if let Some(total) = total {
                        current_tokens = Some(total);
                        if let Ok(mut state) = conversation.lock() {
                            state.context_tokens = Some(total);
                            state.context_window = context_window;
                        }
                        send(
                            incoming,
                            json!({
                                "method": "thread/tokenUsage/updated",
                                "params": {
                                    "threadId": "mistral-local",
                                    "tokenUsage": {
                                        "last": {
                                            "totalTokens": total,
                                        },
                                        "modelContextWindow": context_window,
                                    }
                                }
                            }),
                        )
                        .await?;
                    }
                }
                if let Some(choice) = chunk.get("choices").and_then(Value::as_array).and_then(|c| c.first()) {
                    let delta = &choice["delta"];
                    if let Some(chunks) = delta["content"].as_array() {
                        has_thinking |= chunks.iter().any(|chunk| chunk["type"] == "thinking");
                        content_chunks.extend(chunks.iter().cloned());
                    } else if let Some(text) = delta["content"].as_str() {
                        content_chunks.push(json!({"type": "text", "text": text}));
                    }
                    let content = content_text(&delta["content"]);
                    if !content.is_empty() {
                        text.push_str(&content);
                        send(incoming, json!({"method": "item/agentMessage/delta", "params": {"threadId": "mistral-local", "turnId": turn, "itemId": message_id, "delta":content}})).await?;
                    }
                    merge_calls(&mut calls, &delta["tool_calls"])?;
                }
            }
            line.clear();
        }
        ensure!(
            finished,
            "Mistral stream ended before completion. Retry the message."
        );
        let mut assistant = json!({"role":"assistant", "content":text});
        if has_thinking {
            assistant["content"] = json!(content_chunks);
        }
        if !calls.is_empty() {
            assistant["tool_calls"] = json!(calls.values().collect::<Vec<_>>());
        }
        messages.push(assistant);
        let item = json!({"id":message_id,"type":"agentMessage","text":text});
        send(incoming,json!({"method":"item/completed","params":{"threadId":"mistral-local","turnId":turn,"item":item}})).await?;
        items.push(item);
        if calls.is_empty() {
            if apply_steering(&conversation, &mut messages, &mut items, true)? {
                save_progress(&conversation, &messages, &mut items)?;
                continue;
            }
            if tools_in_run > 0 && tools_in_run % 30 == 0 && is_tool_limit_continuation_needed(&text) {
                let delay_id = format!("{turn}-autocontinue-{step}");
                send(
                    incoming,
                    json!({
                        "method": "item/started",
                        "params": {
                            "threadId": "mistral-local",
                            "turnId": turn,
                            "item": {
                                "id": &delay_id,
                                "type": "autoContinue",
                                "text": "30 tools reached for this run. Auto-continuing after 5 seconds delay…",
                            }
                        }
                    }),
                )
                .await?;
                smol::Timer::after(auto_continue_delay()).await;
                send(
                    incoming,
                    json!({
                        "method": "item/completed",
                        "params": {
                            "threadId": "mistral-local",
                            "turnId": turn,
                            "item": {
                                "id": &delay_id,
                                "type": "autoContinue",
                                "text": "Auto-continuing run…",
                                "status": "completed",
                            }
                        }
                    }),
                )
                .await?;
                messages.push(json!({"role": "user", "content": "Continue."}));
                items.push(json!({
                    "id": format!("{turn}-autocontinue-msg-{step}"),
                    "type": "userMessage",
                    "content": [{"type": "inputText", "text": "Continue."}],
                }));
                save_progress(&conversation, &messages, &mut items)?;
                continue;
            }
            save_progress(&conversation, &messages, &mut items)?;
            return Ok(());
        }
        for call in calls.values() {
            let name = call["function"]["name"]
                .as_str()
                .context("Tool name missing")?;
            let call_id = call["id"].as_str().context("Tool ID missing")?;
            let arguments: Value =
                serde_json::from_str(call["function"]["arguments"].as_str().unwrap_or("{}"))?;
            let definition = definitions
                .iter()
                .find(|definition| definition["name"] == name);
            let skill_tool = matches!(name, "read_skill" | "read_skill_resource");
            let allowed = definition.is_some() || skill_tool;
            let requires_approval = definition
                .is_some_and(|definition| definition["annotations"]["readOnlyHint"] != true);
            let mut approved = allowed;
            if requires_approval && !auto_approve {
                let approval_id = format!("{turn}-{call_id}");
                send(incoming,json!({"id":approval_id,"method":"item/commandExecution/requestApproval","params":{"threadId":"mistral-local","turnId":turn,"command":format!("{name} {arguments}"),"cwd":directory,"reason":"Allow this native project tool?"}})).await?;
                loop {
                    let answer = approvals.recv().await?;
                    if answer["id"] == approval_id {
                        approved = matches!(
                            answer["result"]["decision"].as_str(),
                            Some("accept" | "acceptForSession")
                        );
                        break;
                    }
                }
            }
            let output = if !approved {
                json!({"isError":true,"content":[{"type":"text","text":"Tool unavailable or permission denied"}]})
            } else if skill_tool {
                let result = read_skill(&skills, name, &arguments);
                match result {
                    Ok(text) => json!({"content":[{"type":"text","text":text}]}),
                    Err(error) => {
                        json!({"isError":true,"content":[{"type":"text","text":format!("{error:#}")}]})
                    }
                }
            } else {
                let name = name.to_owned();
                let (returned_server, result) = smol::unblock(move || {
                    let result = native_request(
                        &mut server,
                        "tools/call",
                        json!({"name":name,"arguments":arguments}),
                    );
                    (server, result)
                })
                .await;
                server = returned_server;
                result?
            };
            messages.push(json!({"role":"tool","name":name,"tool_call_id":call_id,"content":serde_json::to_string(&output)?}));
            let item = json!({"id":format!("{turn}-{call_id}"),"type":"mcpToolCall","server":"zedstorm","tool":name,"status":if output["isError"] == true {"failed"} else {"completed"},"result":output});
            send(incoming,json!({"method":"item/completed","params":{"threadId":"mistral-local","turnId":turn,"item":item}})).await?;
            items.push(item);
            tools_in_run += 1;
            tools_since_auto_continue += 1;
        }
        save_progress(&conversation, &messages, &mut items)?;
        if tools_since_auto_continue >= 30 {
            tools_since_auto_continue = 0;
            let delay_id = format!("{turn}-autocontinue-{step}");
            send(
                incoming,
                json!({
                    "method": "item/started",
                    "params": {
                        "threadId": "mistral-local",
                        "turnId": turn,
                        "item": {
                            "id": &delay_id,
                            "type": "autoContinue",
                            "text": "30 tools reached for this run. Auto-continuing after 5 seconds delay…",
                        }
                    }
                }),
            )
            .await?;
            smol::Timer::after(auto_continue_delay()).await;
            send(
                incoming,
                json!({
                    "method": "item/completed",
                    "params": {
                        "threadId": "mistral-local",
                        "turnId": turn,
                        "item": {
                            "id": &delay_id,
                            "type": "autoContinue",
                            "text": "Auto-continuing run…",
                            "status": "completed",
                        }
                    }
                }),
            )
            .await?;
        }
    }
    bail!("Mistral reached the execution step limit. Send a follow-up to continue.")
}

fn save_progress(
    conversation: &SharedConversation,
    messages: &[Value],
    items: &mut Vec<Value>,
) -> Result<()> {
    let mut state = conversation
        .lock()
        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
    state.messages = messages.to_vec();
    state.items.append(items);
    Ok(())
}

fn apply_steering(
    conversation: &SharedConversation,
    messages: &mut Vec<Value>,
    items: &mut Vec<Value>,
    finishing: bool,
) -> Result<bool> {
    let mut state = conversation
        .lock()
        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
    let has_steering = !state.steering.is_empty();
    for item in state.steering.drain(..) {
        messages.push(json!({"role": "user", "content": item["content"][0]["text"]}));
        items.push(item);
    }
    if finishing && !has_steering {
        state.active_turn = None;
    }
    Ok(has_steering)
}

fn finish_turn(conversation: &SharedConversation) -> Result<()> {
    let mut state = conversation
        .lock()
        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
    let steering = std::mem::take(&mut state.steering);
    for item in steering {
        state
            .messages
            .push(json!({"role": "user", "content": item["content"][0]["text"]}));
        state.items.push(item);
    }
    state.active_turn = None;
    Ok(())
}

fn read_skill(skills: &[Skill], tool: &str, arguments: &Value) -> Result<String> {
    let skill = skills
        .iter()
        .find(|skill| arguments["name"] == skill.name)
        .context("Unknown skill")?;
    let path = if tool == "read_skill" {
        skill.path.clone()
    } else {
        let root = skill
            .path
            .parent()
            .context("Skill has no directory")?
            .canonicalize()?;
        let path = root
            .join(
                arguments["path"]
                    .as_str()
                    .context("Missing skill resource path")?,
            )
            .canonicalize()?;
        ensure!(
            path.starts_with(&root),
            "Skill resource must stay inside its skill directory"
        );
        path
    };
    read_bounded(&path)
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|chunk| chunk["type"] == "text")
        .filter_map(|chunk| chunk["text"].as_str())
        .collect()
}

fn merge_calls(calls: &mut BTreeMap<usize, Value>, fragments: &Value) -> Result<()> {
    for fragment in fragments.as_array().into_iter().flatten() {
        let index = fragment["index"]
            .as_u64()
            .context("Tool call index missing")? as usize;
        let call = calls.entry(index).or_insert_with(
            || json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
        );
        if let Some(id) = fragment["id"].as_str() {
            call["id"] = json!(id);
        }
        for field in ["name", "arguments"] {
            if let Some(part) = fragment["function"][field].as_str() {
                let mut value = call["function"][field]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                value.push_str(part);
                call["function"][field] = json!(value);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steering_reaches_the_next_request_in_the_same_turn() -> Result<()> {
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let (started, request_started) = channel::bounded(1);
            let (release, released) = channel::bounded(1);
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = count.clone();
            let http = http_client::FakeHttpClient::create(move |mut request| {
                let started = started.clone();
                let released = released.clone();
                let count = observed.clone();
                async move {
                    let mut bytes = Vec::new();
                    request.body_mut().read_to_end(&mut bytes).await?;
                    let body: Value = serde_json::from_slice(&bytes)?;
                    let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if index == 0 {
                        started.send(()).await?;
                        released.recv().await?;
                    } else {
                        assert_eq!(index, 1);
                        let messages = body["messages"].as_array().context("Messages")?;
                        assert_eq!(
                            messages.last().context("Steering input")?["content"],
                            "Focus on tests"
                        );
                        assert!(messages.iter().any(|message| message["role"] == "assistant"
                            && message["content"] == "Original response"));
                    }
                    let text = if index == 0 {
                        "Original response"
                    } else {
                        "Steered response"
                    };
                    Ok(http_client::Response::builder().body(
                        format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            json!({"choices": [{"delta": {"content": text}}]})
                        )
                        .into(),
                    )?)
                }
            });
            let conversation = SharedConversation::default();
            let (outgoing, commands) = channel::unbounded();
            let (incoming, events) = channel::unbounded();
            let server = smol::spawn(serve_with_client(
                Arc::new(Client::new(http, "test".into(), "https://api.mistral.ai/v1".into())),
                "devstral-latest".into(),
                conversation.clone(),
                directory.path().into(),
                commands,
                incoming,
            ));
            outgoing
                .send(json!({"id": 1, "method": "turn/start", "params": {
                    "input": [{"text": "Initial prompt"}], "sandboxPolicy": {"type": "readOnly"}
                }}))
                .await?;
            let turn = events.recv().await??["result"]["turn"]["id"].clone();
            request_started.recv().await?;
            outgoing.send(json!({"id": 2, "method": "turn/steer", "params": {
                "threadId": "mistral-local", "expectedTurnId": "stale", "input": [{"text": "Ignored"}]
            }})).await?;
            assert!(events.recv().await??.get("error").is_some());
            outgoing.send(json!({"id": 3, "method": "turn/steer", "params": {
                "threadId": "mistral-local", "expectedTurnId": turn, "input": [{"text": "Focus on tests"}]
            }})).await?;
            assert_eq!(events.recv().await??["result"]["turnId"], turn);
            release.send(()).await?;
            loop {
                let event = events.recv().await??;
                if event["method"] == "turn/completed" {
                    assert_eq!(event["params"]["turn"]["id"], turn);
                    assert_eq!(event["params"]["turn"]["status"], "completed");
                    break;
                }
            }
            outgoing.send(json!({"id": 4, "method": "turn/steer", "params": {
                "threadId": "mistral-local", "expectedTurnId": turn, "input": [{"text": "Too late"}]
            }})).await?;
            assert!(events.recv().await??.get("error").is_some());
            drop(outgoing);
            server.await?;
            let state = conversation
                .lock()
                .map_err(|_| anyhow::anyhow!("Lock failed"))?;
            assert!(state.items.iter().any(|item| item["type"] == "userMessage"
                && item["content"][0]["text"] == "Focus on tests"));
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
            Ok(())
        })
    }

    #[test]
    fn saved_api_key_takes_precedence_and_missing_key_points_to_settings() -> Result<()> {
        assert_eq!(
            api_key(
                Some(("mistral".into(), b" saved-key ".to_vec())),
                Some("environment-key".into())
            )?,
            "saved-key"
        );
        assert_eq!(
            api_key(None, Some("environment-key".into()))?,
            "environment-key"
        );
        let error = api_key(None, None)
            .err()
            .context("Expected missing key error")?;
        assert!(error.to_string().contains("Settings → AI → Vibe"));
        assert!(api_key(Some(("mistral".into(), vec![255])), None).is_err());
        Ok(())
    }

    #[test]
    fn streamed_tool_fragments_and_text() -> Result<()> {
        let mut calls = BTreeMap::new();
        merge_calls(
            &mut calls,
            &json!([{"index":0,"id":"call1","function":{"name":"read","arguments":"{\"pa"}}]),
        )?;
        merge_calls(
            &mut calls,
            &json!([{"index":0,"function":{"arguments":"th\":\"file\"}"}}]),
        )?;
        let call = calls.get(&0).context("Missing call")?;
        assert_eq!(call["function"]["name"], "read");
        assert_eq!(
            serde_json::from_str::<Value>(
                call["function"]["arguments"]
                    .as_str()
                    .context("Missing arguments")?
            )?,
            json!({"path":"file"})
        );
        assert_eq!(
            content_text(
                &json!([{"type":"thinking","thinking":"private"},{"type":"text","text":"Reply"}])
            ),
            "Reply"
        );
        Ok(())
    }

    #[test]
    fn model_catalog_requires_chat_and_tools() {
        let models = model_list(
            &json!({"data":[{"id":"coding","capabilities":{"completion_chat":true,"function_calling":true}},{"id":"embedding","capabilities":{"completion_chat":false}},{"id":"chat-only","capabilities":{"completion_chat":true,"function_calling":false}}]}),
        );
        assert_eq!(models.len(), 1);
        assert_eq!(
            models.first().map(|model| &model["model"]),
            Some(&json!("coding"))
        );
        assert_eq!(
            models
                .first()
                .map(|model| &model["supportedReasoningEfforts"]),
            Some(&json!([]))
        );
    }

    #[test]
    fn model_catalog_exposes_only_supported_reasoning_levels() -> Result<()> {
        for (model, efforts) in [
            ("mistral-small-latest", vec!["none", "high"]),
            ("mistral-small-2603", vec!["none", "high"]),
            ("mistral-medium-3-5", vec!["none", "high"]),
            ("mistral-large-4-0", vec!["none", "high"]),
            ("mistral-small-2506", vec![]),
            ("mistral-medium-2508", vec![]),
            ("devstral-latest", vec![]),
        ] {
            let models = model_list(&json!({"data": [{
                "id": model,
                "capabilities": {"completion_chat": true, "function_calling": true}
            }]}));
            let model = models.first().context("Missing chat model")?;
            let actual = model["supportedReasoningEfforts"]
                .as_array()
                .context("Missing reasoning levels")?
                .iter()
                .filter_map(|effort| effort["reasoningEffort"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(actual, efforts);
        }
        Ok(())
    }

    #[test]
    fn shared_skill_resources_cannot_escape() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let skill_directory = directory.path().join("skill");
        std::fs::create_dir(&skill_directory)?;
        let path = skill_directory.join("SKILL.md");
        std::fs::write(&path, "Original skill")?;
        std::fs::write(directory.path().join("outside"), "secret")?;
        let skills = vec![Skill {
            name: "shared".into(),
            description: String::new(),
            path,
        }];
        assert_eq!(
            read_skill(&skills, "read_skill", &json!({"name":"shared"}))?,
            "Original skill"
        );
        assert!(
            read_skill(
                &skills,
                "read_skill_resource",
                &json!({"name":"shared","path":"../outside"})
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn unsupported_reasoning_fails_before_sending_a_request() -> Result<()> {
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let client = Arc::new(Client::new(
                http_client::FakeHttpClient::create(|_| async {
                    bail!("Unexpected HTTP request")
                }),
                "test-key".into(),
                "https://api.mistral.ai/v1".into(),
            ));
            let (incoming, _receiver) = channel::unbounded();
            let (_approvals, approval_receiver) = channel::unbounded();
            for (model, effort) in [
                ("devstral-latest", "high"),
                ("mistral-small-latest", "low"),
                ("mistral-medium-3-5", "medium"),
            ] {
                let result = run_turn(
                    client.clone(),
                    SharedConversation::default(),
                    directory.path().into(),
                    json!({"effort": effort}),
                    model.into(),
                    "unsupported",
                    &incoming,
                    approval_receiver.clone(),
                )
                .await;
                let error = result.err().context("Unsupported reasoning must fail")?;
                assert_eq!(
                    error.to_string(),
                    format!("Reasoning level {effort} is not supported by {model}")
                );
            }
            Ok(())
        })
    }

    #[test]
    fn api_tool_loop_preserves_followup_history() -> Result<()> {
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            std::fs::write(directory.path().join("hello.txt"), "hello")?;
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = count.clone();
            let http = http_client::FakeHttpClient::create(move |mut request| {
                let count = observed.clone();
                async move {
                    assert_eq!(request.uri().path(), "/v1/chat/completions");
                    assert_eq!(
                        request
                            .headers()
                            .get("authorization")
                            .context("Missing authorization")?,
                        "Bearer test-key"
                    );
                    let mut bytes = Vec::new();
                    request.body_mut().read_to_end(&mut bytes).await?;
                    let body: Value = serde_json::from_slice(&bytes)?;
                    let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    match index {
                        0 | 1 => assert_eq!(body["reasoning_effort"], "high"),
                        2 => assert_eq!(body["reasoning_effort"], "none"),
                        _ => assert!(body.get("reasoning_effort").is_none()),
                    }
                    let delta = if index == 0 {
                        json!({
                            "content": [{"type": "thinking", "thinking": [{"type": "text", "text": "Read the file first."}]}],
                            "tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":"read","arguments":"{\"path\":\"hello.txt\"}"}}]
                        })
                    } else {
                        let messages = body["messages"].as_array().context("Missing messages")?;
                        assert!(messages.iter().any(|message| {
                            message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("hello"))
                        }));
                        assert!(messages.iter().any(|message| {
                            message["role"] == "assistant"
                                && message["content"][0]["type"] == "thinking"
                                && message["content"][0]["thinking"][0]["text"]
                                    == "Read the file first."
                                && content_text(&message["content"]) == "Reading."
                        }));
                        json!({"content":"Done"})
                    };
                    let mut stream = format!("data: {}\n\n", json!({"choices":[{"delta":delta}]}));
                    if index == 0 {
                        stream.push_str(&format!(
                            "data: {}\n\n",
                            json!({"choices": [{"delta": {"content": "Reading."}}]})
                        ));
                    }
                    stream.push_str("data: [DONE]\n\n");
                    Ok(http_client::Response::builder().body(stream.into())?)
                }
            });
            let client = Arc::new(Client::new(http, "test-key".into(), "https://api.mistral.ai/v1".into()));
            let conversation = SharedConversation::default();
            let (incoming, receiver) = channel::unbounded();
            let (_approvals, approval_receiver) = channel::unbounded();
            for (turn, effort) in [
                ("first", Some("high")),
                ("followup", Some("none")),
                ("default", None),
            ] {
                let mut parameters =
                    json!({"input":[{"text":"Read hello"}],"sandboxPolicy":{"type":"readOnly"}});
                if let Some(effort) = effort {
                    parameters["effort"] = json!(effort);
                }
                run_turn(
                    client.clone(),
                    conversation.clone(),
                    directory.path().into(),
                    parameters,
                    "mistral-small-latest".into(),
                    turn,
                    &incoming,
                    approval_receiver.clone(),
                )
                .await?;
            }
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 4);
            assert!(!receiver.is_empty());
            let state = conversation
                .lock()
                .map_err(|_| anyhow::anyhow!("Lock failed"))?;
            let tool = state
                .items
                .iter()
                .find(|item| item["type"] == "mcpToolCall")
                .context("Saved tool call")?;
            assert!(
                tool["result"]["content"]
                    .as_array()
                    .context("Tool result content")?
                    .iter()
                    .any(|part| part["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("hello")))
            );
            drop(state);
            assert_eq!(
                conversation
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Lock failed"))?
                    .messages
                    .iter()
                    .filter(|message| message["role"] == "user")
                    .count(),
                3
            );
            Ok(())
        })
    }
    #[test]
    fn project_writes_obey_access_and_approval() -> Result<()> {
        smol::block_on(async {
            for (mode, decision, should_write) in [
                ("readOnly", None, false),
                ("workspaceWrite", Some("decline"), false),
                ("workspaceWrite", Some("accept"), true),
                ("dangerFullAccess", None, true),
            ] {
                let directory = tempfile::tempdir()?;
                let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let http = http_client::FakeHttpClient::create(move |_| {
                    let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async move {
                        let delta = if index == 0 {
                            json!({"tool_calls":[{"index":0,"id":"mkdir-call","function":{"name":"mkdir","arguments":"{\"path\":\"created\"}"}}]})
                        } else {
                            json!({"content":"Finished"})
                        };
                        Ok(http_client::Response::builder().body(
                            format!(
                                "data: {}\n\ndata: [DONE]\n\n",
                                json!({"choices":[{"delta":delta}]})
                            )
                            .into(),
                        )?)
                    }
                });
                let client = Arc::new(Client::new(http, "test".into(), "https://api.mistral.ai/v1".into()));
                let (incoming, receiver) = channel::unbounded();
                let (approvals, approval_receiver) = channel::unbounded();
                let runner = run_turn(
                    client,
                    SharedConversation::default(),
                    directory.path().into(),
                    json!({"input":[{"text":"Create directory"}],"sandboxPolicy":{"type":mode}}),
                    "devstral-latest".into(),
                    "turn",
                    &incoming,
                    approval_receiver,
                );
                if let Some(decision) = decision {
                    let responder = async {
                        while let Ok(message) = receiver.recv().await {
                            let message = message?;
                            if message["method"] == "item/commandExecution/requestApproval" {
                                approvals
                                    .send(
                                        json!({"id":message["id"],"result":{"decision":decision}}),
                                    )
                                    .await?;
                                return Ok::<_, anyhow::Error>(());
                            }
                        }
                        bail!("Approval was not requested")
                    };
                    let (result, approval) = futures::join!(runner, responder);
                    result?;
                    approval?;
                } else {
                    runner.await?;
                }
                assert_eq!(
                    directory.path().join("created").exists(),
                    should_write,
                    "mode {mode}, decision {decision:?}"
                );
            }
            Ok(())
        })
    }

    #[test]
    fn api_authentication_errors_are_visible() -> Result<()> {
        smol::block_on(async {
            let http = http_client::FakeHttpClient::create(|_| async {
                Ok(http_client::Response::builder()
                    .status(401)
                    .body("Invalid API key".into())?)
            });
            let client = Client::new(http, "test".into(), "https://api.mistral.ai/v1".into());
            let error = client
                .request("models", None)
                .await
                .err()
                .context("Expected authentication failure")?;
            assert!(error.to_string().contains("401"));
            assert!(error.to_string().contains("Invalid API key"));
            Ok(())
        })
    }
    #[test]
    fn stop_cancels_pending_api_and_keeps_session_usable() -> Result<()> {
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let (started, request_started) = channel::bounded(1);
            let http = http_client::FakeHttpClient::create(move |_| {
                let started = started.clone();
                async move {
                    started.send(()).await?;
                    futures::future::pending::<Result<http_client::Response<http_client::AsyncBody>>>().await
                }
            });
            let client = Arc::new(Client::new(http, "test".into(), "https://api.mistral.ai/v1".into()));
            let conversation = SharedConversation::default();
            let (outgoing, commands) = channel::unbounded();
            let (incoming, events) = channel::unbounded();
            let server = smol::spawn(serve_with_client(
                client,
                "devstral-latest".into(),
                conversation.clone(),
                directory.path().into(),
                commands,
                incoming,
            ));
            outgoing.send(json!({"id":1,"method":"initialize"})).await?;
            assert_eq!(events.recv().await??["id"], 1);
            outgoing
                .send(json!({"id":2,"method":"turn/start","params":{"input":[{"text":"test"}]}}))
                .await?;
            let response = events.recv().await??;
            let turn_id = response["result"]["turn"]["id"].clone();
            request_started.recv().await?;
            outgoing
                .send(json!({"id":3,"method":"turn/interrupt","params":{"turnId":turn_id}}))
                .await?;
            let stopped = events.recv().await??;
            assert_eq!(stopped["params"]["turn"]["status"], "interrupted");
            assert_eq!(stopped["params"]["turn"]["id"], turn_id);
            assert_eq!(events.recv().await??["id"], 3);
            outgoing
                .send(json!({"id":4,"method":"thread/resume"}))
                .await?;
            let resumed = events.recv().await??;
            assert_eq!(resumed["result"]["thread"]["id"], "mistral-local");
            assert!(
                !conversation
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Lock failed"))?
                    .messages
                    .is_empty()
            );
            drop(outgoing);
            server.await?;
            Ok(())
        })
    }

    #[test]
    fn known_model_context_length_and_dynamic_model_caching() {
        assert_eq!(known_context_length("devstral-latest"), Some(262_144));
        assert_eq!(known_context_length("mistral-small-latest"), Some(131_072));
        assert_eq!(known_context_length("mistral-large-latest"), Some(262_144));
        assert_eq!(known_context_length("mistral-large-2411"), Some(262_144));
        assert_eq!(known_context_length("mistral-large-4-0"), Some(1_048_576));
        assert_eq!(known_context_length("codestral-2405"), Some(32_768));
        assert_eq!(known_context_length("unknown-custom-model"), None);

        let client = Client::new(
            http_client::FakeHttpClient::create(|_| async { bail!("Not used") }),
            "key".into(),
            "https://api.mistral.ai/v1".into(),
        );
        assert_eq!(client.context_window("devstral-latest"), Some(262_144));
        assert_eq!(client.context_window("custom-model"), None);

        client.record_model_context_lengths(&json!({
            "data": [
                {
                    "id": "custom-model",
                    "max_context_length": 123456,
                    "aliases": ["custom-alias"]
                }
            ]
        }));
        assert_eq!(client.context_window("custom-model"), Some(123456));
        assert_eq!(client.context_window("custom-alias"), Some(123456));
    }

    #[test]
    fn tokens_used_are_tracked_and_sent_to_session() -> Result<()> {
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let http = http_client::FakeHttpClient::create(|request| async move {
                if request.uri().path() == "/v1/chat/completions" {
                    let mut bytes = Vec::new();
                    request.into_body().read_to_end(&mut bytes).await?;
                    let body: Value = serde_json::from_slice(&bytes)?;
                    assert_eq!(body["stream_options"]["include_usage"], true);
                    Ok(http_client::Response::builder().body(
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            json!({"choices": [{"delta": {"content": "Hello world"}}]}),
                            json!({
                                "choices": [],
                                "usage": {
                                    "prompt_tokens": 120,
                                    "completion_tokens": 80,
                                    "total_tokens": 200
                                }
                            })
                        )
                        .into(),
                    )?)
                } else {
                    bail!("Unexpected request: {}", request.uri())
                }
            });
            let conversation = SharedConversation::default();
            let (outgoing, commands) = channel::unbounded();
            let (incoming, events) = channel::unbounded();
            let server = smol::spawn(serve_with_client(
                Arc::new(Client::new(http, "test".into(), "https://api.mistral.ai/v1".into())),
                "devstral-latest".into(),
                conversation.clone(),
                directory.path().into(),
                commands,
                incoming,
            ));
            outgoing.send(json!({"id": 1, "method": "initialize"})).await?;
            assert_eq!(events.recv().await??["id"], 1);

            outgoing.send(json!({
                "id": 2,
                "method": "turn/start",
                "params": {"input": [{"text": "Hello"}]}
            })).await?;

            let mut received_usage_update = false;
            let mut turn_completed = false;
            while let Ok(msg) = events.recv().await {
                let msg = msg?;
                if msg.get("method").and_then(Value::as_str) == Some("thread/tokenUsage/updated") {
                    let tokens = msg["params"]["tokenUsage"]["last"]["totalTokens"].as_u64();
                    let window = msg["params"]["tokenUsage"]["modelContextWindow"].as_u64();
                    assert_eq!(tokens, Some(200));
                    assert_eq!(window, Some(262_144));
                    received_usage_update = true;
                } else if msg.get("method").and_then(Value::as_str) == Some("turn/completed") {
                    turn_completed = true;
                    break;
                }
            }
            assert!(received_usage_update, "Token usage update notification was not received");
            assert!(turn_completed, "Turn did not complete");

            let state = conversation.lock().unwrap();
            assert_eq!(state.context_tokens, Some(200));
            assert_eq!(state.context_window, Some(262_144));
            drop(state);

            outgoing.send(json!({"id": 3, "method": "thread/resume"})).await?;
            let resume_token_update = events.recv().await??;
            assert_eq!(resume_token_update["method"], "thread/tokenUsage/updated");
            assert_eq!(resume_token_update["params"]["tokenUsage"]["last"]["totalTokens"], 200);
            assert_eq!(resume_token_update["params"]["tokenUsage"]["modelContextWindow"], 262_144);

            let resume_res = events.recv().await??;
            assert_eq!(resume_res["id"], 3);

            drop(outgoing);
            server.await?;
            Ok(())
        })
    }

    #[test]
    fn auto_compacts_context_at_80_percent_of_known_model_context_length() -> Result<()> {
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let http = http_client::FakeHttpClient::create(|request| async move {
                if request.uri().path() == "/v1/chat/completions" {
                    let mut bytes = Vec::new();
                    request.into_body().read_to_end(&mut bytes).await?;
                    let body: Value = serde_json::from_slice(&bytes)?;
                    if body["messages"].as_array().map_or(false, |msgs| {
                        msgs.iter().any(|m| m["role"] == "system" && m["content"].as_str().unwrap_or("").contains("summarizing past conversation"))
                    }) {
                        return Ok(http_client::Response::builder().body(
                            json!({
                                "choices": [{"message": {"role": "assistant", "content": "Summary of previous tasks"}}]
                            }).to_string().into()
                        )?);
                    }
                    Ok(http_client::Response::builder().body(
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            json!({"choices": [{"delta": {"content": "Continuing after compaction"}}]}),
                            json!({
                                "choices": [],
                                "usage": {
                                    "prompt_tokens": 1000,
                                    "completion_tokens": 50,
                                    "total_tokens": 1050
                                }
                            })
                        )
                        .into(),
                    )?)
                } else {
                    bail!("Unexpected request: {}", request.uri())
                }
            });

            let conversation = SharedConversation::default();
            {
                let mut state = conversation.lock().unwrap();
                state.context_tokens = Some(27_000);
                state.context_window = Some(32_768);
                state.messages = vec![
                    json!({"role": "system", "content": "You are a coding assistant"}),
                    json!({"role": "user", "content": "Long past task 1"}),
                    json!({"role": "assistant", "content": "Completed task 1"}),
                ];
            }

            let (outgoing, commands) = channel::unbounded();
            let (incoming, events) = channel::unbounded();
            let server = smol::spawn(serve_with_client(
                Arc::new(Client::new(http, "test".into(), "https://api.mistral.ai/v1".into())),
                "codestral-2405".into(),
                conversation.clone(),
                directory.path().into(),
                commands,
                incoming,
            ));
            outgoing.send(json!({"id": 1, "method": "initialize"})).await?;
            assert_eq!(events.recv().await??["id"], 1);

            outgoing.send(json!({
                "id": 2,
                "method": "turn/start",
                "params": {"input": [{"text": "Next prompt"}]}
            })).await?;

            let mut saw_compaction_started = false;
            let mut saw_compaction_completed = false;
            while let Ok(msg) = events.recv().await {
                let msg = msg?;
                if let Some(item) = msg.pointer("/params/item") {
                    if item["type"] == "contextCompaction" {
                        if msg["method"] == "item/started" {
                            saw_compaction_started = true;
                        } else if msg["method"] == "item/completed" {
                            saw_compaction_completed = true;
                        }
                    }
                }
                if msg.get("method").and_then(Value::as_str) == Some("turn/completed") {
                    break;
                }
            }
            assert!(saw_compaction_started, "contextCompaction item was not started");
            assert!(saw_compaction_completed, "contextCompaction item was not completed");

            let state = conversation.lock().unwrap();
            let summary_msg = state.messages.iter().find(|m| {
                m["content"].as_str().map_or(false, |c| c.contains("[Context Compaction Summary]"))
            });
            assert!(summary_msg.is_some(), "Compacted summary message not found in conversation messages");
            drop(state);

            drop(outgoing);
            server.await?;
            Ok(())
        })
    }

    #[test]
    fn auto_continues_after_30_tools_with_delay() -> Result<()> {
        unsafe { std::env::set_var("MISTRAL_AUTO_CONTINUE_DELAY_MS", "10"); }
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let call_count_clone = call_count.clone();
            let http = http_client::FakeHttpClient::create(move |request| {
                let count = call_count_clone.clone();
                async move {
                    if request.uri().path() == "/v1/chat/completions" {
                        let step = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if step < 30 {
                            let tool_call = json!([{
                                "index": 0,
                                "id": format!("call-{step}"),
                                "type": "function",
                                "function": {
                                    "name": "read_skill",
                                    "arguments": json!({"name": "nonexistent"}).to_string()
                                }
                            }]);
                            Ok(http_client::Response::builder().body(
                                format!(
                                    "data: {}\n\ndata: [DONE]\n\n",
                                    json!({"choices": [{"delta": {"tool_calls": tool_call}}]})
                                )
                                .into(),
                            )?)
                        } else {
                            Ok(http_client::Response::builder().body(
                                format!(
                                    "data: {}\n\ndata: [DONE]\n\n",
                                    json!({"choices": [{"delta": {"content": "Finished all steps!"}}]})
                                )
                                .into(),
                            )?)
                        }
                    } else {
                        bail!("Unexpected request: {}", request.uri())
                    }
                }
            });

            let conversation = SharedConversation::default();
            let (outgoing, commands) = channel::unbounded();
            let (incoming, events) = channel::unbounded();
            let server = smol::spawn(serve_with_client(
                Arc::new(Client::new(http, "test".into(), "https://api.mistral.ai/v1".into())),
                "devstral-latest".into(),
                conversation.clone(),
                directory.path().into(),
                commands,
                incoming,
            ));
            outgoing.send(json!({"id": 1, "method": "initialize"})).await?;
            assert_eq!(events.recv().await??["id"], 1);

            outgoing.send(json!({
                "id": 2,
                "method": "turn/start",
                "params": {"input": [{"text": "Run tools"}]}
            })).await?;

            let mut saw_auto_continue_started = false;
            let mut saw_auto_continue_completed = false;
            while let Ok(msg) = events.recv().await {
                let msg = msg?;
                if let Some(item) = msg.pointer("/params/item") {
                    if item["type"] == "autoContinue" {
                        if msg["method"] == "item/started" {
                            saw_auto_continue_started = true;
                        } else if msg["method"] == "item/completed" {
                            saw_auto_continue_completed = true;
                        }
                    }
                }
                if msg.get("method").and_then(Value::as_str) == Some("turn/completed") {
                    break;
                }
            }
            assert!(saw_auto_continue_started, "autoContinue item was not started");
            assert!(saw_auto_continue_completed, "autoContinue item was not completed");
            assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 31);

            drop(outgoing);
            server.await?;
            Ok(())
        })
    }

    #[test]
    fn auto_continues_when_model_stops_at_30_tools() -> Result<()> {
        unsafe { std::env::set_var("MISTRAL_AUTO_CONTINUE_DELAY_MS", "10"); }
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let call_count_clone = call_count.clone();
            let http = http_client::FakeHttpClient::create(move |request| {
                let count = call_count_clone.clone();
                async move {
                    if request.uri().path() == "/v1/chat/completions" {
                        let step = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if step < 30 {
                            let tool_call = json!([{
                                "index": 0,
                                "id": format!("call-{step}"),
                                "type": "function",
                                "function": {
                                    "name": "read_skill",
                                    "arguments": json!({"name": "nonexistent"}).to_string()
                                }
                            }]);
                            Ok(http_client::Response::builder().body(
                                format!(
                                    "data: {}\n\ndata: [DONE]\n\n",
                                    json!({"choices": [{"delta": {"tool_calls": tool_call}}]})
                                )
                                .into(),
                            )?)
                        } else if step == 30 {
                            Ok(http_client::Response::builder().body(
                                format!(
                                    "data: {}\n\ndata: [DONE]\n\n",
                                    json!({"choices": [{"delta": {"content": "I reached the 30 tool limit per run. Please continue."}}]})
                                )
                                .into(),
                            )?)
                        } else {
                            Ok(http_client::Response::builder().body(
                                format!(
                                    "data: {}\n\ndata: [DONE]\n\n",
                                    json!({"choices": [{"delta": {"content": "Now all done!"}}]})
                                )
                                .into(),
                            )?)
                        }
                    } else {
                        bail!("Unexpected request: {}", request.uri())
                    }
                }
            });

            let conversation = SharedConversation::default();
            let (outgoing, commands) = channel::unbounded();
            let (incoming, events) = channel::unbounded();
            let server = smol::spawn(serve_with_client(
                Arc::new(Client::new(http, "test".into(), "https://api.mistral.ai/v1".into())),
                "devstral-latest".into(),
                conversation.clone(),
                directory.path().into(),
                commands,
                incoming,
            ));
            outgoing.send(json!({"id": 1, "method": "initialize"})).await?;
            assert_eq!(events.recv().await??["id"], 1);

            outgoing.send(json!({
                "id": 2,
                "method": "turn/start",
                "params": {"input": [{"text": "Run 30 tools"}]}
            })).await?;

            let mut auto_continue_count = 0;
            while let Ok(msg) = events.recv().await {
                let msg = msg?;
                if msg.get("method").and_then(Value::as_str) == Some("item/completed") {
                    if msg.pointer("/params/item/type") == Some(&json!("autoContinue")) {
                        auto_continue_count += 1;
                    }
                }
                if msg.get("method").and_then(Value::as_str) == Some("turn/completed") {
                    break;
                }
            }
            assert!(auto_continue_count >= 1, "Expected autoContinue notification");
            assert!(call_count.load(std::sync::atomic::Ordering::SeqCst) >= 31);

            drop(outgoing);
            server.await?;
            Ok(())
        })
    }
}
