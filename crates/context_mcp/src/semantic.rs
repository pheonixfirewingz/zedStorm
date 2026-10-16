use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead as _, Read as _, Write as _},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{DEFAULT_OUTPUT_BYTES, MAX_OUTPUT_BYTES, MAX_REQUEST_BYTES, revision, tool};

pub const TOOLS: &[&str] = &[
    "semantic_status",
    "read_buffer",
    "symbols",
    "workspace_symbols",
    "definitions",
    "type_definitions",
    "declarations",
    "references",
    "implementations",
    "hover",
    "incoming_calls",
    "outgoing_calls",
    "rename",
    "open_file",
    "open_tab",
];

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name) || name == "open_buffer"
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Arguments {
    pub path: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub query: Option<String>,
    pub new_name: Option<String>,
    pub revision: Option<String>,
    #[serde(default)]
    pub offset: usize,
    pub max_bytes: Option<usize>,
}

impl Arguments {
    pub fn validate(&self, name: &str) -> Result<()> {
        ensure!(is_tool(name), "Unknown semantic tool");
        ensure!(
            (256..=MAX_OUTPUT_BYTES).contains(&self.budget()),
            "Invalid output budget"
        );
        if !matches!(name, "semantic_status" | "workspace_symbols") {
            ensure!(
                self.path.as_ref().is_some_and(|path| !path.is_empty()),
                "Missing path"
            );
        }
        if !matches!(
            name,
            "semantic_status"
                | "workspace_symbols"
                | "symbols"
                | "read_buffer"
                | "open_file"
                | "open_tab"
                | "open_buffer"
        ) {
            ensure!(
                self.line.is_some_and(|line| line > 0),
                "Missing or invalid one-based line"
            );
            ensure!(self.column.is_some(), "Missing zero-based UTF-16 column");
        }
        if name == "workspace_symbols" {
            ensure!(
                self.query
                    .as_ref()
                    .is_some_and(|query| !query.trim().is_empty()),
                "Missing symbol query"
            );
        }
        if name == "rename" {
            ensure!(
                self.new_name
                    .as_ref()
                    .is_some_and(|name| !name.trim().is_empty()),
                "Missing new_name"
            );
            ensure!(
                self.revision.is_some(),
                "Rename requires the revision from read_buffer"
            );
            ensure!(self.offset == 0, "Rename cannot be paginated");
        }
        if name == "read_buffer" {
            ensure!(
                self.line.is_none_or(|line| line > 0),
                "Invalid one-based line"
            );
            ensure!(
                self.offset == 0 || self.line.is_none() && self.column.is_none(),
                "Use either a source position or a continuation offset"
            );
        }
        if matches!(name, "open_file" | "open_tab" | "open_buffer") {
            ensure!(
                self.line.is_none_or(|line| line > 0),
                "Invalid one-based line"
            );
            ensure!(self.offset == 0, "Open file cannot be paginated");
        }
        Ok(())
    }

    pub fn budget(&self) -> usize {
        self.max_bytes.unwrap_or(DEFAULT_OUTPUT_BYTES)
    }
}

#[derive(Deserialize, Serialize)]
pub struct Request {
    pub token: String,
    pub root: PathBuf,
    pub name: String,
    pub arguments: Arguments,
}

#[derive(Deserialize, Serialize)]
pub struct Response {
    pub text: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Endpoint {
    pub address: SocketAddr,
    pub token: String,
    pub root: PathBuf,
}

impl Endpoint {
    pub fn authorize(&self, request: &Request, writable: bool) -> Result<()> {
        ensure!(
            request.token == self.token && request.root == self.root,
            "Invalid semantic bridge credentials or project"
        );
        request.arguments.validate(&request.name)?;
        ensure!(
            request.name != "rename" || writable,
            "Writes are disabled in the editor"
        );
        Ok(())
    }

    pub fn new(address: SocketAddr, root: &Path) -> Result<Self> {
        ensure!(
            address.ip().is_loopback(),
            "Semantic bridge must listen on loopback"
        );
        Ok(Self {
            address,
            token: uuid::Uuid::new_v4().to_string(),
            root: root.canonicalize()?,
        })
    }

    pub fn publish(&self) -> Result<PathBuf> {
        let path = endpoint_path(&self.root);
        let parent = path.parent().context("Missing endpoint directory")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut file, self)?;
        file.as_file().sync_all()?;
        // NamedTempFile is private to the current user on Unix; preserve its permissions.
        file.persist(&path).map_err(|error| error.error)?;
        Ok(path)
    }
}

pub fn endpoint_path(root: &Path) -> PathBuf {
    paths::data_dir()
        .join("context-mcp")
        .join(format!("{}.json", revision(&root.to_string_lossy())))
}

