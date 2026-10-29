use anyhow::{Context as _, Result};
use async_trait::async_trait;
use futures::StreamExt;
use gpui::AsyncApp;
use http_client::github::{AssetKind, GitHubLspBinaryVersion, latest_github_release};
use http_client::github_download::download_server_binary;
use language::{LanguageServerName, LspAdapter, LspAdapterDelegate, LspInstaller, Toolchain};
use lsp::{LanguageServerBinary, Uri};
use project::{ContextProviderWithTasks, lsp_store::language_server_settings};
use serde_json::Value;
use std::{env::consts, future::Future, path::PathBuf, sync::Arc};
use task::{TaskTemplate, TaskTemplates, VariableName};
use util::{ResultExt, maybe};

pub(super) fn bash_task_context() -> ContextProviderWithTasks {
    ContextProviderWithTasks::new(TaskTemplates(vec![
        TaskTemplate {
            label: "execute selection".to_owned(),
            command: VariableName::SelectedText.template_value(),
            ..TaskTemplate::default()
        },
        TaskTemplate {
            label: format!("run '{}'", VariableName::File.template_value()),
            command: VariableName::File.template_value(),
            tags: vec!["bash-script".to_owned()],
            ..TaskTemplate::default()
        },
    ]))
}

pub struct BashLspAdapter;

impl BashLspAdapter {
    const SERVER_NAME: LanguageServerName = LanguageServerName::new_static("shuck");
}

impl LspInstaller for BashLspAdapter {
    type BinaryVersion = GitHubLspBinaryVersion;

    async fn cached_server_binary(
        &self,
        container_dir: std::path::PathBuf,
        delegate: &dyn LspAdapterDelegate,
    ) -> Option<lsp::LanguageServerBinary> {
        let mut binary = get_cached_server_binary(container_dir).await?;
        binary.env = Some(delegate.shell_env().await);
        Some(binary)
    }

    async fn check_if_user_installed(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: &gpui::AsyncApp,
    ) -> Option<lsp::LanguageServerBinary> {
        let env = delegate.shell_env().await;

        // 1. Check if user has `shuck` in PATH
        if let Some(path) = delegate.which(Self::SERVER_NAME.as_ref()).await {
            return Some(LanguageServerBinary {
                path,
                env: Some(env),
                arguments: vec!["server".into()],
            });
        }

        // 2. Check next to current executable or in zedstorm-tools
        let exe_name = format!("shuck{}", consts::EXE_SUFFIX);
        if let Ok(current_exe) = std::env::current_exe() {
            if let Some(candidate) = current_exe.parent().map(|p| p.join(&exe_name)) {
                if candidate.is_file() {
                    return Some(LanguageServerBinary {
                        path: candidate,
                        env: Some(env),
                        arguments: vec!["server".into()],
                    });
                }
            }
        }
        let tools_candidate = paths::zedstorm_tools_dir().join("bin").join(&exe_name);
        if tools_candidate.is_file() {
            return Some(LanguageServerBinary {
                path: tools_candidate,
                env: Some(env),
                arguments: vec!["server".into()],
            });
        }

        // 3. Fallback to legacy user-installed bash-language-server if present
        if let Some(path) = delegate.which("bash-language-server".as_ref()).await {
            return Some(LanguageServerBinary {
                path,
                env: Some(env),
                arguments: vec!["start".into()],
            });
        }

        None
    }

    fn check_if_version_installed(
        &self,
        version: &Self::BinaryVersion,
        container_dir: &PathBuf,
        delegate: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Option<lsp::LanguageServerBinary>> + use<> {
        let version_name = version.name.clone();
        let container_dir = container_dir.clone();
        let delegate = delegate.clone();

        async move {
            let version_dir = container_dir.join(format!("shuck_{version_name}"));
            let mut binary = get_cached_server_binary(version_dir).await?;
            binary.env = Some(delegate.shell_env().await);
            Some(binary)
        }
    }

    async fn fetch_latest_server_version(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        pre_release: bool,
        _: &mut gpui::AsyncApp,
    ) -> Result<Self::BinaryVersion> {
        let release =
            latest_github_release("ewhauser/shuck", true, pre_release, delegate.http_client())
                .await?;

        let (os_pattern, ext) = match consts::OS {
            "macos" => ("apple-darwin", "tar.gz"),
            "linux" => ("unknown-linux", "tar.gz"),
            "windows" => ("pc-windows", "zip"),
            other => anyhow::bail!("Running on unsupported os: {other}"),
        };
        let arch = match consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            other => anyhow::bail!("Running on unsupported arch: {other}"),
        };

        let asset = release
            .assets
            .iter()
            .find(|asset| {
                asset.name.contains(arch)
                    && asset.name.contains(os_pattern)
                    && (asset.name.ends_with(ext) || asset.name.ends_with("tar.xz"))
            })
            .or_else(|| {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name.contains(arch) && asset.name.contains(consts::OS))
            })
            .with_context(|| {
                format!(
                    "no prebuilt shuck asset found for {arch}-{os_pattern} in release {}. Consider installing shuck via `cargo install shuck-cli`.",
                    release.tag_name
                )
            })?;

