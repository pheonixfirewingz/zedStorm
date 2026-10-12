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
        return Ok(directory.join("extensions/drawio"));
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
    Ok(data.join("zedstorm-tools/extensions/drawio"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_data_and_tools_overrides() -> Result<(), String> {
        let environment = vec![("HOME".into(), "/home/test".into())];
        assert_eq!(
            extension_directory("linux", &environment)?,
            PathBuf::from("/home/test/.local/share/zed/zedstorm-tools/extensions/drawio")
        );
        let environment = vec![("ZEDSTORM_TOOLS_DIR".into(), "/shared/tools".into())];
        assert_eq!(
            extension_directory("linux", &environment)?,
            PathBuf::from("/shared/tools/extensions/drawio")
        );
        let environment = vec![("ZED_DATA_DIR".into(), "/custom/zed".into())];
        assert_eq!(
            extension_directory("linux", &environment)?,
            PathBuf::from("/custom/zed/zedstorm-tools/extensions/drawio")
        );
        Ok(())
    }
}