pub fn call(root: &Path, name: &str, arguments: Arguments) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(endpoint_path(root))
        .context("Open this project in an updated ZedStorm editor to enable semantic tools")?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "Semantic bridge endpoint is too large"
    );
    let endpoint: Endpoint = serde_json::from_slice(&bytes)
        .context("Invalid semantic bridge endpoint; reconnect the editor panel")?;
    ensure!(
        endpoint.root == root && endpoint.address.ip().is_loopback(),
        "Invalid semantic bridge project or address"
    );
    let mut stream = TcpStream::connect_timeout(&endpoint.address, Duration::from_secs(2))
        .context(
            "ZedStorm semantic bridge is unavailable; reopen the project or reconnect the panel",
        )?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(
        &mut stream,
        &Request {
            token: endpoint.token,
            root: root.to_path_buf(),
            name: name.into(),
            arguments,
        },
    )?;
    stream.write_all(b"\n")?;
    let mut bytes = Vec::new();
    std::io::BufReader::new(stream)
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    ensure!(
        bytes.len() <= MAX_REQUEST_BYTES,
        "Semantic bridge response too large"
    );
    let response: Response =
        serde_json::from_slice(&bytes).context("Invalid semantic bridge response")?;
    match (response.text, response.error) {
        (_, Some(error)) => bail!("{error}"),
        (Some(text), None) => Ok(text),
        _ => bail!("Empty semantic bridge response"),
    }
}

pub fn page(items: Vec<Value>, arguments: &Arguments) -> Result<String> {
    ensure!(
        arguments.offset <= items.len(),
        "Offset exceeds result count; restart the query"
    );
    let mut output = String::new();
    let mut next = arguments.offset;
    for item in items.iter().skip(arguments.offset) {
        let line = serde_json::to_string(item)?;
        if output.len() + line.len() + 128 > arguments.budget() {
            ensure!(
                !output.is_empty(),
                "One result exceeds max_bytes; increase the budget or narrow the query"
            );
            break;
        }
        output.push_str(&line);
        output.push('\n');
        next += 1;
    }
    output.push_str(&format!(
        "complete={} total={} next_offset={}\n",
        next == items.len(),
        items.len(),
        next
    ));
    Ok(output)
}

