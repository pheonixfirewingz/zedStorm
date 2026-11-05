#![cfg_attr(windows, windows_subsystem = "windows")]

mod checker;
mod dictionary;
mod lsp;
mod storage;
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
    let bin_dict = storage::binary_dictionary_path();
    for w in storage::load_words_from_file(&bin_dict) {
        checker.add_custom_word(&w);
    }
    if let Some(user_dict) = storage::user_dictionary_path() {
        for w in storage::load_words_from_file(&user_dict) {
            checker.add_custom_word(&w);
        }
    }

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
                if let Some(params) = &req.params {
                    load_words_from_init_options(params, checker);
                }
                let res = InitializeResult {
                    capabilities: ServerCapabilities {
                        text_document_sync: 1, // Full document sync
                        code_action_provider: true,
                        execute_command_provider: Some(ExecuteCommandOptions {
                            commands: vec![
                                "english-spell-checker.saveToDictionary".to_string(),
                                "english-spell-checker.saveToProject".to_string(),
                                "english-spell-checker.ignoreWord".to_string(),
                                "english-spell-checker.openDictionary".to_string(),
                                "english-spell-checker.addToUserDictionary".to_string(),
                                "english-spell-checker.addToWorkspaceDictionary".to_string(),
                                "english-spell-checker.addWord".to_string(),
                            ],
                        }),
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

                    if let Some(ws_dict) = storage::workspace_dictionary_path(uri) {
                        for w in storage::load_words_from_file(&ws_dict) {
                            checker.add_custom_word(&w);
                        }
                    }

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
        "workspace/executeCommand" => {
            if let Some(params) = &req.params {
                let command = params.get("command").and_then(|c| c.as_str()).unwrap_or_default();
                let args = params.get("arguments").and_then(|a| a.as_array());

                match command {
                    "english-spell-checker.saveToDictionary"
                    | "english-spell-checker.addToUserDictionary"
                    | "english-spell-checker.addWord" => {
                        if let Some(word) = args.and_then(|a| a.first()).and_then(|w| w.as_str()) {
                            let bin_dict = storage::binary_dictionary_path();
                            let _ = storage::append_word_to_file(&bin_dict, word);
                            checker.add_custom_word(word);
                        }
                    }
                    "english-spell-checker.saveToProject"
                    | "english-spell-checker.addToWorkspaceDictionary" => {
                        if let Some(word) = args.and_then(|a| a.first()).and_then(|w| w.as_str()) {
                            let uri = args.and_then(|a| a.get(1)).and_then(|u| u.as_str()).unwrap_or_default();
                            if let Some(ws_dict) = storage::workspace_dictionary_path(uri) {
                                let _ = storage::append_word_to_file(&ws_dict, word);
                            }
                            checker.add_custom_word(word);
                        }
                    }
                    "english-spell-checker.ignoreWord" => {
                        if let Some(word) = args.and_then(|a| a.first()).and_then(|w| w.as_str()) {
                            checker.ignore_word(word);
                        }
                    }
                    "english-spell-checker.openDictionary" => {
                        let bin_dict = storage::binary_dictionary_path();
                        if !bin_dict.exists() {
                            let _ = storage::append_word_to_file(
                                &bin_dict,
                                "# English Spell Checker Dictionary - Add custom words (one per line)",
                            );
                        }
                        let uri = storage::path_to_uri(&bin_dict);
                        let _ = send_show_document_request(&uri, writer);
                    }
                    _ => {}
                }

                if let Some(id) = &req.id {
                    send_response(id.clone(), serde_json::Value::Null, writer)?;
                }

                // Re-check all open documents and publish updated diagnostics
                for (doc_uri, diagnostics) in checker.recheck_all_documents() {
                    send_notification(
                        "textDocument/publishDiagnostics",
                        PublishDiagnosticsParams {
                            uri: doc_uri,
                            diagnostics,
                        },
                        writer,
                    )?;
                }
            }
        }
        "workspace/didChangeConfiguration" => {
            if let Some(params) = &req.params {
                load_words_from_init_options(params, checker);
                for (doc_uri, diagnostics) in checker.recheck_all_documents() {
                    send_notification(
                        "textDocument/publishDiagnostics",
                        PublishDiagnosticsParams {
                            uri: doc_uri,
                            diagnostics,
                        },
                        writer,
                    )?;
                }
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

fn load_words_from_init_options(params: &serde_json::Value, checker: &mut SpellChecker) {
    let options = params.get("initializationOptions").or_else(|| params.get("settings"));
    let Some(opts) = options else { return; };

    let words_array = opts.get("custom_words")
        .or_else(|| opts.get("customWords"))
        .or_else(|| opts.get("words"))
        .and_then(|v| v.as_array());

    if let Some(words) = words_array {
        for w in words {
            if let Some(word_str) = w.as_str() {
                checker.add_custom_word(word_str);
            }
        }
    }

    if let Some(dict_path) = opts.get("dictionary").and_then(|v| v.as_str()) {
        for w in storage::load_words_from_file(std::path::Path::new(dict_path)) {
            checker.add_custom_word(&w);
        }
    }
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

fn send_show_document_request<W: Write>(uri: &str, writer: &mut W) -> io::Result<()> {
    static REQ_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1000);
    let id = REQ_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "window/showDocument",
        "params": {
            "uri": uri,
            "takeFocus": true
        }
    });
    let json_bytes = serde_json::to_vec(&req)?;
    write!(writer, "Content-Length: {}\r\n\r\n", json_bytes.len())?;
    writer.write_all(&json_bytes)?;
    writer.flush()
}
