use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fmt,
    fs::File,
    io::{BufRead as _, BufReader, Read as _},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Auto,
    Cargo,
    Npm,
    Dotnet,
    Cmake,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default = "project_root")]
    pub path: String,
    #[serde(default = "automatic")]
    backend: Backend,
    #[serde(default)]
    release: bool,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    #[serde(default = "default_budget")]
    max_bytes: usize,
}

fn project_root() -> String {
    ".".into()
}
fn automatic() -> Backend {
    Backend::Auto
}
fn default_timeout() -> u64 {
    600
}
fn default_budget() -> usize {
    8192
}

#[derive(Debug)]
pub struct BuildFailure(pub String);
impl fmt::Display for BuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for BuildFailure {}

pub fn build(directory: &Path, arguments: Build) -> Result<String> {
    ensure!(directory.is_dir(), "Build path must be a directory");
    ensure!(
        (1..=1800).contains(&arguments.timeout_seconds),
        "timeout_seconds must be between 1 and 1800"
    );
    ensure!(
        (256..=32768).contains(&arguments.max_bytes),
        "max_bytes must be between 256 and 32768"
    );
    let backend = match arguments.backend {
        Backend::Auto if directory.join("Cargo.toml").is_file() => Backend::Cargo,
        Backend::Auto if directory.join("package.json").is_file() => Backend::Npm,
        Backend::Auto if directory.join("CMakeLists.txt").is_file() => Backend::Cmake,
        Backend::Auto => {
            let mut dotnet = false;
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                if entry.path().extension().is_some_and(|extension| {
                    matches!(
                        extension.to_str(),
                        Some("sln" | "slnx" | "csproj" | "fsproj" | "vbproj")
                    )
                }) {
                    dotnet = true;
                    break;
                }
            }
            ensure!(
                dotnet,
                "No supported build manifest found (Cargo, npm, .NET, CMake)"
            );
            Backend::Dotnet
        }
        backend => backend,
    };
    let deadline = Instant::now() + Duration::from_secs(arguments.timeout_seconds);
    match backend {
        Backend::Cargo => {
            let mut command = Command::new("cargo");
            command.args(["build", "--message-format=json", "--color=never"]);
            if arguments.release {
                command.arg("--release");
            }
            run(command, directory, deadline, arguments.max_bytes, true)?;
        }
        Backend::Npm => {
            ensure!(
                !arguments.release,
                "npm builds use the project's build script; release is not supported"
            );
            let mut command = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" });
            command
                .args(["run", "build"])
                .env("NO_COLOR", "1")
                .env("FORCE_COLOR", "0");
            run(command, directory, deadline, arguments.max_bytes, false)?;
        }
        Backend::Dotnet => {
            let mut command = Command::new("dotnet");
            command.args([
                "build",
                "--nologo",
                "--verbosity",
                "quiet",
                "--configuration",
                if arguments.release {
                    "Release"
                } else {
                    "Debug"
                },
            ]);
            run(command, directory, deadline, arguments.max_bytes, false)?;
        }
        Backend::Cmake => {
            let mut configure = Command::new("cmake");
            configure.args(["-S", ".", "-B", ".zedstorm-build"]);
            configure.arg(if arguments.release {
                "-DCMAKE_BUILD_TYPE=Release"
            } else {
                "-DCMAKE_BUILD_TYPE=Debug"
            });
            run(configure, directory, deadline, arguments.max_bytes, false)?;
            let mut command = Command::new("cmake");
            command.args([
                "--build",
                ".zedstorm-build",
                "--config",
                if arguments.release {
                    "Release"
                } else {
                    "Debug"
                },
            ]);
            run(command, directory, deadline, arguments.max_bytes, false)?;
        }
        Backend::Auto => bail!("Cannot resolve build backend"),
    }
    Ok("built".into())
}