pub fn tool_definitions(writable: bool) -> Vec<Value> {
    TOOLS.iter().filter(|name| writable || **name != "rename").map(|name| {
        let mut properties = json!({
            "max_bytes": {"type": "integer", "minimum": 256, "maximum": MAX_OUTPUT_BYTES, "default": DEFAULT_OUTPUT_BYTES, "description": "UTF-8 response byte limit; follow next_offset when incomplete."},
            "offset": {"type": "integer", "minimum": 0, "default": 0, "description": "Use next_offset; restart if editor contents or index change."}
        });
        let mut required = Vec::new();
        if !matches!(*name, "semantic_status" | "workspace_symbols") {
            properties["path"] = json!({"type": "string", "description": "Project-relative source path."});
            required.push("path");
        }
        if !matches!(
            *name,
            "semantic_status"
                | "workspace_symbols"
                | "symbols"
                | "read_buffer"
                | "open_file"
                | "open_tab"
        ) {
            properties["line"] = json!({"type": "integer", "minimum": 1, "description": "One-based line."});
            properties["column"] = json!({"type": "integer", "minimum": 0, "description": "Zero-based UTF-16 column, not a UTF-8 byte offset."});
            required.extend(["line", "column"]);
        }
        if *name == "workspace_symbols" {
            properties["query"] = json!({"type": "string", "minLength": 1});
            required.push("query");
        }
        if *name == "read_buffer" {
            properties["line"] = json!({"type": "integer", "minimum": 1, "description": "Optional one-based starting line; omit when continuing with offset."});
            properties["column"] = json!({"type": "integer", "minimum": 0, "default": 0, "description": "Optional zero-based UTF-16 column; omit when continuing with offset."});
        }
        if matches!(*name, "open_file" | "open_tab") {
            properties["line"] = json!({"type": "integer", "minimum": 1, "description": "Optional one-based line to navigate to."});
            properties["column"] = json!({"type": "integer", "minimum": 0, "description": "Optional zero-based UTF-16 column to navigate to."});
        }
        if *name == "rename" {
            properties["new_name"] = json!({"type": "string", "minLength": 1});
            properties["revision"] = json!({"type": "string", "description": "Current source buffer revision from read_buffer."});
            required.extend(["new_name", "revision"]);
        }
        let description = match *name {
            "semantic_status" => "Check semantic navigation availability and language-server progress. Requires this project open in ZedStorm; indexing may be incomplete.",
            "read_buffer" => "Read current editor source, including unsaved changes, with revision. offset is a UTF-8 byte cursor.",
            "symbols" => "List document symbols, including nested symbols and their source ranges.",
            "workspace_symbols" => "Search project symbols through running language servers.",
            "definitions" => "Find semantic definitions at a source position.",
            "type_definitions" => "Find semantic type definitions at a source position.",
            "declarations" => "Find semantic declarations at a source position.",
            "references" => "Find semantic references across the project.",
            "implementations" => "Find implementations of a trait, interface or symbol.",
            "hover" => "Get inferred type, signature and documentation at a source position.",
            "incoming_calls" => "Find callers of the symbol at a source position, where supported.",
            "outgoing_calls" => "Find callees of the symbol at a source position, where supported.",
            "rename" => "Rename a symbol across undoable editor buffers. Requires writes enabled in both client and editor. Changes remain unsaved; use the editor to review and save.",
            "open_file" => "Open a file in the editor as a tab for the user. Optionally scrolls to a specified line and column.",
            "open_tab" => "Open a file in the editor as a tab for the user. Optionally scrolls to a specified line and column.",
            _ => unreachable!(),
        };
        tool(name, description, properties, &required, *name != "rename")
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_credentials_project_and_editor_write_access_are_independent() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let endpoint = Endpoint::new("127.0.0.1:1234".parse()?, directory.path())?;
        let mut request = Request {
            token: endpoint.token.clone(),
            root: endpoint.root.clone(),
            name: "semantic_status".into(),
            arguments: Arguments::default(),
        };
        endpoint.authorize(&request, false)?;
        request.token = "wrong".into();
        assert!(endpoint.authorize(&request, true).is_err());
        request.token = endpoint.token.clone();
        request.root = directory.path().join("other-project");
        assert!(endpoint.authorize(&request, true).is_err());
        request.root = endpoint.root.clone();
        request.name = "rename".into();
        request.arguments = Arguments {
            path: Some("main.rs".into()),
            line: Some(1),
            column: Some(0),
            new_name: Some("updated".into()),
            revision: Some("source-revision".into()),
            ..Default::default()
        };
        assert!(endpoint.authorize(&request, false).is_err());
        endpoint.authorize(&request, true)?;
        assert!(Endpoint::new("192.0.2.1:1234".parse()?, directory.path()).is_err());
        Ok(())
    }

    #[test]
    fn validates_semantic_arguments_and_write_schema() {
        assert!(Arguments::default().validate("definitions").is_err());
        let arguments = Arguments {
            path: Some("src/main.rs".into()),
            line: Some(1),
            column: Some(0),
            ..Default::default()
        };
        assert!(arguments.validate("definitions").is_ok());
        assert!(arguments.validate("rename").is_err());
        assert!(arguments.validate("open_file").is_ok());
        assert!(arguments.validate("open_tab").is_ok());
        assert!(
            Arguments {
                path: Some("src/main.rs".into()),
                line: Some(0),
                ..Default::default()
            }
            .validate("open_file")
            .is_err()
        );
        assert!(
            Arguments {
                path: Some("src/main.rs".into()),
                offset: 1,
                ..Default::default()
            }
            .validate("open_file")
            .is_err()
        );
        assert!(
            !tool_definitions(false)
                .iter()
                .any(|tool| tool["name"] == "rename")
        );
        assert!(
            tool_definitions(true)
                .iter()
                .any(|tool| tool["name"] == "rename")
        );
        assert!(
            tool_definitions(false)
                .iter()
                .any(|tool| tool["name"] == "open_file")
        );
        assert!(
            tool_definitions(false)
                .iter()
                .any(|tool| tool["name"] == "open_tab")
        );
    }

    #[test]
    fn pagination_preserves_results_and_enforces_budget() -> Result<()> {
        let items = (0..20)
            .map(|index| json!({"name": format!("symbol_{index}")}))
            .collect::<Vec<_>>();
        let arguments = Arguments {
            max_bytes: Some(256),
            ..Default::default()
        };
        let first = page(items.clone(), &arguments)?;
        assert!(first.len() <= 256);
        assert!(first.contains("complete=false"));
        let mut offset = 0;
        let mut names = Vec::new();
        loop {
            let output = page(
                items.clone(),
                &Arguments {
                    offset,
                    ..arguments.clone()
                },
            )?;
            let mut lines = output.lines().collect::<Vec<_>>();
            let footer = lines.pop().context("Missing footer")?;
            for line in lines {
                names.push(serde_json::from_str::<Value>(line)?);
            }
            offset = names.len();
            if footer.contains("complete=true") {
                break;
            }
        }
        assert_eq!(names, items);
        assert!(
            page(
                items,
                &Arguments {
                    offset: 21,
                    ..arguments
                }
            )
            .is_err()
        );
        Ok(())
    }
}
