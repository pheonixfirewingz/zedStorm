use anyhow::{Context as _, Result, bail, ensure};
use context_mcp::semantic::{Arguments, Endpoint, Request, Response, page};
use futures::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, FutureExt as _};
use gpui::{AnyWindowHandle, App, AppContext as _, AsyncApp, Context, Entity, Focusable as _, Task, WeakEntity};
use language::Buffer;
use project::{
    DocumentSymbol, Location, Project,
    lsp_command::{
        GetDeclarations, GetDefinitions, GetDocumentSymbols, GetHover, GetImplementations,
        GetIncomingCalls, GetOutgoingCalls, GetReferences, GetTypeDefinitions, LspCommand,
        PerformRename, PrepareCallHierarchy, PrepareRename, RenameScope,
    },
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use smol::{io::BufReader, net::TcpListener};
use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    time::Duration,
};
use text::{Bias, PointUtf16, ToOffset as _, ToPoint as _, ToPointUtf16 as _, Unclipped};

pub fn start(
    project: WeakEntity<Project>,
    workspace: WeakEntity<workspace::Workspace>,
    window: AnyWindowHandle,
    root: PathBuf,
    cx: &mut App,
) -> Task<()> {
    cx.spawn(async move |cx| {
        if let Err(error) = serve(project, Review { workspace, window }, root, cx).await {
            log::error!("ZedStorm semantic MCP bridge: {error:#}");
        }
    })
}

struct Registration {
    path: PathBuf,
    token: String,
}

#[derive(Default)]
struct Buffers(
    Vec<(
        Entity<Buffer>,
        Option<project::lsp_store::OpenLspBufferHandle>,
    )>,
    Option<Review>,
);

struct Review {
    workspace: WeakEntity<workspace::Workspace>,
    window: AnyWindowHandle,
}

