use anyhow::{Context as _, Result, ensure};
use futures::{AsyncBufReadExt as _, StreamExt as _, io::BufReader};
use gpui::{ClipboardItem, Task};
use http_client::{
    HttpClient,
    github::{AssetKind, latest_github_release},
    github_download::download_server_binary,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use ui::prelude::*;
use util::ResultExt as _;
use util::command::{Command, Stdio};

pub(crate) struct GitHubAccount {
    task: Option<Task<()>>,
    message: SharedString,
    device_code: Option<SharedString>,
    signed_in: bool,
    failed: bool,
}

static INSTALL_LOCK: futures::lock::Mutex<()> = futures::lock::Mutex::new(());

fn tools_directory() -> Result<PathBuf> {
    let executable =
        std::env::current_exe().context("Could not locate the application executable")?;
    Ok(executable
        .parent()
        .context("The application executable has no parent directory")?
        .join("zedstorm-tools"))
}

fn github_cli_path(directory: &Path) -> PathBuf {
    directory
        .join("github-cli")
        .join("bin")
        .join(if cfg!(windows) { "gh.exe" } else { "gh" })
}

fn release_archive(
    version: &str,
    operating_system: &str,
    architecture: &str,
) -> Result<(String, AssetKind, PathBuf)> {
    ensure!(
        !version.is_empty()
            && version
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.'),
        "Invalid GitHub CLI release version"
    );
    let architecture = match architecture {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" if operating_system == "linux" => "armv6",
        other => anyhow::bail!("GitHub CLI is not available for architecture {other}"),
    };
    let (platform, extension, kind) = match operating_system {
        "linux" => ("linux", "tar.gz", AssetKind::TarGz),
        "windows" => ("windows", "zip", AssetKind::Zip),
        "macos" => ("macOS", "zip", AssetKind::Zip),
        other => anyhow::bail!("GitHub CLI is not available for {other}"),
    };
    let name = format!("gh_{version}_{platform}_{architecture}");
    let binary = if operating_system == "windows" {
        PathBuf::from("bin/gh.exe")
    } else {
        PathBuf::from(&name).join("bin/gh")
    };
    Ok((format!("{name}.{extension}"), kind, binary))
}

async fn ensure_github_cli(directory: PathBuf, http: Arc<dyn HttpClient>) -> Result<PathBuf> {
    let _guard = INSTALL_LOCK.lock().await;
    http_client::managed_tools::ensure_tool(http_client::managed_tools::Tool::Git, http.clone())
        .await?;
    let binary = github_cli_path(&directory);
    match smol::fs::metadata(&binary).await {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "GitHub CLI path is not a file: {}",
                binary.display()
            );
            return Ok(binary);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Could not inspect the bundled GitHub CLI"),
    }
    smol::fs::create_dir_all(&directory).await.with_context(|| format!("Could not install GitHub tools beside the application at {}. Move the application to a writable folder and retry", directory.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".github-cli-install-")
        .tempdir_in(&directory)?;
    let release = latest_github_release("cli/cli", true, false, http.clone()).await?;
    let version = release
        .tag_name
        .strip_prefix('v')
        .context("Invalid GitHub CLI release tag")?;
    let (asset_name, kind, relative_binary) =
        release_archive(version, std::env::consts::OS, std::env::consts::ARCH)?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .context("GitHub CLI release has no download for this platform")?;
    ensure!(
        asset
            .browser_download_url
            .starts_with("https://github.com/cli/cli/releases/download/"),
        "Unexpected GitHub CLI download URL"
    );
    let digest = asset
        .digest
        .as_deref()
        .context("GitHub CLI release has no SHA-256 checksum")?;
    let archive = staging.path().join("archive");
    download_server_binary(
        http.as_ref(),
        &asset.browser_download_url,
        Some(digest),
        &archive,
        kind,
    )
    .await
    .context("Could not download and verify GitHub CLI")?;
    let source = archive.join(relative_binary);
    let payload = staging.path().join("payload");
    smol::fs::create_dir_all(payload.join("bin")).await?;
    let destination = payload.join("bin").join(
        binary
            .file_name()
            .context("Invalid GitHub CLI binary path")?,
    );
    smol::fs::copy(&source, &destination)
        .await
        .context("GitHub CLI archive did not contain its executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        smol::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755)).await?;
    }
    let license = source
        .parent()
        .and_then(Path::parent)
        .context("Invalid GitHub CLI archive layout")?
        .join("LICENSE");
    smol::fs::copy(license, payload.join("LICENSE"))
        .await
        .context("Could not preserve the GitHub CLI license")?;
    smol::fs::rename(&payload, directory.join("github-cli"))
        .await
        .context("Could not finish installing GitHub CLI beside the application")?;
    Ok(binary)
}

fn github_command(binary: &Path) -> Result<Command> {
    let mut command = Command::new(binary);
    command
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.env(
        "PATH",
        http_client::managed_tools::path_with_tools(std::env::var_os("PATH").as_deref())?,
    );
    if let Some(git) =
        http_client::managed_tools::system_or_bundled(http_client::managed_tools::Tool::Git)?
    {
        http_client::managed_tools::configure_git(&git, &mut command)?;
    }
    Ok(command)
}

fn device_code(line: &str) -> Option<&str> {
    let code = if let Some((_, code)) = line.split_once("First copy your one-time code:") {
        code.trim()
    } else {
        let (_, remaining) = line.split_once("One-time code (")?;
        let (code, suffix) = remaining.split_once(')')?;
        if suffix.trim() != "copied to clipboard" {
            return None;
        }
        code
    };
    (code.len() == 9
        && code.as_bytes().get(4) == Some(&b'-')
        && code
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-'))
    .then_some(code)
}

fn verification_url(line: &str) -> Option<&str> {
    line.split_whitespace().find(|part| {
        *part == "https://github.com/login/device" || *part == "https://github.com/login/device/"
    })
}

impl GitHubAccount {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut account = Self {
            task: None,
            message: "Checking GitHub sign-in…".into(),
            device_code: None,
            signed_in: false,
            failed: false,
        };
        account.refresh(cx);
        account
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            return;
        }
        self.failed = false;
        self.message = "Checking GitHub sign-in…".into();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result: Result<Option<bool>> = async {
                let binary = github_cli_path(&tools_directory()?);
                match smol::fs::metadata(&binary).await {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error).context("Could not inspect GitHub tools"),
                }
                let output = github_command(&binary)?
                    .args(["auth", "status", "--hostname", "github.com"])
                    .output().await.context("Could not start the bundled GitHub CLI")?;
                Ok(Some(output.status.success()))
            }.await;
            this.update(cx, |this, cx| {
                this.task = None;
                match result {
                    Ok(status) => {
                        this.signed_in = status.unwrap_or(false);
                        this.message = if this.signed_in {
                            "Signed in to GitHub".into()
                        } else if status.is_none() {
                            "Sign in with your browser. GitHub tools will be downloaded automatically.".into()
                        } else {
                            "Sign in to GitHub to authenticate HTTPS pushes and pulls.".into()
                        };
                    }
                    Err(error) => {
                        this.failed = true;
                        this.message = format!("{error:#}").into();
                    }
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            return;
        }
        self.message = "Preparing GitHub sign-in. Downloading tools if needed…".into();
        self.device_code = None;
        self.failed = false;
        let directory = tools_directory();
        let http = cx.http_client();
        let installation =
            cx.background_spawn(async move { ensure_github_cli(directory?, http).await });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result: Result<()> = async {
                let binary = installation.await?;
                this.update(cx, |this, cx| {
                    this.message = "Starting GitHub browser sign-in…".into();
                    cx.notify();
                })?;
                let mut child = github_command(&binary)?
                    .args([
                        "auth",
                        "login",
                        "--hostname",
                        "github.com",
                        "--git-protocol",
                        "https",
                        "--web",
                        "--skip-ssh-key",
                    ])
                    .spawn()
                    .context("Could not start the bundled GitHub CLI")?;
                let stderr = child
                    .stderr
                    .take()
                    .context("GitHub sign-in output is unavailable")?;
                let mut lines = BufReader::new(stderr).lines();
                let mut diagnostic = String::new();
                while let Some(line) = lines.next().await {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    diagnostic = line.chars().take(500).collect();
                    let code = device_code(&line).map(SharedString::from);
                    let url = verification_url(&line).map(str::to_owned);
                    this.update(cx, |this, cx| {
                        this.message = diagnostic.clone().into();
                        if let Some(code) = code {
                            this.device_code = Some(code);
                        }
                        if let Some(url) = url {
                            cx.open_url(&url);
                            this.message =
                                "Complete sign-in in your browser using the code below.".into();
                        }
                        cx.notify();
                    })?;
                }
                ensure!(
                    child.status().await?.success(),
                    "GitHub sign-in failed: {diagnostic}"
                );
                let output = github_command(&binary)?
                    .args(["auth", "setup-git", "--hostname", "github.com"])
                    .output()
                    .await
                    .context("Could not configure Git authentication")?;
                ensure!(
                    output.status.success(),
                    "Signed in, but Git credential setup failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                Ok(())
            }
            .await;
            this.update(cx, |this, cx| {
                this.task = None;
                this.device_code = None;
                match result {
                    Ok(()) => {
                        this.signed_in = true;
                        this.message =
                            "Signed in to GitHub. HTTPS pushes and pulls are configured.".into();
                    }
                    Err(error) => {
                        this.failed = true;
                        this.message = format!("{error:#}").into();
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        self.device_code = None;
        self.message = "GitHub sign-in cancelled.".into();
        cx.notify();
    }
}

impl Render for GitHubAccount {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w_full().gap_2()
            .child(Label::new("GitHub Account").size(LabelSize::Large))
            .child(Label::new(self.message.clone()).color(if self.failed { Color::Error } else { Color::Muted }))
            .when_some(self.device_code.clone(), |this, code| {
                this.child(v_flex().gap_2()
                    .child(Label::new("GitHub sign-in code"))
                    .child(Label::new(code.clone()).size(LabelSize::Large))
                    .child(Label::new("Enter this code at github.com/login/device. If copying fails, type it manually; it stays visible here while sign-in is pending.").size(LabelSize::Small))
                    .child(h_flex().gap_2().flex_wrap()
                    .child(Button::new("copy-github-code", "Copy code").tab_index(0_isize).on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                    }))
                    .child(Button::new("open-github-browser", "Open browser").tab_index(0_isize).on_click(|_, _, cx| {
                        cx.open_url("https://github.com/login/device");
                    }))))
            })
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("github-sign-in", if self.signed_in { "Sign in with another account" } else { "Sign in with browser" })
                    .tab_index(0_isize)
                    .disabled(self.task.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.sign_in(cx))))
                .child(Button::new("github-refresh", "Refresh status")
                    .tab_index(0_isize)
                    .disabled(self.task.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                .when(self.task.is_some(), |this| {
                    this.child(Button::new("github-cancel", "Cancel")
                        .tab_index(0_isize)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
                }))
            .child(Label::new("Sign in once for HTTPS pushes and pulls. SSH remotes use SSH keys; commit signing uses the signing settings below.").size(LabelSize::Small).color(Color::Muted))
    }
}

#[cfg(test)]
mod tests {
    use super::{device_code, release_archive, verification_url};
    use http_client::github::AssetKind;
    use std::path::PathBuf;

    #[test]
    fn matches_official_release_archive_layouts() -> anyhow::Result<()> {
        assert_eq!(
            release_archive("2.90.0", "linux", "x86_64")?,
            (
                "gh_2.90.0_linux_amd64.tar.gz".into(),
                AssetKind::TarGz,
                PathBuf::from("gh_2.90.0_linux_amd64/bin/gh")
            )
        );
        assert_eq!(
            release_archive("2.90.0", "windows", "aarch64")?,
            (
                "gh_2.90.0_windows_arm64.zip".into(),
                AssetKind::Zip,
                PathBuf::from("bin/gh.exe")
            )
        );
        assert_eq!(
            release_archive("2.90.0", "macos", "aarch64")?,
            (
                "gh_2.90.0_macOS_arm64.zip".into(),
                AssetKind::Zip,
                PathBuf::from("gh_2.90.0_macOS_arm64/bin/gh")
            )
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_versions_and_unsupported_platforms() {
        assert!(release_archive("../../other", "linux", "x86_64").is_err());
        assert!(release_archive("", "linux", "x86_64").is_err());
        assert!(release_archive("2.90.0", "freebsd", "x86_64").is_err());
        assert!(release_archive("2.90.0", "linux", "unknown").is_err());
    }

    #[test]
    fn extracts_device_code_from_github_cli_output() {
        assert_eq!(
            device_code("! First copy your one-time code: ABCD-1234"),
            Some("ABCD-1234")
        );
        assert_eq!(
            device_code("! First copy your one-time code: invalid"),
            None
        );
        assert_eq!(device_code("token: secret"), None);
        assert_eq!(
            device_code("! One-time code (ABCD-1234) copied to clipboard"),
            Some("ABCD-1234")
        );
        assert_eq!(
            device_code("! One-time code (invalid) copied to clipboard"),
            None
        );
        assert_eq!(
            device_code("! Failed to copy one-time code to clipboard"),
            None
        );
    }

    #[test]
    fn only_opens_github_device_verification() {
        assert_eq!(
            verification_url(
                "Open this URL to continue in your web browser: https://github.com/login/device"
            ),
            Some("https://github.com/login/device")
        );
        assert_eq!(
            verification_url("https://github.com.attacker.test/login/device"),
            None
        );
        assert_eq!(verification_url("https://attacker.test/login/device"), None);
    }
}
