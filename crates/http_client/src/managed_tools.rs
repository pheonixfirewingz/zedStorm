use crate::{
    HttpClient,
    github::{AssetKind, latest_github_release},
    github_download::download_server_binary,
};
use anyhow::{Context as _, Result, ensure};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Git,
    Codex,
    Gpg,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Codex => "codex",
            Self::Gpg => "gpg",
        }
    }

    pub fn repository(self, operating_system: &str) -> &'static str {
        match (self, operating_system) {
            (Self::Git, _) => "desktop/dugite-native",
            (Self::Codex, _) => "openai/codex",
            (Self::Gpg, "windows") => "portapps/gnupg-portable",
            (Self::Gpg, _) => "pheonixfirewingz/zedStorm",
        }
    }

    pub fn binary(self, operating_system: &str) -> PathBuf {
        match (self, operating_system) {
            (Self::Git, "windows") => PathBuf::from("cmd/git.exe"),
            (Self::Git, _) => PathBuf::from("bin/git"),
            (Self::Codex, "windows") => PathBuf::from("bin/codex.exe"),
            (Self::Codex, _) => PathBuf::from("bin/codex"),
            (Self::Gpg, "windows") => PathBuf::from("bin/gpg.exe"),
            (Self::Gpg, _) => PathBuf::from("bin/gpg"),
        }
    }
}

static INSTALL_LOCK: futures::lock::Mutex<()> = futures::lock::Mutex::new(());

pub fn tools_directory() -> Result<PathBuf> {
    Ok(paths::zedstorm_tools_dir().clone())
}

pub fn bundled_path(tool: Tool) -> Result<PathBuf> {
    Ok(tools_directory()?
        .join(tool.name())
        .join(tool.binary(std::env::consts::OS)))
}

pub fn system_or_bundled(tool: Tool) -> Result<Option<PathBuf>> {
    let binary = bundled_path(tool)?;
    if binary.is_file() {
        return Ok(Some(binary));
    }
    match which::which(tool.name()) {
        Ok(binary) => return Ok(Some(binary)),
        Err(which::Error::CannotFindBinaryPath) => {}
        Err(error) => return Err(error).context("Could not search for a system tool"),
    }
    if tool == Tool::Gpg {
        if let Ok(binary) = which::which("gpg2") {
            return Ok(Some(binary));
        }
    }
    Ok(None)
}

pub fn path_with_tools(existing: Option<&OsStr>) -> Result<OsString> {
    let directory = tools_directory()?;
    let mut paths = existing
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    paths.extend([
        directory.join("git/bin"),
        directory.join("git/cmd"),
        directory.join("git/usr/bin"),
        directory.join("codex/bin"),
        directory.join("github-cli/bin"),
        directory.join("gpg/bin"),
        directory.join("gpg"),
    ]);
    std::env::join_paths(paths).context("Could not add bundled tools to the command search path")
}

pub fn configure_git(binary: &Path, command: &mut util::command::Command) -> Result<()> {
    if binary != bundled_path(Tool::Git)? {
        return Ok(());
    }
    let root = tools_directory()?.join("git");
    if cfg!(not(windows)) {
        command
            .env("GIT_EXEC_PATH", root.join("libexec/git-core"))
            .env("GIT_TEMPLATE_DIR", root.join("share/git-core/templates"));
        let certificates = root.join("ssl/cacert.pem");
        if certificates.is_file() {
            command.env("GIT_SSL_CAINFO", certificates);
        }
    }
    command.env(
        "PATH",
        path_with_tools(std::env::var_os("PATH").as_deref())?,
    );
    Ok(())
}

pub async fn ensure_tool(tool: Tool, http: Arc<dyn HttpClient>) -> Result<PathBuf> {
    let bundled = bundled_path(tool)?;
    if bundled.is_file() {
        return Ok(bundled);
    }
    match install_tool(tool, tools_directory()?, http).await {
        Ok(binary) => Ok(binary),
        Err(install_error) => {
            if let Some(system) = system_or_bundled(tool)? {
                log::warn!(
                    "Could not install bundled {}: {:#}. Falling back to system binary at {}",
                    tool.name(),
                    install_error,
                    system.display()
                );
                Ok(system)
            } else {
                Err(install_error)
            }
        }
    }
}

fn asset_suffix(tool: Tool, operating_system: &str, architecture: &str) -> Result<String> {
    match tool {
        Tool::Git => {
            let platform = match operating_system {
                "linux" => "ubuntu",
                "windows" => "windows",
                "macos" => "macOS",
                other => anyhow::bail!("Portable Git is unavailable for {other}"),
            };
            let architecture = match architecture {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                "x86" => "x86",
                "arm" if operating_system == "linux" => "arm",
                other => anyhow::bail!("Portable Git is unavailable for {other}"),
            };
            Ok(format!("-{platform}-{architecture}.tar.gz"))
        }
        Tool::Codex => {
            ensure!(
                matches!(architecture, "x86_64" | "aarch64"),
                "Codex is unavailable for {architecture}"
            );
            let target = match operating_system {
                "linux" => "unknown-linux-musl",
                "windows" => "pc-windows-msvc.exe",
                "macos" => "apple-darwin",
                other => anyhow::bail!("Codex is unavailable for {other}"),
            };
            Ok(format!("codex-{architecture}-{target}.tar.gz"))
        }
        Tool::Gpg => match operating_system {
            "windows" => Ok("gnupg-portable-win32.zip".to_string()),
            "linux" => {
                let architecture = match architecture {
                    "x86_64" => "x86_64",
                    "aarch64" => "aarch64",
                    other => anyhow::bail!("Portable GPG is unavailable for {other}"),
                };
                Ok(format!("gpg-{architecture}-unknown-linux-musl.tar.gz"))
            }
            "macos" => {
                let architecture = match architecture {
                    "x86_64" => "x86_64",
                    "aarch64" => "aarch64",
                    other => anyhow::bail!("Portable GPG is unavailable for {other}"),
                };
                Ok(format!("gpg-{architecture}-apple-darwin.tar.gz"))
            }
            other => anyhow::bail!("Portable GPG is unavailable for {other}"),
        },
    }
}

