mod capabilities;
pub mod extension_builder;
mod extension_events;
mod extension_host_proxy;
mod extension_manifest;
mod types;
pub mod registry;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ::lsp::LanguageServerName;
use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use collections::BTreeMap;
use futures::future::BoxFuture;
use gpui::{App, BorrowAppContext as _, EntityId, Global, Task};
use language::LanguageName;
use semver::Version;
use task::{SpawnInTerminal, ZedDebugConfig};
use util::rel_path::RelPath;

pub use crate::capabilities::*;
pub use crate::extension_events::*;
pub use crate::extension_host_proxy::*;
pub use crate::extension_manifest::*;
pub use crate::registry::*;
pub use crate::types::*;

/// Initializes the `extension` crate.
pub fn init(cx: &mut App) {
    extension_events::init(cx);
    ExtensionHostProxy::default_global(cx);
    cx.default_global::<DiagramRendererRegistry>();
    cx.default_global::<TabRendererRegistry>();
}

pub type DiagramRenderer =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<String>> + Send + Sync>;

#[derive(Default)]
pub struct DiagramRendererRegistry {
    renderers: BTreeMap<Arc<str>, BTreeMap<Arc<str>, DiagramRenderer>>,
}

impl Global for DiagramRendererRegistry {}

impl DiagramRendererRegistry {
    pub fn renderer(id: &str, cx: &App) -> Option<DiagramRenderer> {
        cx.try_global::<Self>()?
            .renderers
            .get(id)
            .and_then(|renderers| renderers.values().next().cloned())
    }

    pub fn register(
        extension_id: Arc<str>,
        renderer_id: Arc<str>,
        renderer: DiagramRenderer,
        cx: &mut App,
    ) {
        cx.default_global::<Self>();
        cx.update_global::<Self, _>(|registry, _| {
            registry
                .renderers
                .entry(renderer_id)
                .or_default()
                .insert(extension_id, renderer);
        });
    }

