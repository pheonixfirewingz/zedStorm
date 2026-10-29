use std::path::PathBuf;

pub fn extension_directory(
    operating_system: &str,
    environment: &[(String, String)],
) -> Result<PathBuf, String> {
    let variable = |name: &str| {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| PathBuf::from(value))
            .filter(|path| !path.as_os_str().is_empty())
    };

    if let Some(directory) = variable("ZEDSTORM_TOOLS_DIR") {
        return Ok(directory.join("extensions/yaml"));
    }

    let data = if let Some(directory) = variable("ZED_DATA_DIR") {
        directory
    } else {
        match operating_system {
            "mac" => variable("HOME")
                .ok_or("HOME is unavailable")?
                .join("Library/Application Support/Zed"),
            "windows" => variable("LOCALAPPDATA")
                .ok_or("LOCALAPPDATA is unavailable")?
                .join("Zed"),
            "linux" => variable("FLATPAK_XDG_DATA_HOME")
                .or_else(|| variable("XDG_DATA_HOME"))
                .or_else(|| variable("HOME").map(|home| home.join(".local/share")))
                .ok_or("Cannot determine Zed's data directory")?
                .join("zed"),
            _ => return Err("Unsupported operating system".into()),
        }
    };

    Ok(data.join("zedstorm-tools/extensions/yaml"))
}
