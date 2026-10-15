mod tools_path;

use std::path::PathBuf;
use zed_extension_api::{self as zed, Command, TabRendererBackend, Worktree};

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
        let (os, _arch) = zed::current_platform();
        let os_str = match os {
            zed::Os::Mac => "mac",
            zed::Os::Linux => "linux",
            zed::Os::Windows => "windows",
        };
        let binary_name = if os == zed::Os::Windows {
            "zed-diagrams.exe"
        } else {
            "zed-diagrams"
        };

        let binary_path = tools_path::extension_directory(os_str, &[])
            .map(|dir| dir.join("bin").join(binary_name))
            .unwrap_or_else(|_| PathBuf::from(binary_name));

        Ok(TabRendererBackend::Command(Command {
            command: binary_path.to_string_lossy().into_owned(),
            args: vec!["--embedded".to_string(), file_path.to_string()],
            env: vec![],
        }))
    }
}

zed::register_extension!(DrawioExtension);
