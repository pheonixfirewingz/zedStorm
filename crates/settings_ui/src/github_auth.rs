use anyhow::{Context as _, Result, ensure};
use fs::Fs;
use futures::{AsyncBufReadExt as _, StreamExt as _, io::BufReader};
use gpui::{ClipboardItem, Task};
use http_client::{
    HttpClient,
    github::{AssetKind, latest_github_release},
    github_download::download_server_binary,
};
use paths::home_dir;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use ui::{Divider, DividerColor, prelude::*};
use util::ResultExt as _;
use util::command::{Command, Stdio};

pub(crate) struct GitHubAccount {
    task: Option<Task<()>>,
    message: SharedString,
    device_code: Option<SharedString>,
    signed_in: bool,
    failed: bool,
    ssh_authenticated: bool,
    ssh_user: Option<SharedString>,
    ssh_key_path: Option<PathBuf>,
    ssh_public_key: Option<SharedString>,
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
        http_client::managed_tools::configure_git(&git, &mut command)?
    }
    Ok(command)
}

fn ssh_command(program: &str) -> Result<Command> {
    let mut command = Command::new(program);
    command.kill_on_drop(true);
    if let Ok(path) =
        http_client::managed_tools::path_with_tools(std::env::var_os("PATH").as_deref())
    {
        command.env("PATH", path);
    }
    Ok(command)
}

pub(crate) fn get_device_name() -> String {
    if let Ok(hostname) = std::env::var("HOSTNAME") {
        let trimmed = hostname.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(hostname) = std::env::var("COMPUTERNAME") {
        let trimmed = hostname.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(hostname) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = hostname.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(hostname) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let trimmed = hostname.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(output) = std::process::Command::new("hostname").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    "device".to_string()
}

fn parse_github_username(text: &str) -> Option<String> {
    for line in text.lines() {
        if line.contains("successfully authenticated") {
            if let Some(rest) = line.trim().strip_prefix("Hi ") {
                if let Some((user, _)) = rest.split_once('!') {
                    let user = user.trim();
                    if !user.is_empty() {
                        return Some(user.to_string());
                    }
                }
            }
        }
    }
    None
}

async fn ensure_ssh_config(ssh_dir: &Path, key_path: &Path) -> Result<()> {
    let config_path = ssh_dir.join("config");
    let content = smol::fs::read_to_string(&config_path)
        .await
        .unwrap_or_default();

    let key_str = if let Ok(rel) = key_path.strip_prefix(home_dir()) {
        format!("~/{rel}", rel = rel.display())
    } else {
        key_path.display().to_string()
    };

    if !content.contains("Host github.com") {
        let mut new_content = content;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(&format!(
            "Host github.com\n    HostName github.com\n    User git\n    IdentityFile {key_str}\n    IdentitiesOnly yes\n"
        ));
        smol::fs::write(&config_path, new_content).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            smol::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
                .await
                .ok();
        }
    }
    Ok(())
}

async fn ensure_github_known_hosts(ssh_dir: &Path) -> Result<()> {
    let known_hosts_path = ssh_dir.join("known_hosts");
    let content = smol::fs::read_to_string(&known_hosts_path)
        .await
        .unwrap_or_default();
    if !content.contains("github.com") {
        let mut new_content = content;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(
            "github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl\n\
             github.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=\n\
             github.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=\n",
        );
        smol::fs::write(&known_hosts_path, new_content).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            smol::fs::set_permissions(&known_hosts_path, std::fs::Permissions::from_mode(0o644))
                .await
                .ok();
        }
    }
    Ok(())
}