#[allow(
    clippy::disallowed_methods,
    reason = "This blocking runner executes only in the dedicated MCP server process, never on the editor thread or through smol command conversion"
)]
fn run(
    mut command: Command,
    directory: &Path,
    deadline: Instant,
    budget: usize,
    cargo: bool,
) -> Result<()> {
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    command
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x08000000 | 0x00000200);
    }
    let mut child = command
        .spawn()
        .context("Could not start build tool; ensure it is installed and on PATH")?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let mut terminate = if cfg!(windows) {
                Command::new("taskkill")
            } else {
                Command::new("kill")
            };
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                terminate.creation_flags(0x08000000);
            }
            if cfg!(windows) {
                terminate.args(["/PID", &child.id().to_string(), "/T", "/F"]);
            } else {
                terminate.args(["-KILL", "--", &format!("-{}", child.id())]);
            }
            let terminated = terminate
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("Build timed out, but could not terminate its process tree")?;
            if !terminated.success() {
                child
                    .kill()
                    .context("Build timed out, but terminating it failed")?;
            }
            child.wait()?;
            return Err(BuildFailure("Build timed out".into()).into());
        }
        thread::sleep(Duration::from_millis(25));
    };
    if status.success() {
        return Ok(());
    }
    let mut diagnostics = Diagnostics::new(budget);
    diagnostics.read(stdout, cargo)?;
    diagnostics.read(stderr, false)?;
    if diagnostics.text.is_empty() {
        return Err(BuildFailure(format!(
            "Build failed ({status}); no error diagnostic was emitted"
        ))
        .into());
    }
    Err(BuildFailure(diagnostics.finish()).into())
}

