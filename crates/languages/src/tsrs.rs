use anyhow::{Context as _, Result};
use async_trait::async_trait;
use collections::HashMap;
use futures::{AsyncReadExt, StreamExt};
use gpui::AsyncApp;
use http_client::AsyncBody;
use http_client::github::AssetKind;
use http_client::github_download::download_server_binary;
use language::{
    CodeLabel, Language, LanguageName, LspAdapter, LspAdapterDelegate, LspInstaller, Toolchain,
};
use lsp::{CodeActionKind, LanguageServerBinary, LanguageServerName, Uri};
use project::Fs;
use project::lsp_store::language_server_settings;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use smol::fs;
use std::{
    ffi::OsString,
    future::Future,
    path::PathBuf,
    sync::{Arc, LazyLock},
};
use util::{ResultExt, maybe, merge_json_value_into};

const SERVER_NAME: LanguageServerName = LanguageServerName::new_static("tsrs");

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibcType {
    Gnu,
    Musl,
}

#[cfg(target_os = "linux")]
async fn determine_libc_type() -> LibcType {
    async fn from_ldd_version() -> Option<LibcType> {
        let output = util::command::new_command("ldd")
            .arg("--version")
            .output()
            .await
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        if text.contains("GLIBC") || text.contains("GNU libc") {
            Some(LibcType::Gnu)
        } else if text.contains("musl") {
            Some(LibcType::Musl)
        } else {
            None
        }
    }

    if let Some(libc_type) = from_ldd_version().await {
        return libc_type;
    }

    let Ok(dir_entries) = smol::fs::read_dir("/lib").await else {
        return LibcType::Gnu;
    };
    let dir_entries = dir_entries.filter_map(async move |e| e.ok());
    futures::pin_mut!(dir_entries);

    let mut has_musl = false;
    let mut has_gnu = false;

    while let Some(entry) = dir_entries.next().await {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with("ld-musl-") {
            has_musl = true;
        } else if file_name.starts_with("ld-linux-") {
            has_gnu = true;
        }
    }

    match (has_musl, has_gnu) {
        (true, _) => LibcType::Musl,
        (_, true) => LibcType::Gnu,
        _ => LibcType::Gnu,
    }
}

pub struct TsrsLspAdapter {
    fs: Arc<dyn Fs>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TsrsBinaryVersion {
    pub version: String,
    pub tarball_url: String,
}

impl TsrsLspAdapter {
    pub fn new(fs: Arc<dyn Fs>) -> Self {
        Self { fs }
    }

    #[cfg(target_os = "windows")]
    async fn platform_package_name() -> Result<&'static str> {
        #[cfg(target_arch = "x86_64")]
        {
            Ok("@tsrs/win32-x64")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            anyhow::bail!(
                "Unsupported Windows architecture for tsrs: {}",
                std::env::consts::ARCH
            )
        }
    }

    #[cfg(target_os = "macos")]
    async fn platform_package_name() -> Result<&'static str> {
        match std::env::consts::ARCH {
            "aarch64" => Ok("@tsrs/darwin-arm64"),
            "x86_64" => Ok("@tsrs/darwin-x64"),
            arch => anyhow::bail!("Unsupported macOS architecture for tsrs: {arch}"),
        }
    }

    #[cfg(target_os = "linux")]
    async fn platform_package_name() -> Result<&'static str> {
        let libc = determine_libc_type().await;
        match (std::env::consts::ARCH, libc) {
            ("x86_64", LibcType::Gnu) => Ok("@tsrs/linux-x64-gnu"),
            ("x86_64", LibcType::Musl) => Ok("@tsrs/linux-x64-musl"),
            ("aarch64", LibcType::Gnu) => Ok("@tsrs/linux-arm64-gnu"),
            ("aarch64", LibcType::Musl) => Ok("@tsrs/linux-arm64-musl"),
            (arch, _) => anyhow::bail!("Unsupported Linux architecture for tsrs: {arch}"),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    async fn platform_package_name() -> Result<&'static str> {
        anyhow::bail!(
            "Unsupported operating system for tsrs: {}",
            std::env::consts::OS
        )
    }

    fn binary_name() -> &'static str {
        if cfg!(target_os = "windows") {
            "tsrs.exe"
        } else {
            "tsrs"
        }
    }

    fn binary_arguments() -> Vec<OsString> {
        vec!["--lsp".into(), "-stdio".into()]
    }

    pub fn enhance_diagnostic_message(message: &str) -> Option<String> {
        static SINGLE_WORD_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"'([^\s']*)'").expect("Failed to create REGEX"));

        static MULTI_WORD_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"'([^']+\s+[^']*)'").expect("Failed to create REGEX"));

        let first = SINGLE_WORD_REGEX.replace_all(message, "`$1`").to_string();
        let second = MULTI_WORD_REGEX
            .replace_all(&first, "\n```typescript\n$1\n```\n")
            .to_string();
        Some(second)
    }
}