async fn find_or_create_ssh_key(ssh_dir: &Path) -> Result<(PathBuf, bool)> {
    smol::fs::create_dir_all(ssh_dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        smol::fs::set_permissions(ssh_dir, std::fs::Permissions::from_mode(0o700)).await?;
    }

    let zedstorm_key = ssh_dir.join("id_ed25519_zedstorm");
    let zedstorm_pub = PathBuf::from(format!("{}.pub", zedstorm_key.display()));
    if smol::fs::metadata(&zedstorm_key).await.is_ok() {
        if smol::fs::metadata(&zedstorm_pub).await.is_err() {
            if let Ok(mut extract) = ssh_command("ssh-keygen") {
                extract.args(["-y", "-f"]).arg(&zedstorm_key).stdin(Stdio::null());
                if let Ok(out) = extract.output().await {
                    if out.status.success() {
                        smol::fs::write(&zedstorm_pub, out.stdout).await.ok();
                    }
                }
            }
        }
        if smol::fs::metadata(&zedstorm_pub).await.is_ok() {
            return Ok((zedstorm_key, false));
        }
    }

    let mut command = ssh_command("ssh-keygen")?;
    command.args(["-t", "ed25519", "-C", "ZedStorm", "-f"]);
    command.arg(&zedstorm_key);
    command.args(["-N", ""]);
    command.stdin(Stdio::null());
    let output = command
        .output()
        .await
        .context("Could not run ssh-keygen to generate SSH key")?;
    ensure!(
        output.status.success(),
        "ssh-keygen failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        smol::fs::set_permissions(&zedstorm_key, std::fs::Permissions::from_mode(0o600)).await?;
    }

    Ok((zedstorm_key, true))
}