        let version = GitHubLspBinaryVersion {
            name: release.tag_name,
            url: asset.browser_download_url.clone(),
            digest: asset.digest.clone(),
        };
        Ok(version)
    }

    fn fetch_server_binary(
        &self,
        version: Self::BinaryVersion,
        container_dir: std::path::PathBuf,
        delegate: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Result<lsp::LanguageServerBinary>> + use<> {
        let delegate = delegate.clone();

        async move {
            let GitHubLspBinaryVersion {
                name,
                url,
                digest: expected_digest,
            } = version;
            let version_dir = container_dir.join(format!("shuck_{name}"));

            if let Some(mut cached) = get_cached_server_binary(version_dir.clone()).await {
                cached.env = Some(delegate.shell_env().await);
                return Ok(cached);
            }

            let asset_kind = if consts::OS == "windows" {
                AssetKind::Zip
            } else {
                AssetKind::TarGz
            };

            download_server_binary(
                &*delegate.http_client(),
                &url,
                expected_digest.as_deref(),
                &version_dir,
                asset_kind,
            )
            .await?;

            let mut binary = get_cached_server_binary(version_dir)
                .await
                .context("could not locate shuck binary after download")?;
            util::fs::make_file_executable(&binary.path).await?;
            binary.env = Some(delegate.shell_env().await);
            Ok(binary)
        }
    }
}

async fn get_cached_server_binary(container_dir: PathBuf) -> Option<LanguageServerBinary> {
    maybe!(async {
        let binary_name = format!("shuck{}", consts::EXE_SUFFIX);
        let mut candidates = vec![container_dir.join(&binary_name)];
        if let Ok(mut entries) = smol::fs::read_dir(&container_dir).await {
            while let Some(entry) = entries.next().await {
                if let Ok(entry) = entry {
                    if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                        candidates.push(entry.path().join(&binary_name));
                        candidates.push(entry.path().join("bin").join(&binary_name));
                    }
                }
            }
        }
        for candidate in candidates {
            if candidate.is_file() {
                return Ok(LanguageServerBinary {
                    path: candidate,
                    env: None,
                    arguments: vec!["server".into()],
                });
            }
        }
        anyhow::bail!("no cached shuck binary found in {container_dir:?}");
    })
    .await
    .log_err()
}

#[async_trait(?Send)]
impl LspAdapter for BashLspAdapter {
    fn name(&self) -> LanguageServerName {
        Self::SERVER_NAME
    }

    async fn workspace_configuration(
        self: Arc<Self>,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: Option<Uri>,
        cx: &mut AsyncApp,
    ) -> Result<Value> {
        let settings = cx.update(|cx| {
            language_server_settings(delegate.as_ref(), &self.name(), cx)
                .and_then(|s| s.settings.clone())
        });

        Ok(settings.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, BorrowAppContext, Context, TestAppContext};
    use language::{AutoindentMode, Buffer};
    use settings::SettingsStore;
    use std::num::NonZeroU32;
    use util::Unindent;
    use util::test::marked_text_offsets;

    #[gpui::test]
    async fn test_bash_autoindent(cx: &mut TestAppContext) {
        cx.executor().set_block_on_ticks(usize::MAX..=usize::MAX);
        let language = crate::language("bash", tree_sitter_bash::LANGUAGE.into());
        cx.update(|cx| {
            let test_settings = SettingsStore::test(cx);
            cx.set_global(test_settings);
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |s| {
                    s.project.all_languages.defaults.tab_size = NonZeroU32::new(2)
                });
            });
        });

        cx.new(|cx| {
            let mut buffer = Buffer::local("", cx).with_language(language, cx);

            let expect_indents_to =
                |buffer: &mut Buffer, cx: &mut Context<Buffer>, input: &str, expected: &str| {
                    buffer.edit(
                        [(0..buffer.len(), input)],
                        Some(AutoindentMode::EachLine),
                        cx,
                    );
                    assert_eq!(buffer.text(), expected);
                };

            // Do not indent after shebang
            expect_indents_to(
                &mut buffer,
                cx,
                "#!/usr/bin/env bash\n#",
                "#!/usr/bin/env bash\n#",
            );

            // indent function correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "function name() {\necho \"Hello, World!\"\n}",
                "function name() {\n  echo \"Hello, World!\"\n}",
            );

            // indent if-else correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "if true;then\nfoo\nelse\nbar\nfi",
                "if true;then\n  foo\nelse\n  bar\nfi",
            );

            // indent if-elif-else correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "if true;then\nfoo\nelif true;then\nbar\nelse\nbar\nfi",
                "if true;then\n  foo\nelif true;then\n  bar\nelse\n  bar\nfi",
            );

            // indent case-when-else correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "case $1 in\nfoo) echo \"Hello, World!\";;\n*) echo \"Unknown argument\";;\nesac",
                "case $1 in\n  foo) echo \"Hello, World!\";;\n  *) echo \"Unknown argument\";;\nesac",
            );

            // indent for-loop correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "for i in {1..10};do\nfoo\ndone",
                "for i in {1..10};do\n  foo\ndone",
            );

            // indent while-loop correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "while true; do\nfoo\ndone",
                "while true; do\n  foo\ndone",
            );

            // indent array correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "array=(\n1\n2\n3\n)",
                "array=(\n  1\n  2\n  3\n)",
            );

            // indents non-"function" function correctly
            expect_indents_to(
                &mut buffer,
                cx,
                "foo() {\necho \"Hello, World!\"\n}",
                "foo() {\n  echo \"Hello, World!\"\n}",
            );

            let (input, offsets) = marked_text_offsets(
                &r#"
                if foo; then
                  1ˇ
                else
                  3
                fi
                "#
                .unindent(),
            );

            buffer.edit([(0..buffer.len(), input)], None, cx);
            buffer.edit(
                [(offsets[0]..offsets[0], "\n")],
                Some(AutoindentMode::EachLine),
                cx,
            );
            buffer.edit(
                [(offsets[0] + 3..offsets[0] + 3, "elif")],
                Some(AutoindentMode::EachLine),
                cx,
            );
            let expected = r#"
                if foo; then
                  1
                elif
                else
                  3
                fi
                "#
            .unindent();

            assert_eq!(buffer.text(), expected);

            buffer
        });
    }
}