impl LspInstaller for TsrsLspAdapter {
    type BinaryVersion = TsrsBinaryVersion;

    async fn check_if_user_installed(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: &AsyncApp,
    ) -> Option<LanguageServerBinary> {
        let env = delegate.shell_env().await;

        // 1. Check if bundled in ZedStorm's managed tools directory
        if let Ok(bundled) = http_client::managed_tools::bundled_path(http_client::managed_tools::Tool::Tsrs) {
            if self.fs.is_file(&bundled).await {
                return Some(LanguageServerBinary {
                    path: bundled,
                    arguments: Self::binary_arguments(),
                    env: Some(env),
                });
            }
        }

        // 2. Check if available on PATH (which includes zedstorm-tools)
        if let Some(path) = delegate.which(SERVER_NAME.as_ref()).await {
            return Some(LanguageServerBinary {
                path,
                arguments: Self::binary_arguments(),
                env: Some(env),
            });
        }

        // 3. Check workspace node_modules
        let worktree_root = delegate.worktree_root_path();
        let binary_name = Self::binary_name();
        let possible_workspace_paths = [
            worktree_root
                .join("node_modules")
                .join(".bin")
                .join(binary_name),
            worktree_root.join("node_modules").join(".bin").join("tsrs"),
            worktree_root
                .join("node_modules")
                .join("tsrs")
                .join("tsdk")
                .join(if cfg!(windows) { "tsgo.exe" } else { "tsgo" }),
        ];

        for path in possible_workspace_paths {
            if self.fs.is_file(&path).await {
                return Some(LanguageServerBinary {
                    path,
                    arguments: Self::binary_arguments(),
                    env: Some(env),
                });
            }
        }

        None
    }