async fn test_github_ssh(key_path: &Path) -> Result<Option<String>> {
    let mut command = ssh_command("ssh")?;
    command
        .args(["-i"])
        .arg(key_path)
        .args([
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "-T",
            "git@github.com",
        ]);
    let output = command.output().await.context("Could not execute ssh")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");
    Ok(parse_github_username(&combined))
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
            message: "Checking GitHub and SSH status…".into(),
            device_code: None,
            signed_in: false,
            failed: false,
            ssh_authenticated: false,
            ssh_user: None,
            ssh_key_path: None,
            ssh_public_key: None,
        };
        account.refresh(cx);
        account
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            return;
        }
        self.failed = false;
        self.message = "Checking GitHub and SSH status…".into();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result: Result<(Option<bool>, Option<String>, Option<PathBuf>, Option<String>)> =
                async {
                    let gh_status = async {
                        let binary = github_cli_path(&tools_directory()?);
                        match smol::fs::metadata(&binary).await {
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                return Ok(None);
                            }
                            Err(error) => {
                                return Err(error).context("Could not inspect GitHub tools");
                            }
                        }
                        let output = github_command(&binary)?
                            .args(["auth", "status", "--hostname", "github.com"])
                            .output()
                            .await
                            .context("Could not start the bundled GitHub CLI")?;
                        Ok(Some(output.status.success()))
                    }
                    .await
                    .unwrap_or(None);

                    let (ssh_user, key_path, pubkey) = async {
                        let ssh_dir = home_dir().join(".ssh");
                        let zedstorm = ssh_dir.join("id_ed25519_zedstorm");
                        let ed25519 = ssh_dir.join("id_ed25519");
                        let rsa = ssh_dir.join("id_rsa");
                        let ecdsa = ssh_dir.join("id_ecdsa");
                        let mut found_key = None;
                        for candidate in [&zedstorm, &ed25519, &rsa, &ecdsa] {
                            if smol::fs::metadata(candidate).await.is_ok() {
                                found_key = Some(candidate.to_path_buf());
                                break;
                            }
                        }
                        let Some(key) = found_key else {
                            return (None, None, None);
                        };
                        let pub_key_path = PathBuf::from(format!("{}.pub", key.display()));
                        let pubkey = smol::fs::read_to_string(&pub_key_path).await.ok();
                        let user = test_github_ssh(&key).await.ok().flatten();
                        (user, Some(key), pubkey)
                    }
                    .await;

                    Ok((gh_status, ssh_user, key_path, pubkey))
                }
                .await;

            this.update(cx, |this, cx| {
                this.task = None;
                match result {
                    Ok((gh_status, ssh_user, key_path, pubkey)) => {
                        this.signed_in = gh_status.unwrap_or(false);
                        this.ssh_authenticated = ssh_user.is_some();
                        this.ssh_user = ssh_user.map(Into::into);
                        this.ssh_key_path = key_path;
                        if let Some(pk) = pubkey {
                            this.ssh_public_key = Some(pk.trim().to_string().into());
                        }

                        let key_display = this.ssh_key_path.as_ref().map(|p| {
                            if let Ok(rel) = p.strip_prefix(home_dir()) {
                                format!("~/{rel}", rel = rel.display())
                            } else {
                                p.display().to_string()
                            }
                        });

                        let is_zedstorm = this.ssh_key_path.as_ref().map_or(false, |p| {
                            p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.contains("zedstorm"))
                        });

                        this.message = match (&this.ssh_user, this.signed_in, is_zedstorm) {
                            (Some(user), true, true) => {
                                format!(
                                    "Signed in to GitHub (HTTPS). ZedStorm SSH key registered and authenticated as {user}."
                                )
                                .into()
                            }
                            (Some(user), false, true) => {
                                format!("ZedStorm SSH key registered and authenticated with GitHub as {user}.")
                                    .into()
                            }
                            (Some(user), true, false) => {
                                format!(
                                    "Signed in to GitHub (HTTPS). Existing SSH key ({user}) found. Click 'Set Up SSH' to register ZedStorm key."
                                )
                                .into()
                            }
                            (Some(user), false, false) => {
                                format!("Existing SSH key ({user}) found. Click 'Set Up SSH' to register a dedicated ZedStorm key.")
                                    .into()
                            }
                            (None, true, _) => {
                                if let Some(kd) = key_display {
                                    format!(
                                        "Signed in to GitHub CLI. SSH key found at {kd} but not yet authenticated with GitHub."
                                    )
                                    .into()
                                } else {
                                    "Signed in to GitHub CLI (HTTPS). SSH is not configured yet."
                                        .into()
                                }
                            }
                            (None, false, _) => {
                                "Set up SSH for Git and GitHub in 1 click, or sign in with your browser."
                                    .into()
                            }
                        };
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

    pub(crate) fn setup_ssh(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            return;
        }
        self.failed = false;
        self.device_code = None;
        self.message = "Setting up SSH for Git and GitHub…".into();
        let directory = tools_directory();
        let http = cx.http_client();
        let installation =
            cx.background_spawn(async move { ensure_github_cli(directory?, http).await });

        self.task = Some(cx.spawn(async move |this, cx| {
            let result: Result<String> = async {
                let ssh_dir = home_dir().join(".ssh");
                this.update(cx, |this, cx| {
                    this.message = "Preparing SSH key for ZedStorm…".into();
                    cx.notify();
                })?;

                let (private_key, _newly_created) = find_or_create_ssh_key(&ssh_dir).await?;
                ensure_github_known_hosts(&ssh_dir).await.log_err();
                ensure_ssh_config(&ssh_dir, &private_key).await.log_err();

                let key_display = if let Ok(rel) = private_key.strip_prefix(home_dir()) {
                    format!("~/{rel}", rel = rel.display())
                } else {
                    private_key.display().to_string()
                };

                let pub_key_path = PathBuf::from(format!("{}.pub", private_key.display()));
                let pub_key_content = smol::fs::read_to_string(&pub_key_path)
                    .await
                    .context("Could not read SSH public key")?;

                this.update(cx, |this, cx| {
                    this.ssh_key_path = Some(private_key.clone());
                    this.ssh_public_key = Some(pub_key_content.trim().to_string().into());
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        pub_key_content.trim().to_string(),
                    ));
                    settings::update_settings_file(<dyn Fs>::global(cx), cx, {
                        let key_display = key_display.clone();
                        move |settings, _| {
                            settings.git.get_or_insert_default().ssh_key = Some(key_display);
                        }
                    });
                    this.message = "Connecting to GitHub to register SSH key…".into();
                    cx.notify();
                })?;

                let device_name = get_device_name();
                let title = format!("ZedStorm {device_name}");

                let binary = installation.await?;
                let auth_status = github_command(&binary)?
                    .args(["auth", "status", "--hostname", "github.com"])
                    .output()
                    .await;
                let gh_signed_in = auth_status.map(|o| o.status.success()).unwrap_or(false);

                let mut upload_succeeded = false;
                if gh_signed_in {
                    let mut add_cmd = github_command(&binary)?;
                    add_cmd
                        .args(["ssh-key", "add"])
                        .arg(&pub_key_path)
                        .args(["--title", &title, "--type", "authentication"])
                        .stdin(Stdio::null());
                    let add_output = add_cmd.output().await?;

                    let stderr = String::from_utf8_lossy(&add_output.stderr);
                    if add_output.status.success()
                        || stderr.contains("already in use")
                        || stderr.contains("already exists")
                    {
                        upload_succeeded = true;
                    } else if stderr.contains("admin:public_key") || stderr.contains("404") {
                        this.update(cx, |this, cx| {
                            this.message =
                                "Requesting GitHub authorization for SSH key access…".into();
                            cx.notify();
                        })?;

                        let mut refresh_cmd = github_command(&binary)?;
                        refresh_cmd
                            .args([
                                "auth",
                                "refresh",
                                "-h",
                                "github.com",
                                "-s",
                                "admin:public_key",
                            ])
                            .stdin(Stdio::null());
                        let mut child = refresh_cmd
                            .spawn()
                            .context("Could not start GitHub CLI auth refresh")?;

                        let stderr =
                            child.stderr.take().context("No stderr from gh auth refresh")?;
                        let mut lines = BufReader::new(stderr).lines();
                        while let Some(line) = lines.next().await {
                            let line = line?;
                            if line.trim().is_empty() {
                                continue;
                            }
                            let code = device_code(&line).map(SharedString::from);
                            let url = verification_url(&line).map(str::to_owned);
                            this.update(cx, |this, cx| {
                                this.message = line.chars().take(500).collect::<String>().into();
                                if let Some(code) = code {
                                    this.device_code = Some(code);
                                }
                                if let Some(url) = url {
                                    cx.open_url(&url);
                                    this.message =
                                        "Authorize SSH key permissions in your browser using the code below."
                                            .into();
                                }
                                cx.notify();
                            })?;
                        }

                        ensure!(
                            child.status().await?.success(),
                            "GitHub authorization failed"
                        );

                        let mut add_again_cmd = github_command(&binary)?;
                        add_again_cmd
                            .args(["ssh-key", "add"])
                            .arg(&pub_key_path)
                            .args(["--title", &title, "--type", "authentication"])
                            .stdin(Stdio::null());
                        let add_again = add_again_cmd.output().await?;
                        let stderr_again = String::from_utf8_lossy(&add_again.stderr);
                        if add_again.status.success()
                            || stderr_again.contains("already in use")
                            || stderr_again.contains("already exists")
                        {
                            upload_succeeded = true;
                        }
                    }
                } else {
                    this.update(cx, |this, cx| {
                        this.message = "Starting GitHub sign-in to upload SSH key…".into();
                        cx.notify();
                    })?;

                    let mut login_cmd = github_command(&binary)?;
                    login_cmd
                        .args([
                            "auth",
                            "login",
                            "--hostname",
                            "github.com",
                            "-s",
                            "admin:public_key",
                            "--git-protocol",
                            "ssh",
                            "--web",
                        ])
                        .stdin(Stdio::null());
                    let mut child = login_cmd
                        .spawn()
                        .context("Could not start GitHub CLI login")?;

                    let stderr = child.stderr.take().context("No stderr from gh auth login")?;
                    let mut lines = BufReader::new(stderr).lines();
                    while let Some(line) = lines.next().await {
                        let line = line?;
                        if line.trim().is_empty() {
                            continue;
                        }
                        let code = device_code(&line).map(SharedString::from);
                        let url = verification_url(&line).map(str::to_owned);
                        this.update(cx, |this, cx| {
                            this.message = line.chars().take(500).collect::<String>().into();
                            if let Some(code) = code {
                                this.device_code = Some(code);
                            }
                            if let Some(url) = url {
                                cx.open_url(&url);
                                this.message =
                                    "Sign in to GitHub in your browser using the code below.".into();
                            }
                            cx.notify();
                        })?;
                    }

                    if child.status().await.map(|s| s.success()).unwrap_or(false) {
                        let mut add_cmd = github_command(&binary)?;
                        add_cmd
                            .args(["ssh-key", "add"])
                            .arg(&pub_key_path)
                            .args(["--title", &title, "--type", "authentication"])
                            .stdin(Stdio::null());
                        let add_res = add_cmd.output().await;
                        if let Ok(res) = add_res {
                            let stderr = String::from_utf8_lossy(&res.stderr);
                            if res.status.success()
                                || stderr.contains("already in use")
                                || stderr.contains("already exists")
                            {
                                upload_succeeded = true;
                            }
                        }
                    }
                }

                let _ = github_command(&binary)?
                    .args(["config", "set", "git_protocol", "ssh"])
                    .output()
                    .await;

                if let Some(username) = test_github_ssh(&private_key).await? {
                    Ok(format!(
                        "SSH configured! Key registered to GitHub as ZedStorm {device_name} and authenticated as {username}."
                    ))
                } else if upload_succeeded {
                    Ok(format!(
                        "SSH key registered to GitHub as ZedStorm {device_name} and configured in Zed (key: {key_display})."
                    ))
                } else {
                    this.update(cx, |_, cx| {
                        cx.open_url("https://github.com/settings/ssh/new");
                    })?;
                    Ok(format!(
                        "SSH key created and copied to clipboard! Paste it into GitHub SSH settings with title 'ZedStorm {device_name}', then click 'Refresh'."
                    ))
                }
            }
            .await;

            this.update(cx, |this, cx| {
                this.task = None;
                this.device_code = None;
                match result {
                    Ok(msg) => {
                        this.failed = false;
                        this.message = msg.into();
                        this.refresh(cx);
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

    pub(crate) fn sign_in(&mut self, cx: &mut Context<Self>) {
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
                        this.refresh(cx);
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
        self.message = "Operation cancelled.".into();
        cx.notify();
    }
}

impl Render for GitHubAccount {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let device_name = get_device_name();
        let is_zedstorm = self.ssh_key_path.as_ref().map_or(false, |p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.contains("zedstorm"))
        });
        let key_display = self.ssh_key_path.as_ref().map(|p| {
            if let Ok(rel) = p.strip_prefix(home_dir()) {
                format!("~/{rel}", rel = rel.display())
            } else {
                p.display().to_string()
            }
        });

        v_flex()
            .w_full()
            .gap_3()
            .child(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(
                        Label::new("GitHub")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .buffer_font(cx),
                    )
                    .child(Divider::horizontal().color(DividerColor::BorderFaded)),
            )
            .child(
                Label::new(self.message.clone()).color(if self.failed {
                    Color::Error
                } else if self.ssh_authenticated || self.signed_in {
                    Color::Success
                } else {
                    Color::Muted
                }),
            )
            .when_some(key_display, |this, key_path| {
                let is_zedstorm = self.ssh_key_path.as_ref().map_or(false, |p| {
                    p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.contains("zedstorm"))
                });
                let status_text = if self.ssh_authenticated {
                    if is_zedstorm {
                        if let Some(ref user) = self.ssh_user {
                            format!("SSH key: {key_path} (Registered to GitHub as ZedStorm {device_name}, authenticated as {user})")
                        } else {
                            format!("SSH key: {key_path} (Registered to GitHub as ZedStorm {device_name}, authenticated)")
                        }
                    } else {
                        if let Some(ref user) = self.ssh_user {
                            format!("Existing SSH key: {key_path} (Authenticated as {user} on GitHub). Click 'Set Up SSH' to register ZedStorm {device_name}.")
                        } else {
                            format!("Existing SSH key: {key_path} (Authenticated on GitHub). Click 'Set Up SSH' to register ZedStorm {device_name}.")
                        }
                    }
                } else {
                    format!("SSH key: {key_path} (Not registered to GitHub)")
                };
                this.child(
                    Label::new(status_text).size(LabelSize::Small).color(
                        if self.ssh_authenticated {
                            Color::Success
                        } else {
                            Color::Muted
                        },
                    ),
                )
            })
            .when_some(self.device_code.clone(), |this, code| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(Label::new("GitHub authorization code"))
                        .child(Label::new(code.clone()).size(LabelSize::Large))
                        .child(
                            Label::new(
                                "Enter this code at github.com/login/device to grant access. It stays visible here until completed.",
                            )
                            .size(LabelSize::Small),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    Button::new("copy-github-code", "Copy code")
                                        .tab_index(0_isize)
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                code.to_string(),
                                            ));
                                        }),
                                )
                                .child(
                                    Button::new("open-github-browser", "Open browser")
                                        .tab_index(0_isize)
                                        .on_click(|_, _, cx| {
                                            cx.open_url("https://github.com/login/device");
                                        }),
                                ),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new(
                            "github-login",
                            if self.signed_in {
                                "Log In (Signed In)"
                            } else {
                                "Log In"
                            },
                        )
                        .tab_index(0_isize)
                        .disabled(self.task.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.sign_in(cx))),
                    )
                    .child(
                        Button::new(
                            "setup-ssh",
                            if self.ssh_authenticated && is_zedstorm {
                                "Set Up SSH (Configured)"
                            } else {
                                "Set Up SSH"
                            },
                        )
                        .style(ButtonStyle::Tinted(ui::TintColor::Accent))
                        .tab_index(0_isize)
                        .disabled(self.task.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.setup_ssh(cx))),
                    )
                    .child(
                        Button::new("github-refresh", "Refresh")
                            .tab_index(0_isize)
                            .disabled(self.task.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .when(self.task.is_some(), |this| {
                        this.child(
                            Button::new("github-cancel", "Cancel")
                                .tab_index(0_isize)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                    })
                    .when_some(self.ssh_public_key.clone(), |this, pubkey| {
                        this.child(
                            Button::new("copy-ssh-pubkey", "Copy public key")
                                .tab_index(0_isize)
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        pubkey.to_string(),
                                    ));
                                }),
                        )
                    }),
            )
            .child(
                Label::new(format!(
                    "Log in for GitHub Git authentication, or click 'Set Up SSH' to generate an SSH key and register it to GitHub as ZedStorm {device_name} in 1 click."
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_device_name_formatting() {
        let name = super::get_device_name();
        assert!(!name.trim().is_empty());
        let title = format!("ZedStorm {}", name);
        assert!(title.starts_with("ZedStorm "));
        assert!(!title.contains('"'));
    }

    use super::{device_code, parse_github_username, release_archive, verification_url};
    use http_client::github::AssetKind;
    use std::path::PathBuf;

    #[test]
    fn parses_github_username_from_ssh_output() {
        assert_eq!(
            parse_github_username(
                "Hi octocat! You've successfully authenticated, but GitHub does not provide shell access."
            ),
            Some("octocat".to_string())
        );
        assert_eq!(
            parse_github_username(
                "Hi pheonixfirewingz! You've successfully authenticated, but GitHub does not provide shell access."
            ),
            Some("pheonixfirewingz".to_string())
        );
        assert_eq!(
            parse_github_username("git@github.com: Permission denied (publickey)."),
            None
        );
    }

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
