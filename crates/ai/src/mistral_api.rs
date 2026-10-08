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
}

struct Client {
    http: Arc<dyn HttpClient>,
    key: String,
    base: String,
}

impl Client {
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
    let client = Arc::new(Client {
        http,
        key,
        base: "https://api.mistral.ai/v1".into(),
    });
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
                let items = conversation
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?
                    .items
                    .clone();
                json!({"thread": {"id": "mistral-local", "turns": [{"items": items}]}, "model": default_model})
            }
            Some("turn/start") => {
                if let Some(task) = active.take() {
                    task.cancel().await;
                }
                while approval_receiver.try_recv().is_ok() {}
                let turn = {
                    let mut state = conversation
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?;
                    state.next_turn += 1;
                    format!("mistral-turn-{}", state.next_turn)
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
                        conversation,
                        directory,
                        parameters,
                        model,
                        &turn,
                        &incoming,
                        approvals,
                    )
                    .await;
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
            Some("turn/interrupt") => {
                if parameters["turnId"].as_str() != Some(current_turn.as_str()) {
                    send(&incoming, json!({"id": id, "result": {}})).await?;
                    continue;
                }
                if let Some(task) = active.take() {
                    task.cancel().await;
                }
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
    Ok(())
}

fn model_list(data: &Value) -> Vec<Value> {
    data["data"].as_array().into_iter().flatten().filter(|model| {
        model["capabilities"]["completion_chat"] == true && model["capabilities"]["function_calling"] == true
    }).filter_map(|model| {
        let id = model["id"].as_str()?;
        Some(json!({"model": id, "displayName": format!("Mistral · {id}"), "defaultReasoningEffort": "none", "supportedReasoningEfforts": []}))
    }).collect()
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
    for step in 0..32 {
        let mut response = client.request("chat/completions", Some(json!({"model": model, "messages": messages, "tools": tools, "stream": true, "parallel_tool_calls": false}))).await?;
        let message_id = format!("{turn}-message-{step}");
        let mut reader = BufReader::new(response.body_mut());
        let mut line = String::new();
        let mut text = String::new();
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
                let delta = &chunk["choices"][0]["delta"];
                let content = content_text(&delta["content"]);
                if !content.is_empty() {
                    text.push_str(&content);
                    send(incoming, json!({"method": "item/agentMessage/delta", "params": {"threadId": "mistral-local", "turnId": turn, "itemId": message_id, "delta":content}})).await?;
                }
                merge_calls(&mut calls, &delta["tool_calls"])?;
            }
            line.clear();
        }
        ensure!(
            finished,
            "Mistral stream ended before completion. Retry the message."
        );
        let mut assistant = json!({"role":"assistant", "content":text});
        if !calls.is_empty() {
            assistant["tool_calls"] = json!(calls.values().collect::<Vec<_>>());
        }
        messages.push(assistant);
        let item = json!({"id":message_id,"type":"agentMessage","text":text});
        send(incoming,json!({"method":"item/completed","params":{"threadId":"mistral-local","turnId":turn,"item":item}})).await?;
        items.push(item);
        if calls.is_empty() {
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
            let item = json!({"id":format!("{turn}-{call_id}"),"type":"mcpToolCall","server":"zedstorm","tool":name,"status":if output["isError"] == true {"failed"} else {"completed"}});
            send(incoming,json!({"method":"item/completed","params":{"threadId":"mistral-local","turnId":turn,"item":item}})).await?;
            items.push(item);
        }
        save_progress(&conversation, &messages, &mut items)?;
    }
    bail!("Mistral reached the 32-step tool limit. Send a follow-up to continue.")
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
                    let delta = if index == 0 {
                        json!({"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":"read","arguments":"{\"path\":\"hello.txt\"}"}}]})
                    } else {
                        let messages = body["messages"].as_array().context("Missing messages")?;
                        assert!(messages.iter().any(|message| {
                            message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("hello"))
                        }));
                        json!({"content":"Done"})
                    };
                    let stream = format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"delta":delta}]})
                    );
                    Ok(http_client::Response::builder().body(stream.into())?)
                }
            });
            let client = Arc::new(Client {
                http,
                key: "test-key".into(),
                base: "https://api.mistral.ai/v1".into(),
            });
            let conversation = SharedConversation::default();
            let (incoming, receiver) = channel::unbounded();
            let (_approvals, approval_receiver) = channel::unbounded();
            for turn in ["first", "followup"] {
                run_turn(
                    client.clone(),
                    conversation.clone(),
                    directory.path().into(),
                    json!({"input":[{"text":"Read hello"}],"sandboxPolicy":{"type":"readOnly"}}),
                    "devstral-latest".into(),
                    turn,
                    &incoming,
                    approval_receiver.clone(),
                )
                .await?;
            }
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
            assert!(!receiver.is_empty());
            assert_eq!(
                conversation
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Lock failed"))?
                    .messages
                    .iter()
                    .filter(|message| message["role"] == "user")
                    .count(),
                2
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
                let client = Arc::new(Client {
                    http,
                    key: "test".into(),
                    base: "https://api.mistral.ai/v1".into(),
                });
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
            let client = Client {
                http,
                key: "test".into(),
                base: "https://api.mistral.ai/v1".into(),
            };
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
            let client = Arc::new(Client {
                http,
                key: "test".into(),
                base: "https://api.mistral.ai/v1".into(),
            });
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
}
