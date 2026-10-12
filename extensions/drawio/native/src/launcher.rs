#![allow(
    clippy::disallowed_methods,
    reason = "This standalone launcher waits for child processes outside GPUI's foreground thread"
)]

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{Result, bail};
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};
#[path = "../../src/tools_path.rs"]
mod tools_path;

#[cfg(target_os = "linux")]
const GNOME_RUNTIME: &str = "org.gnome.Platform//50";

fn main() {
    match launch() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("diagrams.net: {error:#}");
            std::process::exit(1);
        }
    }
}

fn launch() -> Result<ExitStatus> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--help")
    {
        println!(
            "Usage: zed-diagrams [--root PROJECT] FILE.drawio\nRuntime cache: {}",
            tools_dir()?.display()
        );
        return Ok(success_status());
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "--version")
    {
        println!(env!("CARGO_PKG_VERSION"));
        return Ok(success_status());
    }
    if arguments.is_empty() {
        bail!("Usage: zed-diagrams [--root PROJECT] FILE.drawio");
    }
    let executable = std::env::current_exe()?;
    let binary = executable.with_file_name(if cfg!(windows) {
        "zed-diagrams-webview.exe"
    } else {
        "zed-diagrams-webview"
    });
    if !binary.is_file() {
        bail!(
            "Native companion missing: {}. Build and install both native binaries.",
            binary.display()
        );
    }
    let probe = Command::new(&binary).arg("--probe-webview").output()?;
    if probe.status.success() {
        let mut command = Command::new(&binary);
        if arguments
            .first()
            .is_some_and(|argument| argument == "--embedded")
        {
            command.env("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        return Ok(command.args(arguments).status()?);
    }
    eprintln!(
        "System webview unavailable; preparing a runtime in {}",
        tools_dir()?.display()
    );
    #[cfg(target_os = "linux")]
    return linux_fallback(&binary, &arguments);
    #[cfg(target_os = "windows")]
    return windows_fallback(&binary, &arguments);
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    bail!(
        "The system WebKit runtime is unavailable: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
}

fn tools_dir() -> Result<PathBuf> {
    let operating_system = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    tools_path::extension_directory(operating_system, &std::env::vars().collect::<Vec<_>>())
        .map_err(anyhow::Error::msg)
}

#[cfg(target_os = "linux")]
fn linux_fallback(binary: &Path, arguments: &[std::ffi::OsString]) -> Result<ExitStatus> {
    let cache = tools_dir()?.join("webview/flatpak");
    std::fs::create_dir_all(&cache)?;
    let flatpak = || {
        let mut command = Command::new("flatpak");
        command.env("FLATPAK_USER_DIR", &cache);
        command
    };
    let available = flatpak()
        .args(["info", "--user", GNOME_RUNTIME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context(
            "The automatic Linux WebKit download requires Flatpak. Install flatpak and retry.",
        )?;
    if !available.success() {
        checked(flatpak().args([
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            "https://flathub.org/repo/flathub.flatpakrepo",
        ]))?;
        checked(flatpak().args([
            "install",
            "--user",
            "--noninteractive",
            "--assumeyes",
            "--no-related",
            "flathub",
            GNOME_RUNTIME,
        ]))?;
    }
    let binary_directory = binary.parent().context("Missing binary directory")?;
    let current_directory = std::env::current_dir()?;
    let embedded = arguments
        .first()
        .is_some_and(|argument| argument == "--embedded");
    let file = if embedded {
        PathBuf::from(arguments.get(1).context("Missing diagram path")?)
    } else if arguments
        .first()
        .is_some_and(|argument| argument == "--root")
    {
        let root = arguments.get(1).context("Missing project root")?;
        let path = arguments.get(2).context("Missing diagram path")?;
        PathBuf::from(root).join(path)
    } else {
        PathBuf::from(arguments.first().context("Missing diagram path")?)
    };
    let file = current_directory.join(file);
    let file_directory = file
        .parent()
        .context("Missing diagram directory")?
        .canonicalize()?;
    let mut command = flatpak();
    command
        .args([
            "run",
            "--user",
            "--share=network",
            "--socket=wayland",
            "--socket=fallback-x11",
        ])
        .arg(format!("--filesystem={}:ro", binary_directory.display()))
        .arg(format!("--filesystem={}", file_directory.display()))
        .arg(format!("--command={}", binary.display()));
    if !embedded {
        command.arg("--device=dri");
    } else {
        command.arg("--env=WEBKIT_DISABLE_DMABUF_RENDERER=1");
    }
    command.arg(GNOME_RUNTIME);
    if embedded {
        command.arg("--embedded");
    }
    Ok(command.arg(file).status()?)
}

#[cfg(target_os = "windows")]
fn windows_fallback(binary: &Path, arguments: &[std::ffi::OsString]) -> Result<ExitStatus> {
    let cache = tools_dir()?.join("webview");
    std::fs::create_dir_all(&cache)?;
    let script = cache.join("install-webview2.ps1");
    std::fs::write(&script, include_str!("install-webview2.ps1"))?;
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-Destination")
        .arg(&cache)
        .arg("-Architecture")
        .arg(if cfg!(target_arch = "aarch64") {
            "arm64"
        } else if cfg!(target_arch = "x86_64") {
            "x64"
        } else {
            "x86"
        })
        .output()?;
    if !output.status.success() {
        bail!(
            "WebView2 download failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let runtime = String::from_utf8(output.stdout)?;
    Ok(Command::new(binary)
        .env("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", runtime.trim())
        .args(arguments)
        .status()?)
}

#[cfg(target_os = "linux")]
fn checked(command: &mut Command) -> Result<()> {
    command.stdout(Stdio::null());
    let status = command.status()?;
    if !status.success() {
        bail!("Runtime installation failed ({status}): {command:?}");
    }
    Ok(())
}

fn success_status() -> ExitStatus {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}
