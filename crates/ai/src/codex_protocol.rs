use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use smol::channel;
use std::{collections::BTreeMap, path::PathBuf};

pub use codex_runtime::{
    AccessMode, Message, MessageKind, ServerRequest, Session, SessionSettings,
};

pub async fn run_server(
    backend: super::Backend,
    http_client: std::sync::Arc<dyn http_client::HttpClient>,
    conversation: super::mistral_api::SharedConversation,
    directory: PathBuf,
    credentials: Option<gpui::Task<Result<Option<(String, Vec<u8>)>>>>,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) {
    let result = match backend {
        super::Backend::Codex => serve(directory, http_client, outgoing, incoming.clone()).await,
        super::Backend::Mistral => {
            let key = async {
                let stored = match credentials {
                    Some(credentials) => {
                        credentials.await.context("Could not read Vibe API key")?
                    }
                    None => None,
                };
                super::mistral_api::api_key(stored, std::env::var("MISTRAL_API_KEY").ok())
            }
            .await;
            match key {
                Ok(key) => {
                    super::mistral_api::serve(
                        http_client,
                        key,
                        conversation,
                        directory,
                        outgoing,
                        incoming.clone(),
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        }
    };
    if let Err(error) = result {
        if let Err(error) = incoming.send(Err(error)).await {
            log::debug!("Codex panel closed: {error}");
        }
    }
}

async fn serve(
    directory: PathBuf,
    http_client: std::sync::Arc<dyn http_client::HttpClient>,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) -> Result<()> {
    let executable =
        std::env::current_exe().context("Cannot locate ZedStorm's built-in MCP server")?;
    let writable = std::env::var("ZEDSTORM_CONTEXT_MCP_WRITE").is_ok_and(|value| value == "1");
    let configuration = context_mcp::codex_configuration(&executable, &directory, writable)?;
    serve_with_configuration(directory, http_client, configuration, outgoing, incoming).await
}

pub async fn serve_commit_message(
    directory: PathBuf,
    http_client: std::sync::Arc<dyn http_client::HttpClient>,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) -> Result<()> {
    let configuration = vec![
        ("model".into(), json!("gpt-6-luna")),
        ("model_reasoning_effort".into(), json!("low")),
        ("mcp_servers".into(), json!({})),
        ("project_doc_max_bytes".into(), json!(0)),
        ("features.shell_tool".into(), json!(false)),
        (
            "developer_instructions".into(),
            json!("Write only the requested commit message. Do not use tools."),
        ),
    ];
    serve_with_configuration(directory, http_client, configuration, outgoing, incoming).await
}

async fn serve_with_configuration(
    directory: PathBuf,
    http_client: std::sync::Arc<dyn http_client::HttpClient>,
    mut configuration: Vec<(String, Value)>,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) -> Result<()> {
    use http_client::managed_tools::{self, Tool};
    let git = managed_tools::ensure_tool(Tool::Git, http_client).await?;
    let mut environment = BTreeMap::new();
    environment.insert(
        "PATH",
        managed_tools::path_with_tools(std::env::var_os("PATH").as_deref())?
            .into_string()
            .map_err(|_| anyhow::anyhow!("Codex requires a UTF-8 command search path"))?,
    );
    if git == managed_tools::bundled_path(Tool::Git)? {
        let root = managed_tools::tools_directory()?.join("git");
        if cfg!(not(windows)) {
            environment.insert(
                "GIT_EXEC_PATH",
                root.join("libexec/git-core").to_string_lossy().into_owned(),
            );
            environment.insert(
                "GIT_TEMPLATE_DIR",
                root.join("share/git-core/templates")
                    .to_string_lossy()
                    .into_owned(),
            );
            let certificates = root.join("ssl/cacert.pem");
            if certificates.is_file() {
                environment.insert(
                    "GIT_SSL_CAINFO",
                    certificates.to_string_lossy().into_owned(),
                );
            }
        }
    }
    for (key, value) in environment {
        configuration.push((format!("shell_environment_policy.set.{key}"), json!(value)));
    }
    configuration.extend([
        ("service_tier".into(), json!("default")),
        ("features.fast_mode".into(), json!(false)),
    ]);
    codex_runtime::serve(directory, configuration, outgoing, incoming).await
}