    pub fn unregister(extension_id: &str, cx: &mut App) {
        if !cx.try_global::<Self>().is_some_and(|registry| {
            registry
                .renderers
                .values()
                .any(|renderers| renderers.contains_key(extension_id))
        }) {
            return;
        }
        cx.update_global::<Self, _>(|registry, _| {
            registry.renderers.retain(|_, renderers| {
                renderers.remove(extension_id);
                !renderers.is_empty()
            });
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionTabRendererCommand {
    pub path: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionTabRendererWebEngine {
    pub url: String,
    pub html: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionTabRendererBackend {
    Command(ExtensionTabRendererCommand),
    WebEngine(ExtensionTabRendererWebEngine),
}

pub type TabRendererCallback = Arc<
    dyn Fn(PathBuf, &App) -> BoxFuture<'static, Result<ExtensionTabRendererBackend>>
        + Send
        + Sync,
>;

#[derive(Default)]
pub struct TabRendererRegistry {
    renderers: BTreeMap<Arc<str>, BTreeMap<Arc<str>, (Vec<String>, TabRendererCallback)>>,
}

impl Global for TabRendererRegistry {}

impl TabRendererRegistry {
    pub fn renderer_for_path(
        path: &Path,
        cx: &App,
    ) -> Option<(Arc<str>, Arc<str>, TabRendererCallback)> {
        let extension = path.extension()?.to_str()?;
        let registry = cx.try_global::<Self>()?;
        for (extension_id, renderers) in &registry.renderers {
            for (renderer_id, (file_types, callback)) in renderers {
                if file_types.iter().any(|ft| ft.eq_ignore_ascii_case(extension)) {
                    return Some((extension_id.clone(), renderer_id.clone(), callback.clone()));
                }
            }
        }
        None
    }

    pub fn register(
        extension_id: Arc<str>,
        renderer_id: Arc<str>,
        file_types: Vec<String>,
        renderer: TabRendererCallback,
        cx: &mut App,
    ) {
        cx.default_global::<Self>();
        cx.update_global::<Self, _>(|registry, _| {
            registry
                .renderers
                .entry(extension_id)
                .or_default()
                .insert(renderer_id, (file_types, renderer));
        });
    }

    pub fn unregister(extension_id: &str, cx: &mut App) {
        if !cx.try_global::<Self>().is_some_and(|registry| {
            registry.renderers.contains_key(extension_id)
        }) {
            return;
        }
        cx.update_global::<Self, _>(|registry, _| {
            registry.renderers.remove(extension_id);
        });
    }
}

#[async_trait]
pub trait WorktreeDelegate: Send + Sync + 'static {
    fn id(&self) -> u64;
    fn root_path(&self) -> String;
    async fn read_text_file(&self, path: &RelPath) -> Result<String>;
    async fn which(&self, binary_name: String) -> Option<String>;
    async fn shell_env(&self) -> Vec<(String, String)>;
}

pub struct StandaloneWorktreeDelegate {
    pub root: PathBuf,
}

#[async_trait]
impl WorktreeDelegate for StandaloneWorktreeDelegate {
    fn id(&self) -> u64 {
        0
    }

    fn root_path(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    async fn read_text_file(&self, path: &RelPath) -> Result<String> {
        let full = self.root.join(path.as_std_path());
        std::fs::read_to_string(full).map_err(Into::into)
    }

    async fn which(&self, binary_name: String) -> Option<String> {
        which::which(binary_name)
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    }

    async fn shell_env(&self) -> Vec<(String, String)> {
        std::env::vars().collect()
    }
}

pub trait ProjectDelegate: Send + Sync + 'static {
    fn worktree_ids(&self) -> Vec<u64>;
}

pub trait KeyValueStoreDelegate: Send + Sync + 'static {
    fn insert(&self, key: String, docs: String) -> Task<Result<()>>;
}

/// Strips the leading slash WASI puts on Windows drive paths (`/C:/dir`), which std would
/// otherwise treat as a relative path. Text that is not of the form `/X:/` or `/X:\` is
/// returned unchanged.
pub fn strip_wasi_drive_prefix(text: &str) -> &str {
    match text.as_bytes() {
        [b'/', drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic() => &text[1..],
        _ => text,
    }
}

/// Resolves a path returned by an extension to a path on the host.
///
/// Relative paths prefer the installed extension directory, where companion binaries ship in
/// `bin/`, over the working directory. Extensions built against older layouts return absolute
/// paths into the `zedstorm-tools` directory, which is no longer populated, so a missing path of
/// that shape falls back to the file of the same name inside the installed `bin/` directory.
pub fn resolve_extension_path(
    path: &Path,
    extension_dir: Option<&Path>,
    work_dir: &Path,
) -> PathBuf {
    let path = match path.to_str() {
        Some(text) if cfg!(windows) => Path::new(strip_wasi_drive_prefix(text)),
        _ => path,
    };
    let resolved = if path.is_relative() {
        extension_dir
            .map(|extension_dir| extension_dir.join(path))
            .filter(|candidate| candidate.exists())
            .unwrap_or_else(|| work_dir.join(path))
    } else {
        path.to_path_buf()
    };
    if path.is_absolute()
        && !resolved.exists()
        && path
            .components()
            .any(|component| component.as_os_str() == "zedstorm-tools")
        && let Some(extension_dir) = extension_dir
        && let Some(file_name) = path.file_name()
    {
        let bundled = extension_dir.join("bin").join(file_name);
        if bundled.is_file() {
            return util::normalize_path(&bundled);
        }
    }
    util::normalize_path(&resolved)
}

#[async_trait]
pub trait Extension: Send + Sync + 'static {
    async fn render_diagram(
        &self,
        _renderer_id: Arc<str>,
        _source: String,
        _theme: String,
    ) -> Result<String> {
        bail!("diagram rendering is not supported by this extension")
    }

    async fn tab_renderer_backend(
        &self,
        _renderer_id: Arc<str>,
        _file_path: String,
        _worktree: Arc<dyn WorktreeDelegate>,
    ) -> Result<ExtensionTabRendererBackend> {
        bail!("tab rendering is not supported by this extension")
    }

    /// Returns the [`ExtensionManifest`] for this extension.
    fn manifest(&self) -> Arc<ExtensionManifest>;

    /// Returns the path to this extension's working directory.
    fn work_dir(&self) -> Arc<Path>;

    /// Returns the path to this extension's installed directory, if available.
    fn extension_dir(&self) -> Option<Arc<Path>> {
        None
    }

    /// Returns a path relative to this extension's working or installed directory.
    fn path_from_extension(&self, path: &Path) -> PathBuf {
        resolve_extension_path(path, self.extension_dir().as_deref(), &self.work_dir())
    }

    async fn language_server_command(
        &self,
        language_server_id: LanguageServerName,
        language_name: LanguageName,
        worktree: Arc<dyn WorktreeDelegate>,
        language_server_status_source: EntityId,
    ) -> Result<Command>;

    async fn language_server_initialization_options(
        &self,
        language_server_id: LanguageServerName,
        language_name: LanguageName,
        worktree: Arc<dyn WorktreeDelegate>,
        language_server_status_source: EntityId,
    ) -> Result<Option<String>>;

    async fn language_server_workspace_configuration(
        &self,
        language_server_id: LanguageServerName,
        worktree: Arc<dyn WorktreeDelegate>,
        language_server_status_source: EntityId,
    ) -> Result<Option<String>>;

    async fn language_server_initialization_options_schema(
        &self,
        language_server_id: LanguageServerName,
        worktree: Arc<dyn WorktreeDelegate>,
        language_server_status_source: EntityId,
    ) -> Result<Option<String>>;

    async fn language_server_workspace_configuration_schema(
        &self,
        language_server_id: LanguageServerName,
        worktree: Arc<dyn WorktreeDelegate>,
        language_server_status_source: EntityId,
    ) -> Result<Option<String>>;

    async fn language_server_additional_initialization_options(
        &self,
        language_server_id: LanguageServerName,
        target_language_server_id: LanguageServerName,
        worktree: Arc<dyn WorktreeDelegate>,
        language_server_status_source: EntityId,
    ) -> Result<Option<String>>;

    async fn language_server_additional_workspace_configuration(
        &self,
        language_server_id: LanguageServerName,
        target_language_server_id: LanguageServerName,
        worktree: Arc<dyn WorktreeDelegate>,
        language_server_status_source: EntityId,
    ) -> Result<Option<String>>;

    async fn labels_for_completions(
        &self,
        language_server_id: LanguageServerName,
        completions: Vec<Completion>,
    ) -> Result<Vec<Option<CodeLabel>>>;

    async fn labels_for_symbols(
        &self,
        language_server_id: LanguageServerName,
        symbols: Vec<Symbol>,
    ) -> Result<Vec<Option<CodeLabel>>>;

    async fn complete_slash_command_argument(
        &self,
        command: SlashCommand,
        arguments: Vec<String>,
    ) -> Result<Vec<SlashCommandArgumentCompletion>>;

    async fn run_slash_command(
        &self,
        command: SlashCommand,
        arguments: Vec<String>,
        worktree: Option<Arc<dyn WorktreeDelegate>>,
    ) -> Result<SlashCommandOutput>;

    async fn context_server_command(
        &self,
        context_server_id: Arc<str>,
        project: Arc<dyn ProjectDelegate>,
    ) -> Result<Command>;

    async fn context_server_configuration(
        &self,
        context_server_id: Arc<str>,
        project: Arc<dyn ProjectDelegate>,
    ) -> Result<Option<ContextServerConfiguration>>;

    async fn suggest_docs_packages(&self, provider: Arc<str>) -> Result<Vec<String>>;

    async fn index_docs(
        &self,
        provider: Arc<str>,
        package_name: Arc<str>,
        kv_store: Arc<dyn KeyValueStoreDelegate>,
    ) -> Result<()>;

    async fn get_dap_binary(
        &self,
        dap_name: Arc<str>,
        config: DebugTaskDefinition,
        user_installed_path: Option<PathBuf>,
        worktree: Arc<dyn WorktreeDelegate>,
    ) -> Result<DebugAdapterBinary>;

    async fn dap_request_kind(
        &self,
        dap_name: Arc<str>,
        config: serde_json::Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest>;

    async fn dap_config_to_scenario(&self, config: ZedDebugConfig) -> Result<DebugScenario>;

    async fn dap_locator_create_scenario(
        &self,
        locator_name: String,
        build_config_template: BuildTaskTemplate,
        resolved_label: String,
        debug_adapter_name: String,
    ) -> Result<Option<DebugScenario>>;
    async fn run_dap_locator(
        &self,
        locator_name: String,
        config: SpawnInTerminal,
    ) -> Result<DebugRequest>;
}

pub fn parse_wasm_extension_version(extension_id: &str, wasm_bytes: &[u8]) -> Result<Version> {
    let mut version = None;

    for part in wasmparser::Parser::new(0).parse_all(wasm_bytes) {
        if let wasmparser::Payload::CustomSection(s) =
            part.context("error parsing wasm extension")?
        {
            if s.name() == "zed:api-version" {
                let data = s.data();
                if data.len() == 6 {
                    version = Some(Version::new(
                        u16::from_be_bytes([data[0], data[1]]) as _,
                        u16::from_be_bytes([data[2], data[3]]) as _,
                        u16::from_be_bytes([data[4], data[5]]) as _,
                    ));
                } else {
                    version = Some(Version::parse(std::str::from_utf8(data)?)?);
                }
            }
        }
    }

    version.ok_or_else(|| anyhow!("extension {extension_id} does not specify an api version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_extension_path() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let extension_dir = root.path().join("installed").join("drawio");
        let work_dir = root.path().join("work").join("drawio");
        std::fs::create_dir_all(extension_dir.join("bin"))?;
        std::fs::create_dir_all(&work_dir)?;
        let binary = extension_dir.join("bin").join("zed-diagrams");
        std::fs::write(&binary, "")?;
        let expected = util::normalize_path(&binary);

        assert_eq!(
            resolve_extension_path(Path::new("bin/zed-diagrams"), Some(&extension_dir), &work_dir),
            expected
        );

        let stale_tools_path = root.path().join("zedstorm-tools").join("bin").join("zed-diagrams");
        assert_eq!(
            resolve_extension_path(
                &stale_tools_path,
                Some(&extension_dir),
                &work_dir
            ),
            expected
        );

        #[cfg(windows)]
        assert_eq!(
            resolve_extension_path(
                Path::new(&format!("/{}", binary.to_string_lossy().replace('\\', "/"))),
                Some(&extension_dir),
                &work_dir
            ),
            expected
        );

        assert_eq!(
            resolve_extension_path(Path::new("bin/missing"), Some(&extension_dir), &work_dir),
            util::normalize_path(&work_dir.join("bin").join("missing"))
        );

        let unrelated_missing = root.path().join("elsewhere").join("zed-diagrams");
        assert_eq!(
            resolve_extension_path(&unrelated_missing, Some(&extension_dir), &work_dir),
            util::normalize_path(&unrelated_missing)
        );

        assert_eq!(strip_wasi_drive_prefix("/C:/dir"), "C:/dir");
        assert_eq!(strip_wasi_drive_prefix("/C:\\dir"), "C:\\dir");
        assert_eq!(strip_wasi_drive_prefix("/C:dir"), "/C:dir");
        assert_eq!(strip_wasi_drive_prefix("/usr/bin"), "/usr/bin");
        Ok(())
    }
}
