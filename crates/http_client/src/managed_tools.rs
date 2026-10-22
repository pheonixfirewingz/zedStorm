use crate::{
    HttpClient,
    github::{AssetKind, GithubRelease, latest_github_release},
    github_download::download_server_binary,
};
use anyhow::{Context as _, Result, ensure};
use futures::{AsyncReadExt as _, StreamExt as _};
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
            "windows" => Ok(".7z".to_string()),
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

async fn checksums_file_digest(
    http: &dyn HttpClient,
    release: &GithubRelease,
    asset_name: &str,
) -> Result<String> {
    let checksums_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == "checksums.txt")
        .context("The tool release does not attach per-asset digests and has no checksums.txt to verify against")?;
    let mut response = http
        .get(
            &checksums_asset.browser_download_url,
            Default::default(),
            true,
        )
        .await
        .with_context(|| {
            format!(
                "Could not download checksums.txt from release {}",
                release.tag_name
            )
        })?;
    let mut checksums = String::new();
    response
        .body_mut()
        .read_to_string(&mut checksums)
        .await
        .with_context(|| "Could not read the tool release checksums")?;
    checksums
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find_map(|fields| match fields.as_slice() {
            [digest, name]
                if *name == asset_name
                    && digest.len() == 64
                    && digest
                        .chars()
                        .all(|character| character.is_ascii_hexdigit()) =>
            {
                Some((*digest).to_string())
            }
            _ => None,
        })
        .with_context(|| format!("checksums.txt has no entry for {asset_name}"))
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
                Tool::Git => {
                    asset.name.starts_with("dugite-native-") && asset.name.ends_with(&suffix)
                }
                Tool::Codex => asset.name == suffix,
                Tool::Gpg => {
                    if std::env::consts::OS == "windows" {
                        gpg_windows_asset_matches(&asset.name)
                    } else {
                        asset.name.contains("gpg") && asset.name.ends_with(&suffix)
                    }
                }
            };
            if matches {
                let kind = if asset.name.ends_with(".zip") {
                    AssetKind::Zip
                } else if asset.name.ends_with(".tar.bz2") {
                    AssetKind::TarBz2
                } else if asset.name.ends_with(".7z") {
                    AssetKind::SevenZip
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
    let digest = match asset.digest.as_deref() {
        Some(digest) => digest.to_string(),
        None => checksums_file_digest(http.as_ref(), &release, &asset.name).await?,
    };
    let payload = staging.path().join("payload");
    download_server_binary(
        http.as_ref(),
        &asset.browser_download_url,
        Some(digest.as_str()),
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
        let binary = tool.binary(std::env::consts::OS);
        let destination = payload.join(&binary);
        if !destination.is_file() {
            let binary_name = binary.file_name().context("Invalid tool binary name")?;
            let bin_directory = destination
                .parent()
                .context("Invalid tool binary name")?
                .to_path_buf();
            // portapps archives extract GnuPG to `<root>/app/bin` instead of
            // the `<root>/bin` layout the bundled tool expects.
            let portable_bin_directory = payload.join("app").join("bin");
            if portable_bin_directory.join(binary_name).is_file() {
                async_fs::create_dir_all(&bin_directory).await?;
                let mut entries = async_fs::read_dir(&portable_bin_directory).await?;
                while let Some(entry) = entries.next().await {
                    let entry = entry?;
                    async_fs::rename(entry.path(), bin_directory.join(entry.file_name()))
                        .await
                        .with_context(|| {
                            format!(
                                "Moving {} out of the portable tool layout",
                                entry.file_name().to_string_lossy()
                            )
                        })?;
                }
                // Drop the rest of the portable app (docs, locales, launcher)
                // so only the tool layout is kept.
                async_fs::remove_dir_all(payload.join("app"))
                    .await
                    .with_context(|| "Could not remove the leftover portable app files")?;
            } else {
                let direct = payload.join(binary_name);
                if direct.is_file() {
                    async_fs::create_dir_all(&bin_directory).await?;
                    async_fs::rename(direct, &destination)
                        .await
                        .context("Could not place gpg executable in tool layout")?;
                }
            }
        }
        #[cfg(unix)]
        {
            if let Some(bin_directory) = payload
                .join(&binary)
                .parent()
                .map(|parent| parent.to_path_buf())
                .filter(|bin_directory| bin_directory.is_dir())
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mut entries = async_fs::read_dir(&bin_directory).await?;
                while let Some(entry) = entries.next().await {
                    let entry = entry?;
                    async_fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o755))
                        .await?;
                }
            }
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

/// Only versioned portapps archives qualify, so the checksum parsed from
/// `checksums.txt` refers to the asset that is actually downloaded.
fn gpg_windows_asset_matches(asset_name: &str) -> bool {
    asset_name
        .strip_prefix("gnupg-portable-win32-")
        .and_then(|name| name.strip_suffix(".7z"))
        .is_some_and(|version| {
            version
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.' || character == '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gpg_asset_matches_portapps_releases() -> Result<()> {
        // Real asset names from portapps/gnupg-portable.
        assert!(gpg_windows_asset_matches(
            "gnupg-portable-win32-2.4.3-12.7z"
        ));
        assert!(gpg_windows_asset_matches(
            "gnupg-portable-win32-3.1.2-21.7z"
        ));
        assert!(!gpg_windows_asset_matches(
            "gnupg-portable-win32-2.4.3-12.7z.exe"
        ));
        assert!(!gpg_windows_asset_matches("gnupg-portable-win32.exe"));
        assert!(!gpg_windows_asset_matches(
            "gnupg-portable-win32-2.4.3-12-setup.exe"
        ));
        assert!(!gpg_windows_asset_matches("checksums.txt"));
        Ok(())
    }
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
        assert_eq!(asset_suffix(Tool::Gpg, "windows", "x86_64")?, ".7z");
        assert!(asset_suffix(Tool::Codex, "linux", "x86").is_err());
        assert!(asset_suffix(Tool::Git, "unknown", "x86_64").is_err());
        assert!(asset_suffix(Tool::Gpg, "unknown", "x86_64").is_err());
        Ok(())
    }
}
