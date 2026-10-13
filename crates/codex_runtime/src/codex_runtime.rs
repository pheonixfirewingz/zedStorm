mod session;

pub use session::{
    AccessMode, MAX_ACTIVITY_TEXT_LEN, Message, MessageKind, Model, ReasoningEffort, ServerRequest,
    Session, SessionSettings,
};

use anyhow::{Context as _, Result, bail};
use codex_app_server::in_process::{
    self, InProcessClientHandle, InProcessClientSender, InProcessServerEvent, InProcessStartArgs,
};
use codex_app_server_protocol::{
    ClientNotification, ClientRequest, ConfigWarningNotification, InitializeParams, RequestId,
};
use codex_arg0::{Arg0DispatchPaths, Arg0PathEntryGuard};
use codex_core::config::{Config, ConfigBuilder, ConfigOverrides};
use codex_exec_server::{EnvironmentManager, ExecServerRuntimeOptions};
use futures::{StreamExt as _, channel::oneshot, future::BoxFuture, stream::FuturesUnordered};
use serde_json::{Value, json};
use smol::channel;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

static HELPER_PATHS: OnceLock<Arg0DispatchPaths> = OnceLock::new();

pub fn dispatch_helpers() -> Option<Arg0PathEntryGuard> {
    // Helper aliases and their PATH entry must be prepared before the editor starts threads.
    let guard = codex_arg0::arg0_dispatch();
    if let Some(guard) = &guard {
        HELPER_PATHS.get_or_init(|| guard.paths().clone());
    }
    guard
}

pub async fn serve(
    directory: PathBuf,
    configuration: Vec<(String, Value)>,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
) -> Result<()> {
    let (completion_sender, completion) = oneshot::channel();
    let (cancel_on_drop, cancellation) = oneshot::channel::<()>();
    // A separate runtime lets cancellation stop all core tasks, including startup work.
    std::thread::Builder::new()
        .name("codex-core".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_stack_size(16 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .context("Could not create Codex runtime")?;
                let result = runtime.block_on(serve_embedded(
                    directory,
                    configuration,
                    outgoing,
                    incoming,
                    cancellation,
                ));
                runtime.shutdown_timeout(std::time::Duration::from_secs(1));
                result
            })();
            if completion_sender.send(result).is_err() {
                log::debug!("Codex connection was closed before shutdown completed");
            }
        })
        .context("Could not start Codex runtime thread")?;
    let result = completion
        .await
        .context("Codex runtime stopped unexpectedly")?;
    drop(cancel_on_drop);
    result
}

async fn serve_embedded(
    directory: PathBuf,
    configuration: Vec<(String, Value)>,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
    mut cancellation: oneshot::Receiver<()>,
) -> Result<()> {
    let startup = async {
        let message = outgoing.recv().await.context("Codex connection closed")?;
        let ClientRequest::Initialize { request_id, params } = serde_json::from_value(message)?
        else {
            bail!("Codex must be initialized before sending requests");
        };
        let overrides = configuration
            .into_iter()
            .map(|(key, value)| Ok((key, toml::Value::try_from(value)?)))
            .collect::<Result<Vec<_>>>()?;
        let embedded_network_policy =
            in_process::EmbeddedNetworkPolicy::load(&Default::default()).await;
        let mut config = ConfigBuilder::default()
            .cli_overrides(overrides.clone())
            .harness_overrides(ConfigOverrides {
                cwd: Some(directory),
                ..Default::default()
            })
            .build()
            .await
            .context("Could not load Codex configuration")?;
        embedded_network_policy.bind_config(&mut config);
        let arg0_paths = HELPER_PATHS
            .get()
            .cloned()
            .context("Codex helpers were not initialized at application startup")?;
        let client = start_client(
            config,
            overrides,
            arg0_paths,
            params,
            embedded_network_policy,
        )
        .await?;
        Ok::<_, anyhow::Error>((client, request_id))
    };
    let (client, initialize_id) = tokio::select! {
        result = startup => result?,
        _ = &mut cancellation => return Ok(()),
    };
    bridge(client, initialize_id, outgoing, incoming, cancellation).await
}

