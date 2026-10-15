use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use crate::registry::ExtensionProvides;
use collections::{BTreeMap, BTreeSet, HashMap};
use fs::Fs;
use language::LanguageName;
use lsp::LanguageServerName;
use semver::Version;
use serde::{Deserialize, Serialize};
use util::paths::PathStyle;
use util::rel_path::{RelPath, RelPathBuf};

use crate::ExtensionCapability;

/// This is the old version of the extension manifest, from when it was `extension.json`.
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct OldExtensionManifest {
    pub name: String,
    pub version: Arc<str>,

    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,

    #[serde(default)]
    pub themes: BTreeMap<Arc<str>, RelPathBuf>,
    #[serde(default)]
    pub languages: BTreeMap<Arc<str>, RelPathBuf>,
    #[serde(default)]
    pub grammars: BTreeMap<Arc<str>, RelPathBuf>,
}

/// The schema version of the [`ExtensionManifest`].
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct SchemaVersion(pub i32);

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl SchemaVersion {
    pub const ZERO: Self = Self(0);
    /// The latest schema version supported by Zed.
    pub const CURRENT: Self = Self(1);

    pub fn is_v0(&self) -> bool {
        self == &Self::ZERO
    }
}

// TODO: We should change this to just always be a Vec<PathBuf> once we bump the
// extension.toml schema version to 2
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionSnippets {
    Single(PathBuf),
    Multiple(Vec<PathBuf>),
}

impl ExtensionSnippets {
    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        match self {
            ExtensionSnippets::Single(path) => std::slice::from_ref(path).iter(),
            ExtensionSnippets::Multiple(paths) => paths.iter(),
        }
    }
}

impl From<&str> for ExtensionSnippets {
    fn from(value: &str) -> Self {
        ExtensionSnippets::Single(value.into())
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct TabRendererManifestEntry {
    #[serde(default)]
    pub file_types: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: Arc<str>,
    pub name: String,
    pub version: Arc<str>,
    pub schema_version: SchemaVersion,

    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub lib: LibManifestEntry,

    #[serde(default)]
    pub themes: Vec<RelPathBuf>,
    #[serde(default)]
    pub icon_themes: Vec<RelPathBuf>,
    #[serde(default)]
    pub languages: Vec<RelPathBuf>,
    #[serde(default)]
    pub grammars: BTreeMap<Arc<str>, GrammarManifestEntry>,
    #[serde(default)]
    pub language_servers: BTreeMap<LanguageServerName, LanguageServerManifestEntry>,
    #[serde(default)]
    pub context_servers: BTreeMap<Arc<str>, ContextServerManifestEntry>,
    #[serde(default)]
    pub slash_commands: BTreeMap<Arc<str>, SlashCommandManifestEntry>,
    #[serde(default)]
    pub snippets: Option<ExtensionSnippets>,
    #[serde(default)]
    pub capabilities: Vec<ExtensionCapability>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub debug_adapters: BTreeMap<Arc<str>, DebugAdapterManifestEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub debug_locators: BTreeMap<Arc<str>, DebugLocatorManifestEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub language_model_providers: BTreeMap<Arc<str>, LanguageModelProviderManifestEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagram_renderers: Vec<Arc<str>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tab_renderers: BTreeMap<Arc<str>, TabRendererManifestEntry>,
}

impl ExtensionManifest {
    /// Returns the set of features provided by the extension.
    pub fn provides(&self) -> BTreeSet<ExtensionProvides> {
        let mut provides = BTreeSet::default();
        if !self.themes.is_empty() {
            provides.insert(ExtensionProvides::Themes);
        }

        if !self.icon_themes.is_empty() {
            provides.insert(ExtensionProvides::IconThemes);
        }

        if !self.languages.is_empty() {
            provides.insert(ExtensionProvides::Languages);
        }

        if !self.grammars.is_empty() {
            provides.insert(ExtensionProvides::Grammars);
        }

        if !self.language_servers.is_empty() {
            provides.insert(ExtensionProvides::LanguageServers);
        }

        if !self.context_servers.is_empty() {
            provides.insert(ExtensionProvides::ContextServers);
        }

        if self.snippets.is_some() {
            provides.insert(ExtensionProvides::Snippets);
        }

        if !self.debug_adapters.is_empty() {
            provides.insert(ExtensionProvides::DebugAdapters);
        }

        provides
    }

    pub fn allow_exec(
        &self,
        desired_command: &str,
        desired_args: &[impl AsRef<str> + std::fmt::Debug],
    ) -> Result<()> {
        let is_allowed = self.capabilities.iter().any(|capability| match capability {
            ExtensionCapability::ProcessExec(capability) => {
                capability.allows(desired_command, desired_args)
            }
            _ => false,
        });

        if !is_allowed {
            bail!(
                "capability for process:exec {desired_command} {desired_args:?} was not listed in the extension manifest",
            );
        }

        Ok(())
    }

    pub fn allow_remote_load(&self) -> bool {
        self.remote_load().is_some()
    }

    pub fn remote_load(&self) -> Option<RemoteLoad<'_>> {
        (!self.language_servers.is_empty()
            || !self.debug_adapters.is_empty()
            || !self.debug_locators.is_empty())
        .then_some(RemoteLoad { manifest: self })
    }
}

pub struct RemoteLoad<'a> {
    manifest: &'a ExtensionManifest,
}

