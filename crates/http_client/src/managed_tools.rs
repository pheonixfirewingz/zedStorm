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
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Git => "git",
        }
    }

    fn repository(self) -> &'static str {
        match self {
            Self::Git => "desktop/dugite-native",
        }
    }

    fn binary(self, operating_system: &str) -> PathBuf {
        match (self, operating_system) {
            (Self::Git, "windows") => PathBuf::from("cmd/git.exe"),
            (Self::Git, _) => PathBuf::from("bin/git"),
        }
    }
}

static INSTALL_LOCK: futures::lock::Mutex<()> = futures::lock::Mutex::new(());

pub fn tools_directory() -> Result<PathBuf> {
    let executable =
        std::env::current_exe().context("Could not locate the application executable")?;
    Ok(executable
        .parent()
        .context("The executable has no parent directory")?
        .join("zedstorm-tools"))
}

pub fn bundled_path(tool: Tool) -> Result<PathBuf> {
    Ok(tools_directory()?
        .join(tool.name())
        .join(tool.binary(std::env::consts::OS)))
}

pub fn system_or_bundled(tool: Tool) -> Result<Option<PathBuf>> {
    match which::which(tool.name()) {
        Ok(binary) => return Ok(Some(binary)),
        Err(which::Error::CannotFindBinaryPath) => {}
        Err(error) => return Err(error).context("Could not search for a system tool"),
    }
    let binary = bundled_path(tool)?;
    Ok(binary.is_file().then_some(binary))
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
        directory.join("github-cli/bin"),
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
    if let Some(binary) = system_or_bundled(tool)? {
        return Ok(binary);
    }
    install_tool(tool, tools_directory()?, http).await
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
    async_fs::create_dir_all(&directory).await.with_context(|| format!("Could not install {} beside the application at {}. Move the application to a writable folder and retry", tool.name(), directory.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".tool-install-")
        .tempdir_in(&directory)?;
    let release = latest_github_release(tool.repository(), true, false, http.clone()).await?;
    let suffix = asset_suffix(tool, std::env::consts::OS, std::env::consts::ARCH)?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name.starts_with("dugite-native-") && asset.name.ends_with(&suffix))
        .context("No supported download was found for this tool and platform")?;
    ensure!(
        asset.browser_download_url.starts_with(&format!(
            "https://github.com/{}/releases/download/",
            tool.repository()
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
        AssetKind::TarGz,
    )
    .await
    .with_context(|| format!("Could not download and verify {}", tool.name()))?;
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
        assert!(asset_suffix(Tool::Git, "unknown", "x86_64").is_err());
        Ok(())
    }
}
