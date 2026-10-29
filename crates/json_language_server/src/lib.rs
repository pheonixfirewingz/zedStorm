pub mod checker;
pub mod lsp;

use checker::JsonChecker;
use lsp::*;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};

pub fn run_server() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    let checker = JsonChecker::new();
    let mut documents: HashMap<String, String> = HashMap::new();

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

        let Some(len) = content_length else {
            continue;
        };

        let mut body_bytes = vec![0u8; len];
        reader.read_exact(&mut body_bytes)?;

        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body_bytes) else {
            continue;
        };

        let method = value.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = value.get("id").cloned();

        match method {
            "initialize" => {
                let result = InitializeResult {
                    capabilities: ServerCapabilities {
                        text_document_sync: 1, // Full sync
                        code_action_provider: true,
                        document_formatting_provider: true,
                    },
                    server_info: ServerInfo {
                        name: "json-language-server".to_string(),
                        version: "0.1.0".to_string(),
                    },
                };
                send_response(&mut writer, id, Ok(result))?;
            }
            "initialized" => {}
            "textDocument/didOpen" => {
                if let Some(params) = value.get("params") {
                    if let Ok(open_params) =
                        serde_json::from_value::<DidOpenTextDocumentParams>(params.clone())
                    {
                        let uri = open_params.text_document.uri;
                        let text = open_params.text_document.text;
                        let diagnostics = checker.check(&text);
                        documents.insert(uri.clone(), text);

                        send_notification(
                            &mut writer,
                            "textDocument/publishDiagnostics",
                            PublishDiagnosticsParams { uri, diagnostics },
                        )?;
                    }
                }
            }
            "textDocument/didChange" => {
                if let Some(params) = value.get("params") {
                    if let Ok(change_params) =
                        serde_json::from_value::<DidChangeTextDocumentParams>(params.clone())
                    {
                        if let Some(change) = change_params.content_changes.into_iter().last() {
                            let uri = change_params.text_document.uri;
                            let text = change.text;
                            let diagnostics = checker.check(&text);
                            documents.insert(uri.clone(), text);

                            send_notification(
                                &mut writer,
                                "textDocument/publishDiagnostics",
                                PublishDiagnosticsParams { uri, diagnostics },
                            )?;
                        }
                    }
                }
            }
            "textDocument/didClose" => {
                if let Some(params) = value.get("params") {
                    if let Some(uri) = params
                        .get("textDocument")
                        .and_then(|td| td.get("uri"))
                        .and_then(|u| u.as_str())
                    {
                        documents.remove(uri);
                        send_notification(
                            &mut writer,
                            "textDocument/publishDiagnostics",
                            PublishDiagnosticsParams {
                                uri: uri.to_string(),
                                diagnostics: Vec::new(),
                            },
                        )?;
                    }
                }
            }
            "textDocument/codeAction" => {
                send_response::<Vec<CodeAction>>(&mut writer, id, Ok(Vec::new()))?;
            }
            "textDocument/formatting" => {
                if let Some(params) = value.get("params") {
                    if let Ok(fmt_params) =
                        serde_json::from_value::<DocumentFormattingParams>(params.clone())
                    {
                        let uri = fmt_params.text_document.uri;
                        let mut edits = Vec::new();
                        if let Some(text) = documents.get(&uri) {
                            if let Some(formatted) = checker.format(text) {
                                let line_count = text.lines().count() as u32;
                                edits.push(TextEdit {
                                    range: Range {
                                        start: Position {
                                            line: 0,
                                            character: 0,
                                        },
                                        end: Position {
                                            line: line_count.max(1),
                                            character: 0,
                                        },
                                    },
                                    new_text: formatted,
                                });
                            }
                        }
                        send_response(&mut writer, id, Ok(edits))?;
                    } else {
                        send_response::<Vec<TextEdit>>(&mut writer, id, Ok(Vec::new()))?;
                    }
                } else {
                    send_response::<Vec<TextEdit>>(&mut writer, id, Ok(Vec::new()))?;
                }
            }
            "shutdown" => {
                send_response(&mut writer, id, Ok(()))?;
            }
            "exit" => {
                return Ok(());
            }
            _ => {
                if id.is_some() {
                    send_response(&mut writer, id, Ok(serde_json::Value::Null))?;
                }
            }
        }
    }
}

fn send_response<T: Serialize>(
    writer: &mut impl Write,
    id: Option<serde_json::Value>,
    result: Result<T, serde_json::Value>,
) -> io::Result<()> {
    let mut resp = serde_json::Map::new();
    resp.insert("jsonrpc".to_string(), serde_json::json!("2.0"));
    if let Some(id_val) = id {
        resp.insert("id".to_string(), id_val);
    }
    match result {
        Ok(res) => {
            resp.insert(
                "result".to_string(),
                serde_json::to_value(res).unwrap_or(serde_json::Value::Null),
            );
        }
        Err(err) => {
            resp.insert("error".to_string(), err);
        }
    }

    let body = serde_json::to_string(&resp).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn send_notification<T: Serialize>(
    writer: &mut impl Write,
    method: &str,
    params: T,
) -> io::Result<()> {
    let mut notif = serde_json::Map::new();
    notif.insert("jsonrpc".to_string(), serde_json::json!("2.0"));
    notif.insert("method".to_string(), serde_json::json!(method));
    notif.insert(
        "params".to_string(),
        serde_json::to_value(params).unwrap_or(serde_json::Value::Null),
    );

    let body = serde_json::to_string(&notif).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()?;
    Ok(())
}
