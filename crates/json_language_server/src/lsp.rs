#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: u32, // 1 = Error, 2 = Warning, 3 = Information, 4 = Hint
    pub source: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PublishDiagnosticsParams {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceEdit {
    pub changes: Option<HashMap<String, Vec<TextEdit>>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CodeAction {
    pub title: String,
    pub kind: String,
    pub edit: Option<WorkspaceEdit>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(rename = "textDocumentSync")]
    pub text_document_sync: u32,
    #[serde(rename = "codeActionProvider")]
    pub code_action_provider: bool,
    #[serde(rename = "documentFormattingProvider")]
    pub document_formatting_provider: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextDocumentItem {
    pub uri: String,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DidOpenTextDocumentParams {
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentItem,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextDocumentContentChangeEvent {
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DidChangeTextDocumentParams {
    #[serde(rename = "textDocument")]
    pub text_document: VersionedTextDocumentIdentifier,
    #[serde(rename = "contentChanges")]
    pub content_changes: Vec<TextDocumentContentChangeEvent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionedTextDocumentIdentifier {
    pub uri: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CodeActionContext {
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CodeActionParams {
    #[serde(rename = "textDocument")]
    pub text_document: VersionedTextDocumentIdentifier,
    pub range: Range,
    pub context: CodeActionContext,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentFormattingParams {
    #[serde(rename = "textDocument")]
    pub text_document: VersionedTextDocumentIdentifier,
}