async fn start_client(
    config: Config,
    overrides: Vec<(String, toml::Value)>,
    arg0_paths: Arg0DispatchPaths,
    params: InitializeParams,
    embedded_network_policy: in_process::EmbeddedNetworkPolicy,
) -> Result<InProcessClientHandle> {
    let runtime_paths = ExecServerRuntimeOptions::from_optional_paths(
        arg0_paths.codex_self_exe.clone(),
        arg0_paths.codex_linux_sandbox_exe.clone(),
    )?;
    let environment_manager = Arc::new(
        EnvironmentManager::from_codex_home(
            &config.codex_home,
            Some(runtime_paths),
            config.http_client_factory(),
        )
        .await
        .context("Could not load Codex execution environments")?,
    );
    let state_db = codex_core::init_state_db(&config).await;
    let config_warnings = config
        .startup_warnings
        .iter()
        .map(|warning| ConfigWarningNotification {
            summary: warning.clone(),
            details: None,
            path: None,
            range: None,
        })
        .collect();
    let client = in_process::start(InProcessStartArgs {
        arg0_paths,
        config: Arc::new(config),
        cli_overrides: overrides,
        loader_overrides: Default::default(),
        strict_config: false,
        cloud_config_bundle: Default::default(),
        embedded_network_policy,
        thread_config_loader: Arc::new(codex_config::NoopThreadConfigLoader),
        feedback: codex_feedback::CodexFeedback::new(),
        log_db: None,
        state_db,
        environment_manager,
        config_warnings,
        session_source: codex_protocol::protocol::SessionSource::Custom("zedstorm".into()),
        enable_codex_api_key_env: false,
        initialize: params,
        channel_capacity: in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await
    .context("Could not initialize embedded Codex")?;
    Ok(client)
}

async fn bridge(
    mut client: InProcessClientHandle,
    initialize_id: RequestId,
    outgoing: channel::Receiver<Value>,
    incoming: channel::Sender<Result<Value>>,
    mut cancellation: oneshot::Receiver<()>,
) -> Result<()> {
    let sender = client.sender();
    let connection = async {
        incoming
            .send(Ok(json!({"id": initialize_id, "result": {}})))
            .await?;
        let mut requests: FuturesUnordered<BoxFuture<'static, Result<Value>>> =
            FuturesUnordered::new();
        loop {
            tokio::select! {
                _ = &mut cancellation => return Ok(()),
                message = outgoing.recv() => {
                    let Ok(message) = message else { return Ok(()); };
                    forward_message(message, &sender, &incoming, &mut requests).await?;
                }
                response = requests.next(), if !requests.is_empty() => {
                    if let Some(response) = response {
                        incoming.send(Ok(response?)).await?;
                    }
                }
                event = client.next_event() => {
                    let message = match event {
                        Some(InProcessServerEvent::ServerRequest(request)) => serde_json::to_value(request)?,
                        Some(InProcessServerEvent::ServerNotification(notification)) => serde_json::to_value(notification)?,
                        Some(InProcessServerEvent::Lagged { skipped }) => bail!("Codex dropped {skipped} events. Reconnect to reload this chat."),
                        None => bail!("Codex disconnected. Reconnect to continue this chat."),
                    };
                    incoming.send(Ok(message)).await?;
                }
            }
        }
    };
    let result = connection.await;
    let shutdown = client
        .shutdown()
        .await
        .context("Could not shut down embedded Codex");
    if result.is_err() {
        if let Err(error) = shutdown {
            log::error!("{error:#}");
        }
        result
    } else {
        shutdown
    }
}

async fn forward_message(
    message: Value,
    sender: &InProcessClientSender,
    incoming: &channel::Sender<Result<Value>>,
    requests: &mut FuturesUnordered<BoxFuture<'static, Result<Value>>>,
) -> Result<()> {
    if message.get("method").and_then(Value::as_str) == Some("initialized") {
        // The embedded runtime performs this handshake before returning its handle.
        return Ok(());
    }
    if message.get("method").is_some() && message.get("id").is_some() {
        match serde_json::from_value::<ClientRequest>(message.clone()) {
            Ok(request) => {
                let sender = sender.clone();
                requests.push(Box::pin(async move {
                    let request_id = request.id().clone();
                    let response = sender.request(request).await?;
                    Ok(match response {
                        Ok(result) => json!({"id": request_id, "result": result}),
                        Err(error) => json!({"id": request_id, "error": error}),
                    })
                }));
            }
            Err(error) => {
                incoming
                    .send(Ok(json!({
                        "id": message.get("id"),
                        "error": {"code": -32600, "message": error.to_string()},
                    })))
                    .await?
            }
        }
    } else if message.get("method").is_some() {
        sender.notify(serde_json::from_value::<ClientNotification>(message)?)?;
    } else {
        let request_id = serde_json::from_value(
            message
                .get("id")
                .cloned()
                .context("Missing Codex request ID")?,
        )?;
        if let Some(error) = message.get("error") {
            sender.fail_server_request(request_id, serde_json::from_value(error.clone())?)?;
        } else {
            sender.respond_to_server_request(
                request_id,
                message
                    .get("result")
                    .cloned()
                    .context("Missing Codex approval response")?,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_bridge_routes_requests_and_shuts_down_on_cancellation() -> Result<()> {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_stack_size(16 * 1024 * 1024)
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    tokio::time::timeout(std::time::Duration::from_secs(30), async {
                        let home = tempfile::tempdir()?;
                        let project = tempfile::tempdir()?;
                        let overrides =
                            vec![("model".into(), toml::Value::String("test-model".into()))];
                        let config = ConfigBuilder::default()
                            .codex_home(home.path().to_path_buf())
                            .cli_overrides(overrides.clone())
                            .harness_overrides(ConfigOverrides {
                                cwd: Some(project.path().to_path_buf()),
                                ..Default::default()
                            })
                            .build()
                            .await?;
                        let initialize = serde_json::from_value(json!({
                            "clientInfo": {"name": "zedstorm-test", "version": "0.0.0"}
                        }))?;
                        let client = Box::pin(start_client(
                            config,
                            overrides,
                            Arg0DispatchPaths {
                                codex_self_exe: Some(std::env::current_exe()?),
                                ..Default::default()
                            },
                            initialize,
                            Default::default(),
                        ))
                        .await?;
                        let (outgoing, requests) = channel::unbounded();
                        let (events, incoming) = channel::unbounded();
                        let (cancel_on_drop, cancellation) = oneshot::channel();
                        let task = tokio::spawn(bridge(
                            client,
                            RequestId::Integer(1),
                            requests,
                            events,
                            cancellation,
                        ));
                        assert_eq!(incoming.recv().await??, json!({"id": 1, "result": {}}));
                        outgoing.send(json!({"method": "initialized"})).await?;
                        outgoing
                            .send(json!({"id": 2, "method": "unknown/method", "params": {}}))
                            .await?;
                        outgoing
                .send(json!({"id": 3, "method": "config/read", "params": {"includeLayers": false}}))
                .await?;
                        let mut invalid_request = false;
                        let mut config_read = false;
                        while !invalid_request || !config_read {
                            let message = incoming.recv().await??;
                            match message.get("id").and_then(Value::as_u64) {
                                Some(2) => {
                                    assert_eq!(
                                        message.pointer("/error/code"),
                                        Some(&json!(-32600))
                                    );
                                    invalid_request = true;
                                }
                                Some(3) => {
                                    assert_eq!(
                                        message.pointer("/result/config/model"),
                                        Some(&json!("test-model"))
                                    );
                                    config_read = true;
                                }
                                _ => {}
                            }
                        }
                        drop(cancel_on_drop);
                        task.await??;
                        Ok::<_, anyhow::Error>(())
                    })
                    .await
                    .context(
                        "Embedded Codex did not finish requests and shutdown within 30 seconds",
                    )?
                })
            })?
            .join()
            .map_err(|_| anyhow::anyhow!("Embedded Codex test thread panicked"))?
    }
}
