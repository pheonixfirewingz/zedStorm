use anyhow::{Context as _, Result, ensure};
use async_tungstenite::tungstenite::{self, Message, WebSocket};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io,
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const DEFAULT_BROWSER_PORT: u16 = 42124;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchArguments {
    pub query: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

fn default_max_results() -> usize {
    5
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchItem {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
}

struct SearchCommand {
    id: String,
    query: String,
    max_results: usize,
    response_tx: Sender<Result<Vec<SearchItem>>>,
}

pub struct BrowserBridge {
    port: u16,
    connected: Arc<AtomicBool>,
    command_tx: Sender<SearchCommand>,
    shutdown: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl BrowserBridge {
    pub fn new(port: u16) -> Self {
        let connected = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (command_tx, command_rx) = mpsc::channel();

        let connected_clone = connected.clone();
        let shutdown_clone = shutdown.clone();

        let thread_handle = thread::Builder::new()
            .name("zedstorm-browser-bridge".into())
            .spawn(move || {
                run_bridge_worker(port, connected_clone, shutdown_clone, command_rx);
            })
            .ok();

        Self {
            port,
            connected,
            command_tx,
            shutdown,
            thread_handle,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn set_connected_for_test(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    pub fn search(&self, arguments: SearchArguments) -> Result<String> {
        ensure!(
            self.is_connected(),
            "Chromium extension is not connected; web_search is unavailable"
        );
        ensure!(
            !arguments.query.trim().is_empty(),
            "Search query cannot be empty"
        );

        let id = uuid::Uuid::new_v4().to_string();
        let (response_tx, response_rx) = mpsc::channel();

        self.command_tx
            .send(SearchCommand {
                id,
                query: arguments.query.clone(),
                max_results: arguments.max_results.clamp(1, 15),
                response_tx,
            })
            .context("Browser bridge worker thread is not running")?;

        let results = response_rx
            .recv_timeout(Duration::from_secs(20))
            .context("Timed out waiting for search results from browser extension")??;

        if results.is_empty() {
            return Ok(format!("No results found for {:?}.", arguments.query));
        }

        let mut output = format!("### Web Search Results for {:?}\n\n", arguments.query);
        for (index, item) in results.iter().enumerate() {
            output.push_str(&format!("{}. [{}]({})\n", index + 1, item.title, item.url));
            if !item.snippet.trim().is_empty() {
                output.push_str(&format!("   {}\n\n", item.snippet.trim()));
            } else {
                output.push('\n');
            }
        }

        Ok(output)
    }
}

impl Drop for BrowserBridge {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_bridge_worker(
    port: u16,
    connected: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    command_rx: Receiver<SearchCommand>,
) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(_) => {
            return;
        }
    };
    let _ = listener.set_nonblocking(true);

    while !shutdown.load(Ordering::SeqCst) {
        // Wait for an incoming WebSocket connection
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };

        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

        let mut ws = match tungstenite::accept(stream) {
            Ok(socket) => socket,
            Err(_) => {
                continue;
            }
        };

        connected.store(true, Ordering::SeqCst);

        // Handle this active connection until it disconnects or shutdown
        handle_connection(&mut ws, &connected, &shutdown, &command_rx);

        connected.store(false, Ordering::SeqCst);
    }
}

fn handle_connection(
    ws: &mut WebSocket<TcpStream>,
    _connected: &Arc<AtomicBool>,
    shutdown: &Arc<AtomicBool>,
    command_rx: &Receiver<SearchCommand>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        // 1. Check if there is a pending search command
        if let Ok(cmd) = command_rx.try_recv() {
            let req = json!({
                "id": cmd.id,
                "action": "search",
                "query": cmd.query,
                "max_results": cmd.max_results
            });

            if let Err(e) = ws.send(Message::Text(req.to_string().into())) {
                let _ = cmd.response_tx.send(Err(anyhow::anyhow!("Failed to send search request to browser: {e}")));
                break;
            }

            // Wait for response matching this ID
            let deadline = Instant::now() + Duration::from_secs(18);
            let mut got_response = false;

            while Instant::now() < deadline && !shutdown.load(Ordering::SeqCst) {
                match ws.read() {
                    Ok(Message::Text(text)) => {
                        if let Ok(resp) = serde_json::from_str::<Value>(text.as_str()) {
                            if resp.get("id").and_then(|id| id.as_str()) == Some(&cmd.id) {
                                if let Some(error) = resp.get("error").and_then(|e| e.as_str()) {
                                    let _ = cmd.response_tx.send(Err(anyhow::anyhow!("{error}")));
                                } else if let Some(results_val) = resp.get("results") {
                                    match serde_json::from_value::<Vec<SearchItem>>(results_val.clone()) {
                                        Ok(items) => {
                                            let _ = cmd.response_tx.send(Ok(items));
                                        }
                                        Err(e) => {
                                            let _ = cmd.response_tx.send(Err(anyhow::anyhow!("Invalid results format: {e}")));
                                        }
                                    }
                                } else {
                                    let _ = cmd.response_tx.send(Ok(vec![]));
                                }
                                got_response = true;
                                break;
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        let _ = ws.send(Message::Pong(payload));
                    }
                    Ok(Message::Close(_)) => {
                        let _ = cmd.response_tx.send(Err(anyhow::anyhow!("Browser extension closed connection during search")));
                        return;
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::TimedOut =>
                    {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => {
                        let _ = cmd.response_tx.send(Err(anyhow::anyhow!("WebSocket error during search: {e}")));
                        return;
                    }
                }
            }

            if !got_response && !shutdown.load(Ordering::SeqCst) {
                let _ = cmd.response_tx.send(Err(anyhow::anyhow!("Timed out waiting for browser extension search response")));
            }
            continue;
        }

        // 2. Read incoming messages / ping from browser extension
        match ws.read() {
            Ok(Message::Ping(payload)) => {
                let _ = ws.send(Message::Pong(payload));
            }
            Ok(Message::Close(_)) => {
                break;
            }
            Ok(Message::Text(text)) => {
                if let Ok(msg) = serde_json::from_str::<Value>(text.as_str()) {
                    if msg.get("type").and_then(|t| t.as_str()) == Some("ping") {
                        let _ = ws.send(Message::Text(json!({"type": "pong"}).to_string().into()));
                    }
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                // Connection broken
                break;
            }
        }
    }
}
