mod tools_path;

use std::env;
use zed_extension_api::{
    self as zed, current_platform, Command, Os, TabRendererBackend, Worktree,
};

struct DrawioExtension;

impl zed::Extension for DrawioExtension {
    fn new() -> Self {
        Self
    }

    fn tab_renderer_backend(
        &mut self,
        _renderer_id: &str,
        file_path: &str,
        _worktree: &Worktree,
    ) -> Result<TabRendererBackend, String> {
        let os = match current_platform().0 {
            Os::Windows => "windows",
            Os::Mac => "mac",
            Os::Linux => "linux",
        };
        let binary_name = if os == "windows" {
            "zed-diagrams.exe"
        } else {
            "zed-diagrams"
        };
        let env_vars: Vec<(String, String)> = env::vars().collect();
        let tools_dir = tools_path::extension_directory(os, &env_vars)?;
        let binary = tools_dir.join("bin").join(binary_name);
        Ok(TabRendererBackend::Command(Command {
            command: binary.to_string_lossy().to_string(),
            args: vec!["--embedded".to_string(), file_path.to_string()],
            env: Vec::new(),
        }))
    }
}

zed::register_extension!(DrawioExtension);