struct Diagnostics {
    text: String,
    limit: usize,
    truncated: bool,
}
impl Diagnostics {
    fn new(budget: usize) -> Self {
        Self {
            text: String::new(),
            limit: budget - 32,
            truncated: false,
        }
    }
    fn append(&mut self, text: &str) {
        if self.text.lines().any(|line| line == text.trim_end()) {
            return;
        }
        let available = self.limit.saturating_sub(self.text.len());
        let end = super::floor_boundary(text, available);
        self.text.push_str(&text[..end]);
        if end < text.len() {
            self.truncated = true;
        } else if !self.text.ends_with('\n') && self.text.len() < self.limit {
            self.text.push('\n');
        }
    }
    fn read(&mut self, mut file: File, cargo: bool) -> Result<()> {
        use std::io::Seek as _;
        file.rewind()?;
        let mut reader = BufReader::new(file);
        let mut continuation = false;
        let mut oversized = false;
        loop {
            let mut bytes = Vec::new();
            if reader
                .by_ref()
                .take(128 * 1024)
                .read_until(b'\n', &mut bytes)?
                == 0
            {
                break;
            }
            let complete = bytes.ends_with(b"\n") || bytes.len() < 128 * 1024;
            if oversized {
                oversized = !complete;
                continue;
            }
            if !complete {
                oversized = true;
                self.truncated = true;
                continue;
            }
            let line = String::from_utf8_lossy(&bytes);
            if cargo {
                if let Ok(message) = serde_json::from_str::<Value>(&line) {
                    if message["reason"] == "compiler-message"
                        && message["message"]["level"] == "error"
                    {
                        if let Some(rendered) = message["message"]["rendered"].as_str() {
                            self.append(rendered);
                        } else if let Some(message) = message["message"]["message"].as_str() {
                            self.append(message);
                        }
                    }
                    continue;
                }
            }
            let lower = line.to_ascii_lowercase();
            if lower.trim_start().starts_with("warning")
                || lower.contains(": warning ")
                || lower.trim_start().starts_with("cmake warning")
                || lower.trim_start().starts_with("0 error")
            {
                continuation = false;
                continue;
            }
            let error = [
                "error",
                "fatal",
                "exception",
                "undefined reference",
                "not found",
                "cannot find",
            ]
            .iter()
            .any(|marker| lower.contains(marker));
            if error {
                self.append(&line);
                continuation = true;
            } else if continuation
                && !line.trim().is_empty()
                && line.starts_with(char::is_whitespace)
            {
                self.append(&line);
            } else {
                continuation = false;
            }
        }
        Ok(())
    }
    fn finish(mut self) -> String {
        if self.truncated {
            self.text.push_str("[errors truncated]");
        }
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn native_build_returns_only_success_or_compiler_errors() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='mcp_build_test'\nversion='0.1.0'\nedition='2024'\n[lib]\npath='source.rs'\n",
        )?;
        std::fs::write(
            directory.path().join("source.rs"),
            "pub fn value() -> u32 { 1 }",
        )?;
        let arguments: Build = serde_json::from_value(serde_json::json!({}))?;
        assert_eq!(build(directory.path(), arguments)?, "built");
        std::fs::write(
            directory.path().join("source.rs"),
            "pub fn value() -> u32 { missing_symbol }",
        )?;
        let arguments: Build = serde_json::from_value(serde_json::json!({"max_bytes": 1024}))?;
        let error = build(directory.path(), arguments).expect_err("invalid source must fail");
        let failure = error
            .downcast_ref::<BuildFailure>()
            .context("Expected build failure")?;
        assert!(failure.0.contains("missing_symbol"));
        assert!(!failure.0.contains("compiler-artifact") && !failure.0.contains("Compiling"));
        assert!(failure.0.len() <= 1024);
        Ok(())
    }

    #[test]
    fn unsupported_projects_and_invalid_limits_fail_before_running() -> Result<()> {
        let directory = tempfile::tempdir()?;
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({"timeout_seconds": 0}),
            serde_json::json!({"timeout_seconds": 1801}),
            serde_json::json!({"max_bytes": 1}),
        ] {
            assert!(build(directory.path(), serde_json::from_value(arguments)?).is_err());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn timeout_terminates_the_build_process_group() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 60"]);
        let started = Instant::now();
        let error = run(
            command,
            directory.path(),
            started + Duration::from_millis(100),
            256,
            false,
        )
        .expect_err("build must time out");
        assert_eq!(error.to_string(), "Build timed out");
        assert!(started.elapsed() < Duration::from_secs(5));
        Ok(())
    }

    #[test]
    fn cargo_errors_exclude_artifacts_and_warnings_and_respect_budget() -> Result<()> {
        let mut file = tempfile::tempfile()?;
        writeln!(
            file,
            "{}",
            serde_json::json!({"reason":"compiler-artifact","filenames":["big-output"]})
        )?;
        writeln!(
            file,
            "{}",
            serde_json::json!({"reason":"compiler-message","message":{"level":"warning","rendered":"unused variable"}})
        )?;
        writeln!(
            file,
            "{}",
            serde_json::json!({"reason":"compiler-message","message":{"level":"error","rendered":format!("error[E0001]: {}", "detail ".repeat(200))}})
        )?;
        let mut diagnostics = Diagnostics::new(256);
        diagnostics.read(file, true)?;
        let errors = diagnostics.finish();
        assert!(errors.starts_with("error[E0001]"));
        assert!(!errors.contains("unused variable") && !errors.contains("big-output"));
        assert!(errors.ends_with("[errors truncated]") && errors.len() <= 256);
        Ok(())
    }

    #[test]
    fn generic_errors_exclude_normal_build_output() -> Result<()> {
        let mut file = tempfile::tempfile()?;
        writeln!(
            file,
            "Starting build\nwarning: unused variable\nCMake Error: missing dependency\n  install dependency\n\nBuild finished"
        )?;
        let mut diagnostics = Diagnostics::new(8192);
        diagnostics.read(file, false)?;
        assert_eq!(
            diagnostics.finish(),
            "CMake Error: missing dependency\n  install dependency\n"
        );
        Ok(())
    }
}
