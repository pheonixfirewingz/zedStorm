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
    Tsrs,
}

pub const ALL_TOOLS: [Tool; 4] = [Tool::Git, Tool::Codex, Tool::Gpg, Tool::Tsrs];

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Codex => "codex",
            Self::Gpg => "gpg",
            Self::Tsrs => "tsrs",
        }
    }

    pub fn repository(self, operating_system: &str) -> &'static str {
        match (self, operating_system) {
            (Self::Git, _) => "desktop/dugite-native",
            (Self::Codex, _) => "openai/codex",
            (Self::Gpg, "windows") => "portapps/gnupg-portable",
            (Self::Gpg, _) => "pheonixfirewingz/zedStorm",
            (Self::Tsrs, _) => "oxc-project/tsrs",
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
            (Self::Tsrs, "windows") => PathBuf::from("tsrs.exe"),
            (Self::Tsrs, _) => PathBuf::from("tsrs"),
        }
    }
}

static INSTALL_LOCK: futures::lock::Mutex<()> = futures::lock::Mutex::new(());

pub fn tools_directory() -> Result<PathBuf> {
    Ok(paths::zedstorm_tools_dir().clone())
}

pub fn bundled_path(tool: Tool) -> Result<PathBuf> {
    let tool_dir = tools_directory()?.join(tool.name());
    let primary = tool_dir.join(tool.binary(std::env::consts::OS));
    if primary.is_file() {
        return Ok(primary);
    }
    if tool == Tool::Tsrs {
        let bin_variant = tool_dir.join("bin").join(tool.binary(std::env::consts::OS));
        if bin_variant.is_file() {
            return Ok(bin_variant);
        }
    }
    Ok(primary)
}

pub fn system_or_bundled(tool: Tool) -> Result<Option<PathBuf>> {
    let binary = bundled_path(tool)?;
    if binary.is_file() {
        return Ok(Some(binary));
    }
    if tool == Tool::Codex {
        return Ok(None);
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
    let mut paths = vec![
        directory.join("git/bin"),
        directory.join("git/cmd"),
        directory.join("git/usr/bin"),
        directory.join("codex/bin"),
        directory.join("github-cli/bin"),
        directory.join("gpg/bin"),
        directory.join("gpg"),
        directory.join("tsrs/bin"),
        directory.join("tsrs"),
    ];
    if let Some(existing) = existing {
        paths.extend(std::env::split_paths(existing));
    }
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
            if tool != Tool::Codex {
                if let Some(system) = system_or_bundled(tool)? {
                    log::warn!(
                        "Could not install bundled {}: {:#}. Falling back to system binary at {}",
                        tool.name(),
                        install_error,
                        system.display()
                    );
                    return Ok(system);
                }
            }
            Err(install_error)
        }
    }
}

pub async fn ensure_all_tools(http: Arc<dyn HttpClient>) -> Vec<(Tool, Result<PathBuf>)> {
    let mut results = Vec::new();
    for &tool in &ALL_TOOLS {
        results.push((tool, ensure_tool(tool, http.clone()).await));
    }
    results
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
        Tool::Tsrs => {
            let pkg = match (operating_system, architecture) {
                ("windows", "x86_64") => "@tsrs/win32-x64",
                ("macos", "aarch64") => "@tsrs/darwin-arm64",
                ("macos", "x86_64") => "@tsrs/darwin-x64",
                ("linux", "x86_64") => "@tsrs/linux-x64-gnu",
                ("linux", "aarch64") => "@tsrs/linux-arm64-gnu",
                (os, arch) => anyhow::bail!("Portable tsrs is unavailable for {os}-{arch}"),
            };
            Ok(pkg.to_string())
        }
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

#[cfg(target_os = "windows")]
fn tsrs_platform_package_name() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("@tsrs/win32-x64"),
        arch => anyhow::bail!("Unsupported Windows architecture for tsrs: {arch}"),
    }
}

#[cfg(target_os = "macos")]
fn tsrs_platform_package_name() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => Ok("@tsrs/darwin-arm64"),
        "x86_64" => Ok("@tsrs/darwin-x64"),
        arch => anyhow::bail!("Unsupported macOS architecture for tsrs: {arch}"),
    }
}