async fn install_tool(
    tool: Tool,
    directory: PathBuf,
    http: Arc<dyn HttpClient>,
) -> Result<PathBuf> {
    let _guard = INSTALL_LOCK.lock().await;
    let binary = directory
        .join(tool.name())
        .join(tool.binary(std::env::consts::OS));
    if binary.is_file() {
        return Ok(binary);
    }
    async_fs::create_dir_all(&directory).await.with_context(|| format!("Could not create tool directory for {} at {}. Ensure the directory is writable and retry", tool.name(), directory.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".tool-install-")
        .tempdir_in(&directory)?;
    let repository = tool.repository(std::env::consts::OS);
    let release = latest_github_release(repository, true, false, http.clone()).await?;
    let suffix = asset_suffix(tool, std::env::consts::OS, std::env::consts::ARCH)?;
    let (kind, asset) = release
        .assets
        .iter()
        .find_map(|asset| {
            let matches = match tool {
                Tool::Git => asset.name.starts_with("dugite-native-") && asset.name.ends_with(&suffix),
                Tool::Codex => asset.name == suffix,
                Tool::Gpg => asset.name.contains("gpg") && asset.name.ends_with(&suffix),
            };
            if matches {
                let kind = if asset.name.ends_with(".zip") {
                    AssetKind::Zip
                } else if asset.name.ends_with(".tar.bz2") {
                    AssetKind::TarBz2
                } else {
                    AssetKind::TarGz
                };
                Some((kind, asset))
            } else {
                None
            }
        })
        .context("No supported download was found for this tool and platform")?;
    ensure!(
        asset.browser_download_url.starts_with(&format!(
            "https://github.com/{}/releases/download/",
            repository
        )),
        "Unexpected tool download URL"
    );
    let digest = asset
        .digest
        .as_deref()
        .context("The tool release is missing its SHA-256 checksum")?;
    let payload = staging.path().join("payload");
    download_server_binary(
        http.as_ref(),
        &asset.browser_download_url,
        Some(digest),
        &payload,
        kind,
    )
    .await
    .with_context(|| format!("Could not download and verify {}", tool.name()))?;
    if tool == Tool::Codex {
        let source = payload.join(suffix.trim_end_matches(".tar.gz"));
        let destination = payload.join(tool.binary(std::env::consts::OS));
        async_fs::create_dir_all(destination.parent().context("Invalid tool layout")?).await?;
        async_fs::rename(source, &destination)
            .await
            .context("Codex download did not contain its executable")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            async_fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).await?;
        }
    } else if tool == Tool::Gpg {
        let destination = payload.join(tool.binary(std::env::consts::OS));
        if !destination.is_file() {
            let binary_name = tool.binary(std::env::consts::OS);
            let file_name = binary_name.file_name().context("Invalid tool binary name")?;
            let direct = payload.join(file_name);
            if direct.is_file() {
                async_fs::create_dir_all(destination.parent().context("Invalid tool layout")?).await?;
                async_fs::rename(direct, &destination)
                    .await
                    .context("Could not place gpg executable in tool layout")?;
            }
        }
        #[cfg(unix)]
        if destination.is_file() {
            use std::os::unix::fs::PermissionsExt as _;
            async_fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755)).await?;
        }
    }
    ensure!(
        payload.join(tool.binary(std::env::consts::OS)).is_file(),
        "The downloaded tool did not contain its expected executable"
    );
    async_fs::rename(payload, directory.join(tool.name()))
        .await
        .context("Could not finish installing the bundled tool")?;
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_portable_platform_assets() -> Result<()> {
        assert_eq!(
            asset_suffix(Tool::Git, "linux", "x86_64")?,
            "-ubuntu-x64.tar.gz"
        );
        assert_eq!(
            asset_suffix(Tool::Git, "windows", "aarch64")?,
            "-windows-arm64.tar.gz"
        );
        assert_eq!(
            asset_suffix(Tool::Codex, "linux", "aarch64")?,
            "codex-aarch64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset_suffix(Tool::Codex, "windows", "x86_64")?,
            "codex-x86_64-pc-windows-msvc.exe.tar.gz"
        );
        assert_eq!(
            asset_suffix(Tool::Gpg, "linux", "x86_64")?,
            "gpg-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset_suffix(Tool::Gpg, "windows", "x86_64")?,
            "gnupg-portable-win32.zip"
        );
        assert!(asset_suffix(Tool::Codex, "linux", "x86").is_err());
        assert!(asset_suffix(Tool::Git, "unknown", "x86_64").is_err());
        assert!(asset_suffix(Tool::Gpg, "unknown", "x86_64").is_err());
        Ok(())
    }
}