impl RemoteLoad<'_> {
    pub fn language_dependencies(&self) -> impl Iterator<Item = LanguageName> + '_ {
        self.manifest
            .language_servers
            .values()
            .flat_map(|language_server_config| language_server_config.languages())
    }
}

pub fn build_debug_adapter_schema_path(
    adapter_name: &Arc<str>,
    meta: &DebugAdapterManifestEntry,
) -> anyhow::Result<RelPathBuf> {
    match &meta.schema_path {
        Some(path) => Ok(path.clone()),
        None => RelPath::new(
            &Path::new("debug_adapter_schemas")
                .join(Path::new(adapter_name.as_ref()).with_extension("json")),
            PathStyle::local(),
        )
        .map(Cow::into_owned),
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct LibManifestEntry {
    pub kind: Option<ExtensionLibraryKind>,
    pub version: Option<Version>,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub enum ExtensionLibraryKind {
    Rust,
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct GrammarManifestEntry {
    pub repository: String,
    #[serde(alias = "commit")]
    pub rev: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct LanguageServerManifestEntry {
    /// Deprecated in favor of `languages`.
    #[serde(default)]
    language: Option<LanguageName>,
    /// The list of languages this language server should work with.
    #[serde(default)]
    languages: Vec<LanguageName>,
    #[serde(default)]
    pub language_ids: HashMap<LanguageName, String>,
    #[serde(default)]
    pub code_action_kinds: Option<Vec<lsp::CodeActionKind>>,
    /// Languages (from `languages`) for which this language server is not started
    /// unless the user explicitly lists it in their `language_servers` setting.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    opt_in_languages: BTreeSet<LanguageName>,
}

impl LanguageServerManifestEntry {
    /// Returns the list of languages for the language server.
    ///
    /// Prefer this over accessing the `language` or `languages` fields directly,
    /// as we currently support both.
    ///
    /// We can replace this with just field access for the `languages` field once
    /// we have removed `language`.
    pub fn languages(&self) -> impl IntoIterator<Item = LanguageName> + '_ {
        let language = if self.languages.is_empty() {
            self.language.clone()
        } else {
            None
        };
        self.languages.iter().cloned().chain(language)
    }

    /// Returns the languages for which the language server is disabled by default.
    pub fn opt_in_languages(&self) -> &BTreeSet<LanguageName> {
        &self.opt_in_languages
    }

    /// Returns whether the language server should only be started for the given
    /// language when the user explicitly enables it.
    pub fn is_opt_in_for(&self, language: &LanguageName) -> bool {
        self.opt_in_languages.contains(language)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct ContextServerManifestEntry {}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct SlashCommandManifestEntry {
    pub description: String,
    pub requires_argument: bool,
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct DebugAdapterManifestEntry {
    pub schema_path: Option<RelPathBuf>,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct DebugLocatorManifestEntry {}

/// Manifest entry for a language model provider.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct LanguageModelProviderManifestEntry {
    /// Display name for the provider.
    pub name: String,
    /// Path to an SVG icon file relative to the extension root (e.g., "icons/provider.svg").
    #[serde(default)]
    pub icon: Option<String>,
}

impl ExtensionManifest {
    pub async fn load(fs: Arc<dyn Fs>, extension_dir: &Path) -> Result<Self> {
        let extension_name = extension_dir
            .file_name()
            .and_then(OsStr::to_str)
            .context("invalid extension name")?;

        let extension_manifest_path = extension_dir.join("extension.toml");
        if fs.is_file(&extension_manifest_path).await {
            let manifest_content = fs.load(&extension_manifest_path).await.with_context(|| {
                format!("loading {extension_name} extension.toml, {extension_manifest_path:?}")
            })?;
            toml::from_str(&manifest_content).map_err(|err| {
                anyhow!("Invalid extension.toml for extension {extension_name}:\n{err}")
            })
        } else if let extension_manifest_path = extension_manifest_path.with_extension("json")
            && fs.is_file(&extension_manifest_path).await
        {
            let manifest_content = fs.load(&extension_manifest_path).await.with_context(|| {
                format!("loading {extension_name} extension.json, {extension_manifest_path:?}")
            })?;

            serde_json::from_str::<OldExtensionManifest>(&manifest_content)
                .with_context(|| format!("invalid extension.json for extension {extension_name}"))
                .map(|manifest_json| manifest_from_old_manifest(manifest_json, extension_name))
        } else {
            anyhow::bail!("No extension manifest found for extension {extension_name}")
        }
    }
}

fn manifest_from_old_manifest(
    manifest_json: OldExtensionManifest,
    extension_id: &str,
) -> ExtensionManifest {
    ExtensionManifest {
        id: extension_id.into(),
        name: manifest_json.name,
        version: manifest_json.version,
        description: manifest_json.description,
        repository: manifest_json.repository,
        authors: manifest_json.authors,
        schema_version: SchemaVersion::ZERO,
        lib: Default::default(),
        themes: {
            let mut themes = manifest_json.themes.into_values().collect::<Vec<_>>();
            themes.sort_unstable();
            themes.dedup();
            themes
        },
        icon_themes: Vec::new(),
        languages: {
            let mut languages = manifest_json.languages.into_values().collect::<Vec<_>>();
            languages.sort_unstable();
            languages.dedup();
            languages
        },
        grammars: manifest_json
            .grammars
            .into_keys()
            .map(|grammar_name| (grammar_name, Default::default()))
            .collect(),
        language_servers: Default::default(),
        context_servers: BTreeMap::default(),
        slash_commands: BTreeMap::default(),
        snippets: None,
        capabilities: Vec::new(),
        debug_adapters: Default::default(),
        debug_locators: Default::default(),
        language_model_providers: Default::default(),
        diagram_renderers: Vec::new(),
        tab_renderers: BTreeMap::default(),
    }
}

#[cfg(test)]
mod tests {

    use util::rel_path::rel_path_buf;

    use super::*;

    #[allow(dead_code)]
    fn extension_manifest() -> ExtensionManifest {
        ExtensionManifest {
            id: "test".into(),
            name: "Test".to_string(),
            version: "1.0.0".into(),
            schema_version: SchemaVersion::ZERO,
            description: None,
            repository: None,
            authors: vec![],
            lib: Default::default(),
            themes: vec![],
            icon_themes: vec![],
            languages: vec![],
            grammars: BTreeMap::default(),
            language_servers: BTreeMap::default(),
            context_servers: BTreeMap::default(),
            slash_commands: BTreeMap::default(),
            snippets: None,
            capabilities: vec![],
            debug_adapters: Default::default(),
            debug_locators: Default::default(),
            language_model_providers: BTreeMap::default(),
            diagram_renderers: Vec::new(),
            tab_renderers: BTreeMap::default(),
        }
    }

    #[test]
    fn test_build_adapter_schema_path_with_schema_path() {
        let adapter_name = Arc::from("my_adapter");
        let entry = DebugAdapterManifestEntry {
            schema_path: Some(rel_path_buf("foo/bar")),
        };

        let path = build_debug_adapter_schema_path(&adapter_name, &entry).unwrap();
        assert_eq!(path, rel_path_buf("foo/bar"));
    }

    #[test]
    fn test_build_adapter_schema_path_without_schema_path() {
        let adapter_name = Arc::from("my_adapter");
        let entry = DebugAdapterManifestEntry::default();

        let path = build_debug_adapter_schema_path(&adapter_name, &entry).unwrap();
        assert_eq!(path, rel_path_buf("debug_adapter_schemas/my_adapter.json"));
    }
}