#[cfg(target_os = "linux")]
async fn is_linux_musl() -> bool {
    if let Ok(output) = util::command::new_command("ldd").arg("--version").output().await {
        let text = String::from_utf8_lossy(&output.stdout);
        if text.contains("musl") {
            return true;
        }
        if text.contains("GLIBC") || text.contains("GNU libc") {
            return false;
        }
    }
    if let Ok(mut entries) = async_fs::read_dir("/lib").await {
        while let Some(Ok(entry)) = entries.next().await {
            if entry.file_name().to_string_lossy().starts_with("ld-musl-") {
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "linux")]
async fn tsrs_platform_package_name() -> Result<&'static str> {
    let is_musl = is_linux_musl().await;
    match (std::env::consts::ARCH, is_musl) {
        ("x86_64", false) => Ok("@tsrs/linux-x64-gnu"),
        ("x86_64", true) => Ok("@tsrs/linux-x64-musl"),
        ("aarch64", false) => Ok("@tsrs/linux-arm64-gnu"),
        ("aarch64", true) => Ok("@tsrs/linux-arm64-musl"),
        (arch, _) => anyhow::bail!("Unsupported Linux architecture for tsrs: {arch}"),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn tsrs_platform_package_name() -> Result<&'static str> {
    anyhow::bail!("Unsupported OS for tsrs: {}", std::env::consts::OS);
}

#[derive(serde::Deserialize)]
struct NpmLatestMetadata {
    dist: NpmDistMetadata,
}

#[derive(serde::Deserialize)]
struct NpmDistMetadata {
    tarball: String,
}

async fn install_tsrs(
    staging_path: &Path,
    directory: &Path,
    http: &dyn HttpClient,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    let package_name = tsrs_platform_package_name().await?;
    #[cfg(not(target_os = "linux"))]
    let package_name = tsrs_platform_package_name()?;

    let url = format!("https://registry.npmjs.org/{package_name}/latest");
    let mut response = http
        .get(&url, Default::default(), true)
        .await
        .with_context(|| format!("Fetching latest tsrs package metadata from {url}"))?;

    ensure!(
        response.status().is_success(),
        "Failed to fetch tsrs metadata from {url}: status {}",
        response.status()
    );

    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    let parsed: NpmLatestMetadata = serde_json::from_str(&body)
        .with_context(|| format!("Parsing npm metadata from {url}"))?;

    let archive = staging_path.join("archive");
    download_server_binary(
        http,
        &parsed.dist.tarball,
        None,
        &archive,
        AssetKind::TarGz,
    )
    .await
    .context("Could not download tsrs binary archive")?;

    let payload = staging_path.join("payload");
    async_fs::create_dir_all(&payload).await?;

    let source_dir = if archive.join("package").is_dir() {
        archive.join("package")
    } else {
        archive.clone()
    };

    let mut entries = async_fs::read_dir(&source_dir).await?;
    while let Some(entry) = entries.next().await {
        let entry = entry?;
        async_fs::rename(entry.path(), payload.join(entry.file_name())).await?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let binary_path = payload.join(Tool::Tsrs.binary(std::env::consts::OS));
        if binary_path.is_file() {
            async_fs::set_permissions(binary_path, std::fs::Permissions::from_mode(0o755)).await?;
        }
    }

    ensure!(
        payload.join(Tool::Tsrs.binary(std::env::consts::OS)).is_file(),
        "The downloaded tsrs tool did not contain its expected executable"
    );

    let destination = directory.join(Tool::Tsrs.name());
    if destination.is_dir() {
        let _ = async_fs::remove_dir_all(&destination).await;
    }
    async_fs::rename(payload, destination)
        .await
        .context("Could not finish installing tsrs tool")?;

    Ok(())
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

    if tool == Tool::Tsrs {
        install_tsrs(staging.path(), &directory, http.as_ref()).await?;
        return Ok(binary);
    }

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
                Tool::Tsrs => false,
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
        assert_eq!(
            asset_suffix(Tool::Tsrs, "windows", "x86_64")?,
            "@tsrs/win32-x64"
        );
        assert_eq!(
            asset_suffix(Tool::Tsrs, "macos", "aarch64")?,
            "@tsrs/darwin-arm64"
        );
        assert!(asset_suffix(Tool::Codex, "linux", "x86").is_err());
        assert!(asset_suffix(Tool::Git, "unknown", "x86_64").is_err());
        assert!(asset_suffix(Tool::Gpg, "unknown", "x86_64").is_err());
        Ok(())
    }

    #[test]
    fn codex_tool_never_resolves_system_binary() -> Result<()> {
        let resolved = system_or_bundled(Tool::Codex)?;
        let bundled = bundled_path(Tool::Codex)?;
        if bundled.is_file() {
            assert_eq!(resolved, Some(bundled));
        } else {
            assert_eq!(resolved, None);
        }
        Ok(())
    }

    #[test]
    fn tsrs_tool_binary_name() -> Result<()> {
        let binary = Tool::Tsrs.binary(std::env::consts::OS);
        if cfg!(windows) {
            assert_eq!(binary, PathBuf::from("tsrs.exe"));
        } else {
            assert_eq!(binary, PathBuf::from("tsrs"));
        }
        Ok(())
    }
}
