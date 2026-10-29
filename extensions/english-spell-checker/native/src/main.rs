mod checker;
mod dictionary;
mod lsp;
mod tokenizer;

use std::io::{self, BufRead, BufReader, Read, Write};
use checker::SpellChecker;
use lsp::*;
use serde::Serialize;

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    let mut checker = SpellChecker::new();

    loop {
        // Read headers (e.g. Content-Length: 123\r\n\r\n)
        let mut content_length: Option<usize> = None;
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            if reader.read_line(&mut line_buf)? == 0 {
                // EOF reached
                return Ok(());
            }

            let trimmed = line_buf.trim();
            if trimmed.is_empty() {
                // Header section ended
                break;
            }

            if let Some(val) = trimmed.strip_prefix("Content-Length:") {
                if let Ok(len) = val.trim().parse::<usize>() {
                    content_length = Some(len);
                }
            }
        }

        let Some(length) = content_length else {
            continue;
        };

        // Read payload of length bytes
        let mut payload = vec![0u8; length];
        reader.read_exact(&mut payload)?;

        let Ok(req) = serde_json::from_slice::<RpcRequest>(&payload) else {
            continue;
        };

        handle_request(&req, &mut checker, &mut writer)?;
    }
}

fn handle_request<W: Write>(
    req: &RpcRequest,
    checker: &mut SpellChecker,
    writer: &mut W,
) -> io::Result<()> {
    match req.method.as_str() {
        "initialize" => {
            if let Some(id) = &req.id {
                let res = InitializeResult {
                    capabilities: ServerCapabilities {
                        text_document_sync: 1, // Full document sync
                        code_action_provider: true,
                    },
                    server_info: ServerInfo {
                        name: "english-spell-checker".to_string(),
                        version: "0.1.0".to_string(),
                    },
                };
                send_response(id.clone(), res, writer)?;
            }
        }
        "initialized" => {
            // Client acknowledgment, nothing to return
        }
        "textDocument/didOpen" => {
            if let Some(params) = &req.params {
                if let Some(td) = params.get("textDocument") {
                    let uri = td.get("uri").and_then(|u| u.as_str()).unwrap_or_default();
                    let text = td.get("text").and_then(|t| t.as_str()).unwrap_or_default();

                    let diagnostics = checker.check_document(uri, text);
                    send_notification(
                        "textDocument/publishDiagnostics",
                        PublishDiagnosticsParams {
                            uri: uri.to_string(),
                            diagnostics,
                        },
                        writer,
                    )?;
                }
            }
        }
        "textDocument/didChange" => {
            if let Some(params) = &req.params {
                if let Some(td) = params.get("textDocument") {
                    let uri = td.get("uri").and_then(|u| u.as_str()).unwrap_or_default();
                    if let Some(changes) = params.get("contentChanges").and_then(|c| c.as_array()) {
                        if let Some(last_change) = changes.last() {
                            if let Some(text) = last_change.get("text").and_then(|t| t.as_str()) {
                                let diagnostics = checker.check_document(uri, text);
                                send_notification(
                                    "textDocument/publishDiagnostics",
                                    PublishDiagnosticsParams {
                                        uri: uri.to_string(),
                                        diagnostics,
                                    },
                                    writer,
                                )?;
                            }
                        }
                    }
                }
            }
        }
        "textDocument/didClose" => {
            if let Some(params) = &req.params {
                if let Some(td) = params.get("textDocument") {
                    let uri = td.get("uri").and_then(|u| u.as_str()).unwrap_or_default();
                    checker.close_document(uri);
                    // Clear diagnostics
                    send_notification(
                        "textDocument/publishDiagnostics",
                        PublishDiagnosticsParams {
                            uri: uri.to_string(),
                            diagnostics: Vec::new(),
                        },
                        writer,
                    )?;
                }
            }
        }
        "textDocument/codeAction" => {
            if let Some(id) = &req.id {
                let mut actions = Vec::new();
                if let Some(params) = &req.params {
                    let uri = params.get("textDocument")
                        .and_then(|t| t.get("uri"))
                        .and_then(|u| u.as_str())
                        .unwrap_or_default();

                    if let Some(range_val) = params.get("range") {
                        if let Ok(range) = serde_json::from_value::<Range>(range_val.clone()) {
                            actions = checker.get_code_actions(uri, &range);
                        }
                    }
                }
                send_response(id.clone(), actions, writer)?;
            }
        }
        "shutdown" => {
            if let Some(id) = &req.id {
                send_response(id.clone(), serde_json::Value::Null, writer)?;
            }
        }
        "exit" => {
            std::process::exit(0);
        }
        _ => {
            // Unhandled request: return method not found if an ID was provided
            if let Some(id) = &req.id {
                send_response(id.clone(), serde_json::Value::Null, writer)?;
            }
        }
    }

    Ok(())
}

fn send_response<T: Serialize, W: Write>(
    id: serde_json::Value,
    result: T,
    writer: &mut W,
) -> io::Result<()> {
    let resp = RpcResponse {
        jsonrpc: "2.0",
        id,
        result,
    };
    let json_bytes = serde_json::to_vec(&resp)?;
    write!(writer, "Content-Length: {}\r\n\r\n", json_bytes.len())?;
    writer.write_all(&json_bytes)?;
    writer.flush()
}

fn send_notification<T: Serialize, W: Write>(
    method: &'static str,
    params: T,
    writer: &mut W,
) -> io::Result<()> {
    let notif = RpcNotification {
        jsonrpc: "2.0",
        method,
        params,
    };
    let json_bytes = serde_json::to_vec(&notif)?;
    write!(writer, "Content-Length: {}\r\n\r\n", json_bytes.len())?;
    writer.write_all(&json_bytes)?;
    writer.flush()
}