impl Buffers {
    fn retain(&mut self, buffer: &Entity<Buffer>, cx: &mut AsyncApp) {
        if self.0.iter().any(|(retained, _)| retained == buffer) {
            return;
        }
        if self.0.len() >= 128
            && let Some(index) = self
                .0
                .iter()
                .position(|(buffer, _)| !buffer.read_with(cx, |buffer, _| buffer.is_dirty()))
        {
            self.0.remove(index);
        }
        self.0.push((buffer.clone(), None));
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let result = (|| -> Result<()> {
            let endpoint: Endpoint = match fs::File::open(&self.path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
                    ensure!(
                        bytes.len() <= 64 * 1024,
                        "Semantic bridge endpoint is too large"
                    );
                    serde_json::from_slice(&bytes)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            if endpoint.token == self.token {
                fs::remove_file(&self.path)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("Could not remove semantic MCP endpoint: {error:#}");
        }
    }
}

async fn serve(
    project: WeakEntity<Project>,
    review: Review,
    root: PathBuf,
    cx: &mut AsyncApp,
) -> Result<()> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let endpoint = Endpoint::new(listener.local_addr()?, &root)?;
    let root = endpoint.root.clone();
    let _registration = Registration {
        path: endpoint.publish()?,
        token: endpoint.token.clone(),
    };
    let mut buffers = Buffers(Default::default(), Some(review));
    loop {
        let (mut stream, _) = listener.accept().await?;
        let executor = cx.background_executor().clone();
        let request = async {
            let result = async {
                let mut bytes = Vec::new();
                BufReader::new(&mut stream)
                    .take(64 * 1024 + 1)
                    .read_until(b'\n', &mut bytes)
                    .await?;
                ensure!(bytes.len() <= 64 * 1024, "Semantic request too large");
                let request: Request = serde_json::from_slice(&bytes)?;
                endpoint.authorize(
                    &request,
                    std::env::var("ZEDSTORM_CONTEXT_MCP_WRITE")
                        .is_ok_and(|value| value == "1"),
                )?;
                let operation = execute(
                    &project,
                    &root,
                    request.name.as_str(),
                    request.arguments,
                    &mut buffers,
                    cx,
                )
                .fuse();
                let operation_timeout = executor.timer(Duration::from_secs(35)).fuse();
                futures::pin_mut!(operation, operation_timeout);
                futures::select_biased! {
                    result = operation => result,
                    _ = operation_timeout => Err(anyhow::anyhow!("Semantic request timed out; language server may still be indexing. For a rename, review editor changes before retrying.")),
                }
            }
            .await;
            let response = match result {
                Ok(text) => Response {
                    text: Some(text),
                    error: None,
                },
                Err(error) => Response {
                    text: None,
                    error: Some(format!("{error:#}")),
                },
            };
            stream.write_all(&serde_json::to_vec(&response)?).await?;
            stream.write_all(b"\n").await?;
            anyhow::Ok(())
        }
        .fuse();
        let timeout = executor.timer(Duration::from_secs(45)).fuse();
        futures::pin_mut!(request, timeout);
        let result = futures::select_biased! {
            result = request => result,
            _ = timeout => Err(anyhow::anyhow!("Semantic request timed out; language server may still be indexing")),
        };
        if let Err(error) = result {
            log::warn!("Semantic MCP request failed: {error:#}");
        }
    }
}

async fn execute(
    project: &WeakEntity<Project>,
    root: &Path,
    name: &str,
    arguments: Arguments,
    buffers: &mut Buffers,
    cx: &mut AsyncApp,
) -> Result<String> {
    if name == "semantic_status" {
        let items = project.read_with(cx, |project, cx| {
            project.language_server_statuses(cx).map(|(identifier, status)| json!({
                "id": identifier.to_string(), "name": status.name.to_string(),
                "pending_work": status.pending_work, "pending_diagnostics": status.has_pending_diagnostic_updates,
                "running": project.lsp_store().read(cx).language_server_for_id(identifier).is_some(),
            })).collect()
        })?;
        return page(items, &arguments);
    }
    if name == "workspace_symbols" {
        let requests = project.read_with(cx, |project, cx| {
            project
                .language_server_statuses(cx)
                .filter_map(|(identifier, _)| {
                    project
                        .lsp_store()
                        .read(cx)
                        .language_server_for_id(identifier)
                        .filter(|server| {
                            matches!(
                                server.capabilities().workspace_symbol_provider,
                                Some(lsp::OneOf::Left(true)) | Some(lsp::OneOf::Right(_))
                            )
                        })
                })
                .map(|server| {
                    server.request::<lsp::request::WorkspaceSymbolRequest>(
                        lsp::WorkspaceSymbolParams {
                            query: arguments.query.clone().unwrap_or_default(),
                            ..Default::default()
                        },
                        Duration::from_secs(30),
                    )
                })
                .collect::<Vec<_>>()
        })?;
        ensure!(
            !requests.is_empty(),
            "No running language server supports workspace symbols; open a source file and retry"
        );
        let mut items = Vec::new();
        for response in futures::future::join_all(requests).await {
            if let Some(response) = response.into_response()? {
                match response {
                    lsp::WorkspaceSymbolResponse::Flat(symbols) => {
                        for symbol in symbols {
                            items.push(workspace_symbol(
                                symbol.name,
                                symbol.container_name,
                                symbol.kind,
                                symbol.location,
                                root,
                            )?);
                        }
                    }
                    lsp::WorkspaceSymbolResponse::Nested(symbols) => {
                        for symbol in symbols {
                            let lsp::OneOf::Left(location) = symbol.location else {
                                bail!("Language server returned an unresolved workspace symbol");
                            };
                            items.push(workspace_symbol(
                                symbol.name,
                                symbol.container_name,
                                symbol.kind,
                                location,
                                root,
                            )?);
                        }
                    }
                }
            }
        }
        items.sort_by_key(Value::to_string);
        items.dedup();
        return page(items, &arguments);
    }
    let path = checked_path(root, arguments.path.as_deref().context("Missing path")?)?;
    let buffer = project
        .update(cx, |project, cx| {
            let (worktree, relative_path) = project
                .find_worktree(&path, cx)
                .context("File is not in this editor project")?;
            let path = project::ProjectPath::from((worktree.read(cx).id(), relative_path));
            anyhow::Ok(project.open_buffer(path, cx))
        })??
        .await?;
    buffers.retain(&buffer, cx);
    if name == "read_buffer" {
        return buffer.read_with(cx, |buffer, _| live_source(buffer, &arguments));
    }
    if matches!(name, "open_file" | "open_tab" | "open_buffer") {
        let review = buffers
            .1
            .as_ref()
            .context("Editor workspace is not available")?;
        let project_path = project
            .read_with(cx, |project, cx| project.find_project_path(&path, cx))?
            .context("File is not in this editor project")?;
        let item = cx
            .update_window(review.window, |_, window, cx| {
                review.workspace.update(cx, |workspace, cx| {
                    workspace.open_path(project_path, None, true, window, cx)
                })
            })??
            .await?;
        let position = if let Some(line) = arguments.line {
            let column = arguments.column.unwrap_or(0);
            Some(buffer.read_with(cx, |buffer, _| {
                let position = PointUtf16::new(line.saturating_sub(1), column);
                let clipped = buffer.clip_point_utf16(Unclipped(position), Bias::Left);
                Unclipped(clipped).to_point(buffer)
            }))
        } else {
            None
        };
        cx.update_window(review.window, |_, window, cx| {
            if let Some(editor) = item.downcast::<editor::Editor>() {
                editor.update(cx, |editor, cx| {
                    if let Some(point) = position {
                        editor.change_selections(
                            editor::SelectionEffects::scroll(editor::scroll::Autoscroll::center()),
                            window,
                            cx,
                            |s| s.select_ranges([point..point]),
                        );
                    }
                    editor.focus_handle(cx).focus(window, cx);
                });
            }
        })?;
        return Ok(format!(
            "opened tab for {}{}",
            display_path(&path, root),
            if let Some(line) = arguments.line {
                if let Some(column) = arguments.column {
                    format!(":{line}:{column}")
                } else {
                    format!(":{line}")
                }
            } else {
                String::new()
            }
        ));
    }
    if let Some((_, registration)) = buffers
        .0
        .iter_mut()
        .find(|(retained, _)| retained == &buffer)
        && registration.is_none()
    {
        *registration = Some(project.update(cx, |project, cx| {
            project.register_buffer_with_language_servers(&buffer, cx)
        })?);
    }
    if name == "symbols" {
        let symbols = project
            .update(cx, |project, cx| {
                request(project, buffer.clone(), GetDocumentSymbols, cx)
            })??
            .await?;
        let mut items = Vec::new();
        flatten_symbols(symbols, "", &mut items);
        return page(items, &arguments);
    }
    let position = buffer.read_with(cx, |buffer, _| -> Result<PointUtf16> {
        let position = PointUtf16::new(
            arguments.line.context("Missing line")? - 1,
            arguments.column.context("Missing column")?,
        );
        ensure!(
            buffer.clip_point_utf16(Unclipped(position), Bias::Left) == position,
            "Position is out of range or splits a UTF-16 character"
        );
        Ok(position)
    })?;
    match name {
        "definitions" | "type_definitions" | "declarations" | "implementations" => {
            let locations = project
                .update(cx, |project, cx| match name {
                    "definitions" => {
                        request(project, buffer.clone(), GetDefinitions { position }, cx)
                    }
                    "type_definitions" => {
                        request(project, buffer.clone(), GetTypeDefinitions { position }, cx)
                    }
                    "declarations" => {
                        request(project, buffer.clone(), GetDeclarations { position }, cx)
                    }
                    _ => request(project, buffer.clone(), GetImplementations { position }, cx),
                })??
                .await?;
            let items = cx.update(|cx| {
                locations
                    .iter()
                    .map(|link| location(&link.target, root, cx))
                    .collect()
            });
            page(items, &arguments)
        }
        "references" => {
            let locations = project
                .update(cx, |project, cx| {
                    request(project, buffer.clone(), GetReferences { position }, cx)
                })??
                .await?;
            let items = cx.update(|cx| {
                locations
                    .iter()
                    .map(|reference| location(reference, root, cx))
                    .collect()
            });
            page(items, &arguments)
        }
        "hover" => {
            let hover = project
                .update(cx, |project, cx| {
                    request(project, buffer.clone(), GetHover { position }, cx)
                })??
                .await?;
            let items = hover.map(|hover| hover.contents.into_iter().map(|block| json!({"text": block.text, "kind": format!("{:?}", block.kind)})).collect()).unwrap_or_default();
            page(items, &arguments)
        }
        "incoming_calls" | "outgoing_calls" => {
            let hierarchy = project
                .update(cx, |project, cx| {
                    request(
                        project,
                        buffer.clone(),
                        PrepareCallHierarchy { position },
                        cx,
                    )
                })??
                .await?;
            let mut items = Vec::new();
            for item in hierarchy {
                if name == "incoming_calls" {
                    let calls = project
                        .update(cx, |project, cx| {
                            project.request_lsp(
                                item.buffer.clone(),
                                project::LanguageServerToQuery::Other(item.server_id),
                                GetIncomingCalls { item },
                                cx,
                            )
                        })?
                        .await?;
                    cx.update(|cx| for call in calls {
                        items.push(json!({"name": call.from.name, "location": location(&Location { buffer: call.from.buffer, range: call.from.selection_range }, root, cx),
                            "call_sites": call.from_ranges.iter().map(|range| location(range, root, cx)).collect::<Vec<_>>()}));
                    });
                } else {
                    let calls = project
                        .update(cx, |project, cx| {
                            project.request_lsp(
                                item.buffer.clone(),
                                project::LanguageServerToQuery::Other(item.server_id),
                                GetOutgoingCalls { item },
                                cx,
                            )
                        })?
                        .await?;
                    cx.update(|cx| for call in calls {
                        items.push(json!({"name": call.to.name, "location": location(&Location { buffer: call.to.buffer, range: call.to.selection_range }, root, cx),
                            "call_sites": call.from_ranges.iter().map(|range| location(range, root, cx)).collect::<Vec<_>>()}));
                    });
                }
            }
            page(items, &arguments)
        }
        "rename" => {
            let budget = arguments.budget();
            buffer.read_with(cx, |buffer, _| {
                ensure!(
                    arguments.revision.as_deref() == Some(&buffer_revision(buffer)),
                    "Stale buffer revision; read_buffer again before renaming"
                );
                Ok(())
            })?;
            let preparation = project
                .update(cx, |project, cx| {
                    request(project, buffer.clone(), PrepareRename { position }, cx)
                })??
                .await?;
            let server = match preparation {
                project::PrepareRenameResponse::Success {
                    language_server_id, ..
                } => language_server_id,
                project::PrepareRenameResponse::OnlyUnpreparedRenameSupported => None,
                project::PrepareRenameResponse::InvalidPosition => {
                    bail!("Symbol cannot be renamed at this position")
                }
            };
            // Preparation awaits the server; verify the source has not changed during that request.
            let transaction = project
                .update(cx, |project, cx| {
                    ensure!(
                        arguments.revision.as_deref() == Some(&buffer_revision(buffer.read(cx))),
                        "Buffer changed while preparing rename; read_buffer again"
                    );
                    let request = PerformRename {
                        position,
                        new_name: arguments.new_name.context("Missing new name")?,
                        push_to_history: true,
                        language_server_id: server,
                        scope: Some(RenameScope {
                            root: root.to_path_buf(),
                            source_version: buffer.read(cx).version(),
                        }),
                    };
                    Ok(project.request_lsp(
                        buffer.clone(),
                        server
                            .map(project::LanguageServerToQuery::Other)
                            .unwrap_or(project::LanguageServerToQuery::FirstCapable),
                        request,
                        cx,
                    ))
                })??
                .await?;
            for buffer in transaction.0.keys() {
                buffers.retain(buffer, cx);
            }
            let mut review_failures = 0;
            if let Some(review) = &buffers.1 {
                for buffer in transaction.0.keys() {
                    let path = buffer.read_with(cx, |buffer, cx| {
                        buffer.file().map(|file| file.full_path(cx))
                    });
                    if let Some(path) = path {
                        let result = async {
                            let path = project
                                .read_with(cx, |project, cx| project.find_project_path(&path, cx))?
                                .context("Renamed buffer has no project path")?;
                            cx.update_window(review.window, |_, window, cx| {
                                review.workspace.update(cx, |workspace, cx| {
                                    workspace.open_path(path, None, false, window, cx)
                                })
                            })??
                            .await?;
                            anyhow::Ok(())
                        }
                        .await;
                        if let Err(error) = result {
                            review_failures += 1;
                            log::error!(
                                "Rename applied, but could not open its review buffer: {error:#}"
                            );
                        }
                    }
                }
            }
            let items = cx.update(|cx| transaction.0.keys().map(|buffer| json!({"path": buffer.read(cx).file().map(|file| display_path(&file.full_path(cx), root)), "unsaved": buffer.read(cx).is_dirty()})).collect());
            // Mutation receipts must never require replaying a rename to retrieve another page.
            let count = transaction.0.len();
            let mut prefix = format!("renamed buffers={count}; changes are undoable and unsaved\n");
            if review_failures > 0 {
                prefix.push_str(&format!(
                    "review_open_failed={review_failures}; open the modified files manually\n"
                ));
            }
            let receipt = page(
                items,
                &Arguments {
                    offset: 0,
                    max_bytes: Some(budget.saturating_sub(prefix.len())),
                    ..Default::default()
                },
            );
            Ok(format!("{prefix}{}", receipt.unwrap_or_else(|_| "complete=false; changed-file list exceeds the receipt budget; review modified editor buffers.\n".into())))
        }
        _ => bail!("Unknown semantic tool"),
    }
}

fn checked_path(root: &Path, path: &str) -> Result<PathBuf> {
    // Reuse filesystem MCP's platform-specific traversal, alias, metadata and symlink checks.
    context_mcp::Server::new(root, false)?.resolve_source_path(path)
}

fn request<R: LspCommand>(
    project: &mut Project,
    buffer: Entity<Buffer>,
    request: R,
    cx: &mut Context<Project>,
) -> Result<Task<Result<R::Response>>>
where
    <R::LspRequest as lsp::request::Request>::Result: Send,
    <R::LspRequest as lsp::request::Request>::Params: Send,
{
    let supported = project.lsp_store().update(cx, |store, cx| {
        let identifiers = buffer.update(cx, |buffer, cx| {
            store.language_servers_for_local_buffer(buffer, cx)
        });
        identifiers.into_iter().any(|identifier| {
            store.language_server_for_id(identifier).is_some()
                && store.language_server_capable_of_lsp_request(&buffer, identifier, &request, cx)
        })
    });
    ensure!(
        supported,
        "No running language server supports {} for this file; check semantic_status and retry after startup/indexing",
        request.display_name()
    );
    Ok(project.request_lsp(
        buffer,
        project::LanguageServerToQuery::FirstCapable,
        request,
        cx,
    ))
}

fn workspace_symbol(
    name: String,
    container: Option<String>,
    kind: lsp::SymbolKind,
    location: lsp::Location,
    root: &Path,
) -> Result<Value> {
    let path = location
        .uri
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("Workspace symbol has a non-file URI"))?;
    Ok(
        json!({"name": name, "container": container, "kind": kind, "path": display_path(&path, root),
        "range": {"start": {"line": location.range.start.line + 1, "column": location.range.start.character},
            "end": {"line": location.range.end.line + 1, "column": location.range.end.character}}}),
    )
}

fn display_path(path: &Path, root: &Path) -> String {
    util::paths::SanitizedPath::new(path)
        .as_path()
        .strip_prefix(util::paths::SanitizedPath::new(root).as_path())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn utf16_range(range: std::ops::Range<Unclipped<PointUtf16>>) -> Value {
    json!({"start": {"line": range.start.0.row + 1, "column": range.start.0.column},
        "end": {"line": range.end.0.row + 1, "column": range.end.0.column}})
}

fn flatten_symbols(symbols: Vec<DocumentSymbol>, parent: &str, items: &mut Vec<Value>) {
    for symbol in symbols {
        let name_path = if parent.is_empty() {
            symbol.name.clone()
        } else {
            format!("{parent}/{}", symbol.name)
        };
        items.push(json!({"name": symbol.name, "name_path": name_path, "kind": format!("{:?}", symbol.kind),
            "range": utf16_range(symbol.range), "selection_range": utf16_range(symbol.selection_range)}));
        flatten_symbols(symbol.children, &name_path, items);
    }
}

fn location(location: &Location, root: &Path, cx: &App) -> Value {
    let buffer = location.buffer.read(cx);
    let start = location.range.start.to_point(buffer);
    let end = location.range.end.to_point(buffer);
    let snippet = buffer
        .text_for_range(location.range.clone())
        .flat_map(str::chars)
        .take(160)
        .collect::<String>();
    json!({"path": buffer.file().map(|file| display_path(&file.full_path(cx), root)),
        "range": utf16_range(Unclipped(start.to_point_utf16(buffer))..Unclipped(end.to_point_utf16(buffer))),
        "snippet": snippet, "unsaved": buffer.is_dirty()})
}

fn buffer_revision(buffer: &Buffer) -> String {
    format!("{:x}", Sha256::digest(buffer.text().as_bytes()))
}

fn live_source(buffer: &Buffer, arguments: &Arguments) -> Result<String> {
    let text = buffer.text();
    ensure!(
        text.len() <= 8 * 1024 * 1024,
        "Source buffer exceeds the 8 MiB limit"
    );
    let offset = if arguments.line.is_some() || arguments.column.is_some() {
        let position = PointUtf16::new(
            arguments
                .line
                .unwrap_or(1)
                .checked_sub(1)
                .context("Invalid one-based line")?,
            arguments.column.unwrap_or(0),
        );
        ensure!(
            buffer.clip_point_utf16(Unclipped(position), Bias::Left) == position,
            "Source position is out of range or splits a UTF-16 character"
        );
        position.to_offset(buffer)
    } else {
        arguments.offset
    };
    ensure!(
        offset <= text.len() && text.is_char_boundary(offset),
        "Invalid UTF-8 source offset"
    );
    let mut end = (offset + arguments.budget().saturating_sub(192)).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let position = buffer.offset_to_point_utf16(offset);
    Ok(format!(
        "revision={} unsaved={} line={} column={}\n{}\ncomplete={} next_offset={}\n",
        buffer_revision(buffer),
        buffer.is_dirty(),
        position.row + 1,
        position.column,
        &text[offset..end],
        end == text.len(),
        end
    ))
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use ::fs::FakeFs;
    use futures::StreamExt as _;
    use gpui::TestAppContext;
    use language::{FakeLspAdapter, rust_lang};

    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = settings::SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
        });
    }

    fn semantic_adapter() -> FakeLspAdapter {
        FakeLspAdapter {
            capabilities: lsp::ServerCapabilities {
                document_symbol_provider: Some(lsp::OneOf::Left(true)),
                references_provider: Some(lsp::OneOf::Left(true)),
                hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
                call_hierarchy_provider: Some(lsp::CallHierarchyServerCapability::Simple(true)),
                rename_provider: Some(lsp::OneOf::Right(lsp::RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                ..lsp::LanguageServer::full_capabilities()
            },
            ..Default::default()
        }
    }

    struct Fixture {
        directory: tempfile::TempDir,
        project: Entity<Project>,
        buffer: Entity<Buffer>,
        server: lsp::FakeLanguageServer,
        _registration: project::lsp_store::OpenLspBufferHandle,
    }

    async fn fixture(cx: &mut TestAppContext, adapter: FakeLspAdapter) -> Result<Fixture> {
        init(cx);
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("main.rs"),
            "fn alpha() {}\nfn beta() { alpha(); }\n",
        )?;
        fs::write(
            directory.path().join("helper.rs"),
            "fn helper() { alpha(); }\n",
        )?;
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(directory.path(), json!({"main.rs": "fn alpha() {}\nfn beta() { alpha(); }\n", "helper.rs": "fn helper() { alpha(); }\n"})).await;
        let project = Project::test(fs, [directory.path()], cx).await;
        let languages = project.read_with(cx, |project, _| project.languages().clone());
        languages.add(rust_lang());
        let mut servers = languages.register_fake_lsp("Rust", adapter);
        let (buffer, registration) = project
            .update(cx, |project, cx| {
                project.open_local_buffer_with_lsp(directory.path().join("main.rs"), cx)
            })
            .await?;
        let server = servers
            .next()
            .await
            .context("Missing fake language server")?;
        cx.run_until_parked();
        Ok(Fixture {
            directory,
            project,
            buffer,
            server,
            _registration: registration,
        })
    }

    fn position(path: &str, line: u32, column: u32) -> Arguments {
        Arguments {
            path: Some(path.into()),
            line: Some(line),
            column: Some(column),
            ..Default::default()
        }
    }

    #[gpui::test]
    fn live_source_pages_unsaved_unicode_without_splitting_characters(cx: &mut TestAppContext) {
        let result = live_source_pages_result(cx);
        assert!(result.is_ok(), "{result:?}");
    }

    fn live_source_pages_result(cx: &mut TestAppContext) -> Result<()> {
        init(cx);
        let buffer = cx.new(|cx| Buffer::local("é🦀\n".repeat(100), cx));
        let initial = buffer.read_with(cx, |buffer, _| buffer_revision(buffer));
        buffer.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "unsaved\n")], None, cx);
        });
        buffer.read_with(cx, |buffer, _| {
            assert_ne!(initial, buffer_revision(buffer));
            let arguments = Arguments {
                max_bytes: Some(256),
                ..Default::default()
            };
            let first = live_source(buffer, &arguments)?;
            assert!(first.contains("unsaved=true"));
            assert!(first.contains("unsaved\n"));
            assert!(first.len() <= 256);
            let mut offset = 0;
            let mut reconstructed = String::new();
            while offset < buffer.text().len() {
                let output = live_source(
                    buffer,
                    &Arguments {
                        offset,
                        ..arguments.clone()
                    },
                )?;
                let (_, body) = output.split_once('\n').context("Missing source header")?;
                let (body, footer) = body
                    .rsplit_once("\ncomplete=")
                    .context("Missing source footer")?;
                reconstructed.push_str(body);
                offset = footer
                    .split_once("next_offset=")
                    .context("Missing source cursor")?
                    .1
                    .trim()
                    .parse()?;
            }
            assert_eq!(reconstructed, buffer.text());
            assert!(
                live_source(
                    buffer,
                    &Arguments {
                        offset: 9,
                        ..arguments
                    }
                )
                .is_err()
            );
            anyhow::Ok(())
        })
    }

    #[gpui::test]
    async fn semantic_navigation_uses_editor_buffers_and_propagates_server_errors(
        cx: &mut TestAppContext,
    ) {
        let result = semantic_navigation_result(cx).await;
        assert!(result.is_ok(), "{result:?}");
    }

    async fn semantic_navigation_result(cx: &mut TestAppContext) -> Result<()> {
        let fixture = fixture(cx, semantic_adapter()).await?;
        let uri = lsp::Uri::from_file_path(fixture.directory.path().join("main.rs"))
            .map_err(|_| anyhow::anyhow!("Invalid fixture URI"))?;
        let _definitions = fixture
            .server
            .set_request_handler::<lsp::request::GotoDefinition, _, _>(move |parameters, _| {
                let uri = uri.clone();
                async move {
                    assert_eq!(
                        parameters.text_document_position_params.position,
                        lsp::Position::new(1, 12)
                    );
                    Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                        lsp::Location::new(
                            uri,
                            lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(0, 8)),
                        ),
                    )))
                }
            });
        let _references = fixture
            .server
            .set_request_handler::<lsp::request::References, _, _>(|_, _| async {
                anyhow::bail!("still indexing")
            });
        let _symbols = fixture
            .server
            .set_request_handler::<lsp::request::DocumentSymbolRequest, _, _>(|_, _| async {
                Ok(Some(lsp::DocumentSymbolResponse::Nested(vec![
                    lsp::DocumentSymbol {
                        name: "alpha".into(),
                        kind: lsp::SymbolKind::FUNCTION,
                        range: lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 13)),
                        selection_range: lsp::Range::new(
                            lsp::Position::new(0, 3),
                            lsp::Position::new(0, 8),
                        ),
                        detail: None,
                        tags: None,
                        deprecated: None,
                        children: None,
                    },
                ])))
            });
        fixture.buffer.update(cx, |buffer, cx| {
            buffer.edit([(13..13, " // unsaved")], None, cx);
        });
        let mut buffers = Buffers::default();
        let mut async_cx = cx.to_async();
        let project = fixture.project.downgrade();
        let output = execute(
            &project,
            fixture.directory.path(),
            "definitions",
            position("main.rs", 2, 12),
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert!(output.contains("alpha"));
        assert!(output.contains("\"unsaved\":true"));
        let output = execute(
            &project,
            fixture.directory.path(),
            "read_buffer",
            Arguments {
                path: Some("main.rs".into()),
                ..Default::default()
            },
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert!(output.contains("// unsaved"));
        let symbols = execute(
            &project,
            fixture.directory.path(),
            "symbols",
            Arguments {
                path: Some("main.rs".into()),
                ..Default::default()
            },
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert!(symbols.contains("\"name_path\":\"alpha\""));
        let error = execute(
            &project,
            fixture.directory.path(),
            "references",
            position("main.rs", 1, 3),
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected indexing error")?;
        assert!(format!("{error:#}").contains("still indexing"));
        let error = execute(
            &project,
            fixture.directory.path(),
            "definitions",
            position("main.rs", 999, 0),
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected position error")?;
        assert!(format!("{error:#}").contains("out of range"));
        Ok(())
    }

    #[gpui::test]
    async fn semantic_hover_workspace_symbols_implementations_and_calls(cx: &mut TestAppContext) {
        let result = semantic_hover_workspace_calls_result(cx).await;
        assert!(result.is_ok(), "{result:?}");
    }

    async fn semantic_hover_workspace_calls_result(cx: &mut TestAppContext) -> Result<()> {
        let fixture = fixture(cx, semantic_adapter()).await?;
        let uri = lsp::Uri::from_file_path(fixture.directory.path().join("main.rs"))
            .map_err(|_| anyhow::anyhow!("Invalid fixture URI"))?;
        let range = lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(0, 8));
        let symbol = lsp::CallHierarchyItem {
            name: "alpha".into(),
            kind: lsp::SymbolKind::FUNCTION,
            tags: None,
            detail: None,
            uri: uri.clone(),
            range,
            selection_range: range,
            data: Some(json!({"key": "preserved"})),
        };
        let _hover = fixture
            .server
            .set_request_handler::<lsp::request::HoverRequest, _, _>(|_, _| async {
                Ok(Some(lsp::Hover {
                    contents: lsp::HoverContents::Scalar(lsp::MarkedString::String(
                        "fn alpha() -> ()".into(),
                    )),
                    range: None,
                }))
            });
        let _workspace = fixture
            .server
            .set_request_handler::<lsp::request::WorkspaceSymbolRequest, _, _>({
                let uri = uri.clone();
                move |parameters, _| {
                    let uri = uri.clone();
                    async move {
                        assert_eq!(parameters.query, "alpha");
                        Ok(Some(lsp::WorkspaceSymbolResponse::Flat(vec![
                            lsp::SymbolInformation {
                                name: "alpha".into(),
                                kind: lsp::SymbolKind::FUNCTION,
                                tags: None,
                                deprecated: None,
                                location: lsp::Location::new(uri, range),
                                container_name: None,
                            },
                        ])))
                    }
                }
            });
        let _implementations = fixture
            .server
            .set_request_handler::<lsp::request::GotoImplementation, _, _>({
                let uri = uri.clone();
                move |_, _| {
                    let uri = uri.clone();
                    async move {
                        Ok(Some(lsp::GotoImplementationResponse::Scalar(
                            lsp::Location::new(uri, range),
                        )))
                    }
                }
            });
        let _references = fixture
            .server
            .set_request_handler::<lsp::request::References, _, _>({
                let uri = uri.clone();
                move |_, _| {
                    let uri = uri.clone();
                    async move { Ok(Some(vec![lsp::Location::new(uri, range)])) }
                }
            });
        let _prepare = fixture
            .server
            .set_request_handler::<lsp::request::CallHierarchyPrepare, _, _>({
                let symbol = symbol.clone();
                move |_, _| {
                    let symbol = symbol.clone();
                    async move { Ok(Some(vec![symbol])) }
                }
            });
        let _incoming = fixture
            .server
            .set_request_handler::<lsp::request::CallHierarchyIncomingCalls, _, _>({
                let symbol = symbol.clone();
                move |parameters, _| {
                    let symbol = symbol.clone();
                    async move {
                        assert_eq!(parameters.item.data, Some(json!({"key": "preserved"})));
                        Ok(Some(vec![lsp::CallHierarchyIncomingCall {
                            from: symbol,
                            from_ranges: vec![range],
                        }]))
                    }
                }
            });
        let _outgoing = fixture
            .server
            .set_request_handler::<lsp::request::CallHierarchyOutgoingCalls, _, _>(move |_, _| {
                let symbol = symbol.clone();
                async move {
                    Ok(Some(vec![lsp::CallHierarchyOutgoingCall {
                        to: symbol,
                        from_ranges: vec![range],
                    }]))
                }
            });
        let mut buffers = Buffers::default();
        let mut async_cx = cx.to_async();
        let project = fixture.project.downgrade();
        for name in [
            "hover",
            "implementations",
            "references",
            "incoming_calls",
            "outgoing_calls",
        ] {
            let output = execute(
                &project,
                fixture.directory.path(),
                name,
                position("main.rs", 1, 3),
                &mut buffers,
                &mut async_cx,
            )
            .await?;
            assert!(output.contains("alpha"), "{name}: {output}");
            assert!(output.contains("complete=true"));
        }
        let output = execute(
            &project,
            fixture.directory.path(),
            "workspace_symbols",
            Arguments {
                query: Some("alpha".into()),
                ..Default::default()
            },
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert!(output.contains("\"path\":\"main.rs\""));
        assert!(output.contains("alpha"));
        Ok(())
    }

    #[gpui::test]
    async fn semantic_requests_distinguish_unsupported_from_empty_results(cx: &mut TestAppContext) {
        let result = unsupported_semantic_requests_result(cx).await;
        assert!(result.is_ok(), "{result:?}");
    }

    async fn unsupported_semantic_requests_result(cx: &mut TestAppContext) -> Result<()> {
        let fixture = fixture(
            cx,
            FakeLspAdapter {
                capabilities: lsp::ServerCapabilities::default(),
                ..Default::default()
            },
        )
        .await?;
        let mut buffers = Buffers::default();
        let mut async_cx = cx.to_async();
        let error = execute(
            &fixture.project.downgrade(),
            fixture.directory.path(),
            "definitions",
            position("main.rs", 1, 3),
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected unsupported error")?;
        assert!(format!("{error:#}").contains("No running language server supports"));
        fixture
            .server
            .request::<lsp::request::RegisterCapability>(
                lsp::RegistrationParams {
                    registrations: vec![lsp::Registration {
                        id: "semantic-definitions".into(),
                        method: "textDocument/definition".into(),
                        register_options: Some(
                            json!({"documentSelector": [{"language": "rust", "scheme": "file"}]}),
                        ),
                    }],
                },
                Duration::from_secs(5),
            )
            .await
            .into_response()?;
        cx.run_until_parked();
        let _definitions = fixture
            .server
            .set_request_handler::<lsp::request::GotoDefinition, _, _>(|_, _| async { Ok(None) });
        let output = execute(
            &fixture.project.downgrade(),
            fixture.directory.path(),
            "definitions",
            position("main.rs", 1, 3),
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert!(output.contains("complete=true total=0"));
        Ok(())
    }

    #[gpui::test]
    async fn scoped_rename_preserves_unsaved_buffers_and_rejects_stale_revisions(
        cx: &mut TestAppContext,
    ) {
        let result = scoped_rename_result(cx).await;
        assert!(result.is_ok(), "{result:?}");
    }

    async fn scoped_rename_result(cx: &mut TestAppContext) -> Result<()> {
        let fixture = fixture(cx, semantic_adapter()).await?;
        let uri = lsp::Uri::from_file_path(fixture.directory.path().join("main.rs"))
            .map_err(|_| anyhow::anyhow!("Invalid fixture URI"))?;
        let helper_uri = lsp::Uri::from_file_path(fixture.directory.path().join("helper.rs"))
            .map_err(|_| anyhow::anyhow!("Invalid fixture URI"))?;
        let _prepare = fixture
            .server
            .set_request_handler::<lsp::request::PrepareRenameRequest, _, _>(|_, _| async {
                Ok(Some(lsp::PrepareRenameResponse::Range(lsp::Range::new(
                    lsp::Position::new(0, 3),
                    lsp::Position::new(0, 8),
                ))))
            });
        let _rename = fixture
            .server
            .set_request_handler::<lsp::request::Rename, _, _>(move |parameters, _| {
                let uri = uri.clone();
                let helper_uri = helper_uri.clone();
                async move {
                    assert_eq!(parameters.new_name, "gamma");
                    Ok(Some(lsp::WorkspaceEdit {
                        changes: Some(
                            [
                                (
                                    uri,
                                    vec![lsp::TextEdit::new(
                                        lsp::Range::new(
                                            lsp::Position::new(0, 3),
                                            lsp::Position::new(0, 8),
                                        ),
                                        "gamma".into(),
                                    )],
                                ),
                                (
                                    helper_uri,
                                    vec![lsp::TextEdit::new(
                                        lsp::Range::new(
                                            lsp::Position::new(0, 14),
                                            lsp::Position::new(0, 19),
                                        ),
                                        "gamma".into(),
                                    )],
                                ),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                        ..Default::default()
                    }))
                }
            });
        let revision = fixture
            .buffer
            .read_with(cx, |buffer, _| buffer_revision(buffer));
        let mut arguments = position("main.rs", 1, 3);
        arguments.new_name = Some("gamma".into());
        arguments.revision = Some("stale".into());
        arguments.max_bytes = Some(256);
        let mut buffers = Buffers::default();
        let mut async_cx = cx.to_async();
        let project = fixture.project.downgrade();
        let error = execute(
            &project,
            fixture.directory.path(),
            "rename",
            arguments.clone(),
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected stale revision error")?;
        assert!(format!("{error:#}").contains("Stale buffer revision"));
        arguments.revision = Some(revision);
        let output = execute(
            &project,
            fixture.directory.path(),
            "rename",
            arguments,
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert!(output.contains("renamed buffers=2"));
        assert!(output.len() <= 256);
        assert!(
            fixture
                .buffer
                .read_with(cx, |buffer, _| buffer.text().contains("fn gamma"))
        );
        assert!(fs::read_to_string(fixture.directory.path().join("main.rs"))?.contains("fn alpha"));
        assert!(buffers.0.iter().any(|(buffer, _)| {
            buffer.read_with(cx, |buffer, _| buffer.text().contains("helper() { gamma"))
        }));
        let outside = tempfile::NamedTempFile::new()?;
        let outside_uri = lsp::Uri::from_file_path(outside.path())
            .map_err(|_| anyhow::anyhow!("Invalid outside URI"))?;
        let _outside_rename = fixture
            .server
            .set_request_handler::<lsp::request::Rename, _, _>(move |_, _| {
                let outside_uri = outside_uri.clone();
                async move {
                    Ok(Some(lsp::WorkspaceEdit {
                        changes: Some(
                            [(
                                outside_uri,
                                vec![lsp::TextEdit::new(
                                    lsp::Range::default(),
                                    "forbidden".into(),
                                )],
                            )]
                            .into_iter()
                            .collect(),
                        ),
                        ..Default::default()
                    }))
                }
            });
        let mut arguments = position("main.rs", 1, 3);
        arguments.new_name = Some("delta".into());
        arguments.revision = Some(
            fixture
                .buffer
                .read_with(cx, |buffer, _| buffer_revision(buffer)),
        );
        let error = execute(
            &project,
            fixture.directory.path(),
            "rename",
            arguments.clone(),
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected scope rejection")?;
        assert!(format!("{error:#}").contains("outside the MCP project root"));
        assert_eq!(fs::read_to_string(outside.path())?, "");
        let source_uri = lsp::Uri::from_file_path(fixture.directory.path().join("main.rs"))
            .map_err(|_| anyhow::anyhow!("Invalid source URI"))?;
        let _resource_rename = fixture
            .server
            .set_request_handler::<lsp::request::Rename, _, _>(move |_, _| {
                let source_uri = source_uri.clone();
                async move {
                    Ok(Some(lsp::WorkspaceEdit {
                        document_changes: Some(lsp::DocumentChanges::Operations(vec![
                            lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Delete(
                                lsp::DeleteFile {
                                    uri: source_uri,
                                    options: None,
                                },
                            )),
                        ])),
                        ..Default::default()
                    }))
                }
            });
        let error = execute(
            &project,
            fixture.directory.path(),
            "rename",
            arguments,
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected resource rejection")?;
        assert!(format!("{error:#}").contains("cannot create, delete or rename files"));
        assert!(fixture.directory.path().join("main.rs").exists());
        let source_uri = lsp::Uri::from_file_path(fixture.directory.path().join("main.rs"))
            .map_err(|_| anyhow::anyhow!("Invalid source URI"))?;
        let buffer = fixture.buffer.clone();
        let _changed_source = fixture
            .server
            .set_request_handler::<lsp::request::Rename, _, _>(move |_, mut cx| {
                let buffer = buffer.clone();
                let source_uri = source_uri.clone();
                async move {
                    buffer.update(&mut cx, |buffer, cx| {
                        let end = buffer.len();
                        buffer.edit([(end..end, "// changed during rename\n")], None, cx);
                    });
                    Ok(Some(lsp::WorkspaceEdit {
                        changes: Some(
                            [(
                                source_uri,
                                vec![lsp::TextEdit::new(
                                    lsp::Range::new(
                                        lsp::Position::new(0, 3),
                                        lsp::Position::new(0, 8),
                                    ),
                                    "delta".into(),
                                )],
                            )]
                            .into_iter()
                            .collect(),
                        ),
                        ..Default::default()
                    }))
                }
            });
        let mut arguments = position("main.rs", 1, 3);
        arguments.new_name = Some("delta".into());
        arguments.revision = Some(
            fixture
                .buffer
                .read_with(cx, |buffer, _| buffer_revision(buffer)),
        );
        let error = execute(
            &project,
            fixture.directory.path(),
            "rename",
            arguments,
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected concurrent source rejection")?;
        assert!(format!("{error:#}").contains("Source buffer changed during rename"));
        assert!(
            fixture
                .buffer
                .read_with(cx, |buffer, _| buffer.text().contains("fn gamma"))
        );
        Ok(())
    }

    #[gpui::test]
    async fn open_file_tab_opens_in_workspace_and_positions_cursor(cx: &mut TestAppContext) {
        let result = open_file_tab_result(cx).await;
        assert!(result.is_ok(), "{result:?}");
    }

    async fn open_file_tab_result(cx: &mut TestAppContext) -> Result<()> {
        let fixture = fixture(cx, semantic_adapter()).await?;
        let mut buffers = Buffers::default();
        let mut async_cx = cx.to_async();
        let project = fixture.project.downgrade();
        let error = execute(
            &project,
            fixture.directory.path(),
            "open_file",
            Arguments {
                path: Some("main.rs".into()),
                ..Default::default()
            },
            &mut buffers,
            &mut async_cx,
        )
        .await
        .err()
        .context("Expected missing workspace error")?;
        assert!(format!("{error:#}").contains("Editor workspace is not available"));

        let mut workspace_entity = None;
        let window = cx.add_window(|window, cx| {
            let workspace = workspace::Workspace::test_new(fixture.project.clone(), window, cx);
            workspace_entity = Some(cx.entity().clone());
            workspace
        });
        let workspace = workspace_entity.context("Missing workspace")?;
        buffers.1 = Some(Review {
            workspace: workspace.downgrade(),
            window: window.into(),
        });

        let output = execute(
            &project,
            fixture.directory.path(),
            "open_file",
            Arguments {
                path: Some("main.rs".into()),
                line: Some(2),
                column: Some(4),
                ..Default::default()
            },
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert_eq!(output, "opened tab for main.rs:2:4");

        workspace.update(cx, |workspace, cx| {
            let active_editor = workspace
                .active_item(cx)
                .context("No active item in workspace")?
                .downcast::<editor::Editor>()
                .context("Active item is not an editor")?;
            active_editor.update(cx, |editor, cx| {
                let display_snapshot = editor.display_snapshot(cx);
                let selection = editor.selections.newest::<text::Point>(&display_snapshot);
                assert_eq!(selection.start, text::Point::new(1, 4));
            });
            anyhow::Ok(())
        })?;

        let output = execute(
            &project,
            fixture.directory.path(),
            "open_tab",
            Arguments {
                path: Some("helper.rs".into()),
                ..Default::default()
            },
            &mut buffers,
            &mut async_cx,
        )
        .await?;
        assert_eq!(output, "opened tab for helper.rs");

        workspace.update(cx, |workspace, cx| {
            let active_editor = workspace
                .active_item(cx)
                .context("No active item in workspace")?
                .downcast::<editor::Editor>()
                .context("Active item is not an editor")?;
            let buffer = active_editor
                .read(cx)
                .buffer()
                .read(cx)
                .as_singleton()
                .context("Missing singleton buffer")?;
            let file_path = buffer
                .read(cx)
                .file()
                .context("Missing file")?
                .full_path(cx);
            assert!(file_path.ends_with("helper.rs"));
            anyhow::Ok(())
        })?;

        Ok(())
    }
}