    async fn fetch_latest_server_version(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _pre_release: bool,
        _: &mut AsyncApp,
    ) -> Result<Self::BinaryVersion> {
        let package_name = Self::platform_package_name().await?;
        let url = format!("https://registry.npmjs.org/{package_name}/latest");
        let mut response = delegate
            .http_client()
            .get(&url, AsyncBody::default(), true)
            .await
            .with_context(|| format!("fetching latest tsrs version from {url}"))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "failed to fetch tsrs release metadata from {url}: status {}",
                response.status()
            );
        }

        let mut body = String::new();
        response.body_mut().read_to_string(&mut body).await?;

        #[derive(Deserialize)]
        struct NpmLatestResponse {
            version: String,
            dist: NpmDist,
        }

        #[derive(Deserialize)]
        struct NpmDist {
            tarball: String,
        }

        let parsed: NpmLatestResponse = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse npm response from {url}"))?;

        Ok(TsrsBinaryVersion {
            version: parsed.version,
            tarball_url: parsed.dist.tarball,
        })
    }

    fn check_if_version_installed(
        &self,
        version: &Self::BinaryVersion,
        container_dir: &PathBuf,
        _: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Option<LanguageServerBinary>> + use<> {
        let bundled = http_client::managed_tools::bundled_path(http_client::managed_tools::Tool::Tsrs).ok();
        let destination = container_dir.join(format!("tsrs-{}", version.version));
        let binary_path = destination.join("package").join(Self::binary_name());
        async move {
            if let Some(bundled) = bundled {
                if fs::metadata(&bundled).await.is_ok() {
                    return Some(LanguageServerBinary {
                        path: bundled,
                        env: None,
                        arguments: Self::binary_arguments(),
                    });
                }
            }
            if fs::metadata(&binary_path).await.is_ok() {
                Some(LanguageServerBinary {
                    path: binary_path,
                    env: None,
                    arguments: Self::binary_arguments(),
                })
            } else {
                None
            }
        }
    }

    fn fetch_server_binary(
        &self,
        latest_version: Self::BinaryVersion,
        container_dir: PathBuf,
        delegate: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Result<LanguageServerBinary>> + use<> {
        let delegate = delegate.clone();
        async move {
            // First try ZedStorm's managed tools infrastructure
            let http = delegate.http_client();
            match http_client::managed_tools::ensure_tool(
                http_client::managed_tools::Tool::Tsrs,
                http.clone(),
            )
            .await
            {
                Ok(binary_path) => {
                    return Ok(LanguageServerBinary {
                        path: binary_path,
                        env: None,
                        arguments: Self::binary_arguments(),
                    });
                }
                Err(err) => {
                    log::warn!(
                        "Could not install tsrs via managed tools: {err:#}. Falling back to container install"
                    );
                }
            }

            let destination = container_dir.join(format!("tsrs-{}", latest_version.version));
            let binary_path = destination.join("package").join(Self::binary_name());
            if fs::metadata(&binary_path).await.is_ok() {
                return Ok(LanguageServerBinary {
                    path: binary_path,
                    env: None,
                    arguments: Self::binary_arguments(),
                });
            }

            download_server_binary(
                &*http,
                &latest_version.tarball_url,
                None,
                &destination,
                AssetKind::TarGz,
            )
            .await?;

            #[cfg(unix)]
            util::fs::make_file_executable(&binary_path).await?;

            Ok(LanguageServerBinary {
                path: binary_path,
                env: None,
                arguments: Self::binary_arguments(),
            })
        }
    }

    async fn cached_server_binary(
        &self,
        container_dir: PathBuf,
        _: &dyn LspAdapterDelegate,
    ) -> Option<LanguageServerBinary> {
        if let Ok(bundled) = http_client::managed_tools::bundled_path(http_client::managed_tools::Tool::Tsrs) {
            if fs::metadata(&bundled).await.is_ok() {
                return Some(LanguageServerBinary {
                    path: bundled,
                    env: None,
                    arguments: Self::binary_arguments(),
                });
            }
        }
        get_cached_server_binary(container_dir).await
    }
}

async fn get_cached_server_binary(container_dir: PathBuf) -> Option<LanguageServerBinary> {
    maybe!(async {
        let mut entries = fs::read_dir(&container_dir)
            .await
            .with_context(|| format!("listing {container_dir:?}"))?;
        let mut versions = Vec::new();
        while let Some(entry) = entries.next().await {
            let entry = entry?;
            let path = entry.path();
            let binary_path = path.join("package").join(TsrsLspAdapter::binary_name());
            if fs::metadata(&binary_path).await.is_ok() {
                versions.push(binary_path);
            }
        }
        versions.sort();
        let binary_path = versions.pop().context("no cached tsrs binary found")?;
        anyhow::Ok(LanguageServerBinary {
            path: binary_path,
            env: None,
            arguments: TsrsLspAdapter::binary_arguments(),
        })
    })
    .await
    .log_err()
}

#[async_trait(?Send)]
impl LspAdapter for TsrsLspAdapter {
    fn name(&self) -> LanguageServerName {
        SERVER_NAME
    }

    fn language_ids(&self) -> HashMap<LanguageName, String> {
        HashMap::from_iter([
            (LanguageName::new_static("TypeScript"), "typescript".into()),
            (LanguageName::new_static("JavaScript"), "javascript".into()),
            (LanguageName::new_static("TSX"), "typescriptreact".into()),
        ])
    }

    fn code_action_kinds(&self) -> Option<Vec<CodeActionKind>> {
        Some(vec![
            CodeActionKind::QUICKFIX,
            CodeActionKind::REFACTOR,
            CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
        ])
    }

    async fn label_for_completion(
        &self,
        item: &lsp::CompletionItem,
        language: &Arc<Language>,
    ) -> Option<CodeLabel> {
        use lsp::CompletionItemKind as Kind;
        let label_len = item.label.len();
        let grammar = language.grammar()?;
        let highlight_id = match item.kind? {
            Kind::CLASS | Kind::INTERFACE | Kind::ENUM => grammar.highlight_id_for_name("type"),
            Kind::CONSTRUCTOR => grammar.highlight_id_for_name("type"),
            Kind::CONSTANT => grammar.highlight_id_for_name("constant"),
            Kind::FUNCTION | Kind::METHOD => grammar.highlight_id_for_name("function"),
            Kind::PROPERTY | Kind::FIELD => grammar.highlight_id_for_name("property"),
            Kind::VARIABLE => grammar.highlight_id_for_name("variable"),
            _ => None,
        }?;

        let text = if let Some(description) = item
            .label_details
            .as_ref()
            .and_then(|label_details| label_details.description.as_ref())
        {
            format!("{} {}", item.label, description)
        } else if let Some(detail) = &item.detail {
            format!("{} {}", item.label, detail)
        } else {
            item.label.clone()
        };
        Some(CodeLabel::filtered(
            text,
            label_len,
            item.filter_text.as_deref(),
            vec![(0..label_len, highlight_id)],
        ))
    }

    async fn workspace_configuration(
        self: Arc<Self>,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: Option<Uri>,
        cx: &mut AsyncApp,
    ) -> Result<Value> {
        let inlays = serde_json::json!({
            "includeInlayParameterNameHints": "all",
            "includeInlayParameterNameHintsWhenArgumentMatchesName": false,
            "includeInlayFunctionParameterTypeHints": true,
            "includeInlayVariableTypeHints": true,
            "includeInlayVariableTypeHintsWhenTypeMatchesName": true,
            "includeInlayPropertyDeclarationTypeHints": true,
            "includeInlayFunctionLikeReturnTypeHints": true,
            "includeInlayEnumMemberValueHints": true,
            "referencesCodeLens": {
                "enabled": true,
                "showOnAllFunctions": true
            },
            "implementationsCodeLens": {
                "enabled": true,
                "showOnAllClassMethods": true
            }
        });

        let mut default_workspace_configuration = serde_json::json!({
            "js/ts": inlays,
            "typescript": {
                "inlayHints": {
                    "parameterNames": {
                        "enabled": "all",
                        "suppressWhenArgumentMatchesName": false
                    },
                    "parameterTypes": { "enabled": true },
                    "variableTypes": {
                        "enabled": true,
                        "suppressWhenTypeMatchesName": false
                    },
                    "propertyDeclarationTypes": { "enabled": true },
                    "functionLikeReturnTypes": { "enabled": true },
                    "enumMemberValues": { "enabled": true }
                },
                "implementationsCodeLens": { "enabled": true, "showOnAllClassMethods": true },
                "referencesCodeLens": { "enabled": true, "showOnAllFunctions": true }
            },
            "javascript": {
                "inlayHints": {
                    "parameterNames": {
                        "enabled": "all",
                        "suppressWhenArgumentMatchesName": false
                    },
                    "parameterTypes": { "enabled": true },
                    "variableTypes": {
                        "enabled": true,
                        "suppressWhenTypeMatchesName": false
                    },
                    "propertyDeclarationTypes": { "enabled": true },
                    "functionLikeReturnTypes": { "enabled": true },
                    "enumMemberValues": { "enabled": true }
                },
                "implementationsCodeLens": { "enabled": true, "showOnAllClassMethods": true },
                "referencesCodeLens": { "enabled": true, "showOnAllFunctions": true }
            }
        });

        let override_options = cx.update(|cx| {
            language_server_settings(delegate.as_ref(), &SERVER_NAME, cx)
                .and_then(|s| s.settings.clone())
        });

        if let Some(override_options) = override_options {
            merge_json_value_into(override_options, &mut default_workspace_configuration);
        }

        Ok(default_workspace_configuration)
    }

    fn diagnostic_message_to_markdown(&self, message: &str) -> Option<String> {
        Self::enhance_diagnostic_message(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_message_to_markdown() {
        // Leaves simple messages unchanged
        let message = "The expected type comes from the return type of this signature.";
        let expected = "The expected type comes from the return type of this signature.";
        assert_eq!(
            TsrsLspAdapter::enhance_diagnostic_message(message).expect("Should be some"),
            expected
        );

        // Parses both multi-word and single-word correctly
        let message = "Property 'baz' is missing in type '{ foo: string; bar: string; }' but required in type 'User'.";
        let expected = "Property `baz` is missing in type \n```typescript\n{ foo: string; bar: string; }\n```\n but required in type `User`.";
        assert_eq!(
            TsrsLspAdapter::enhance_diagnostic_message(message).expect("Should be some"),
            expected
        );

        // Parses multi-and-single word in any order, and ignores existing newlines
        let message = "Type '() => { foo: string; bar: string; }' is not assignable to type 'GetUserFunction'.\n  Property 'baz' is missing in type '{ foo: string; bar: string; }' but required in type 'User'.";
        let expected = "Type \n```typescript\n() => { foo: string; bar: string; }\n```\n is not assignable to type `GetUserFunction`.\n  Property `baz` is missing in type \n```typescript\n{ foo: string; bar: string; }\n```\n but required in type `User`.";
        assert_eq!(
            TsrsLspAdapter::enhance_diagnostic_message(message).expect("Should be some"),
            expected
        );
    }

    #[test]
    fn test_tsrs_server_name_and_args() {
        assert_eq!(SERVER_NAME.0.as_ref(), "tsrs");
        assert_eq!(
            TsrsLspAdapter::binary_arguments(),
            vec![OsString::from("--lsp"), OsString::from("-stdio")]
        );
    }
}
