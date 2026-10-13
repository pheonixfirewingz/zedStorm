use std::ffi::OsStr;
#[cfg(not(target_os = "macos"))]
use std::path::Path;

#[cfg(target_os = "macos")]
mod darwin;

#[cfg(target_os = "macos")]
pub use darwin::{Child, Command, Stdio};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000_u32;

pub use gpui_util::new_std_command;

pub fn prepare_host_command(command: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/.flatpak-info").is_file() {
        wrap_host_command(command);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

#[cfg(target_os = "linux")]
fn wrap_host_command(command: &mut std::process::Command) {
    let mut host_command = std::process::Command::new("/usr/bin/flatpak-spawn");
    host_command.args(["--host", "--watch-bus"]);
    if let Some(directory) = command.get_current_dir() {
        let mut argument = std::ffi::OsString::from("--directory=");
        argument.push(directory);
        host_command.arg(argument);
    }
    for (name, value) in command.get_envs() {
        // The sandbox's search paths refer to runtime libraries and wrapper binaries,
        // which must not replace the host's paths when launching host-built programs.
        if ["PATH", "LD_LIBRARY_PATH", "LD_PRELOAD"]
            .iter()
            .any(|candidate| name == OsStr::new(candidate))
            && value.is_some_and(|value| std::env::var_os(name).as_deref() == Some(value))
        {
            continue;
        }
        let mut argument = std::ffi::OsString::from(if value.is_some() {
            "--env="
        } else {
            "--unset-env="
        });
        argument.push(name);
        if let Some(value) = value {
            argument.push("=");
            argument.push(value);
        }
        host_command.arg(argument);
    }
    host_command.arg("--");
    host_command.arg(command.get_program());
    host_command.args(command.get_args());
    *command = host_command;
}

pub fn new_command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

#[cfg(not(target_os = "macos"))]
pub type Child = smol::process::Child;

#[cfg(not(target_os = "macos"))]
pub use std::process::Stdio;

#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub struct Command(smol::process::Command);

#[cfg(not(target_os = "macos"))]
impl Command {
    #[inline]
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        #[cfg(target_os = "windows")]
        {
            use smol::process::windows::CommandExt;
            let mut cmd = smol::process::Command::new(program);
            cmd.creation_flags(CREATE_NO_WINDOW);
            Self(cmd)
        }
        #[cfg(not(target_os = "windows"))]
        Self(smol::process::Command::new(program))
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.0.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    pub fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.0.get_args()
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> &mut Self {
        self.0.env(key, val);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.0.envs(vars);
        self
    }

    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.0.env_remove(key);
        self
    }

    pub fn env_clear(&mut self) -> &mut Self {
        self.0.env_clear();
        self
    }

    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.0.current_dir(dir);
        self
    }

    pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stdin(cfg.into());
        self
    }

    pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stdout(cfg.into());
        self
    }

    pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stderr(cfg.into());
        self
    }

    pub fn kill_on_drop(&mut self, kill_on_drop: bool) -> &mut Self {
        self.0.kill_on_drop(kill_on_drop);
        self
    }

    pub fn spawn(&mut self) -> std::io::Result<Child> {
        self.0.spawn()
    }

    pub async fn output(&mut self) -> std::io::Result<std::process::Output> {
        self.0.output().await
    }

    pub async fn status(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.status().await
    }

    pub fn get_program(&self) -> &OsStr {
        self.0.get_program()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn host_command_preserves_arguments_directory_and_environment() {
        let mut command = std::process::Command::new("/home/user/project with spaces/program");
        command
            .args(["argument with spaces", "$(must stay literal)"])
            .current_dir("/home/user/project with spaces")
            .env("WORKFLOW_VALUE", "a=b c")
            .env_remove("WORKFLOW_REMOVED");
        wrap_host_command(&mut command);
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/flatpak-spawn"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--host",
                "--watch-bus",
                "--directory=/home/user/project with spaces",
                "--unset-env=WORKFLOW_REMOVED",
                "--env=WORKFLOW_VALUE=a=b c",
                "--",
                "/home/user/project with spaces/program",
                "argument with spaces",
                "$(must stay literal)",
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn host_command_uses_host_search_path() -> anyhow::Result<()> {
        let mut command = std::process::Command::new("cargo");
        command.env(
            "PATH",
            std::env::var_os("PATH").ok_or_else(|| anyhow::anyhow!("PATH is unset"))?,
        );
        wrap_host_command(&mut command);
        assert!(
            !command
                .get_args()
                .any(|argument| argument.to_string_lossy().starts_with("--env=PATH="))
        );

        let mut command = std::process::Command::new("cargo");
        command.env("PATH", "/custom/toolchain/bin");
        wrap_host_command(&mut command);
        assert!(
            command
                .get_args()
                .any(|argument| argument == "--env=PATH=/custom/toolchain/bin")
        );
        Ok(())
    }
}
