use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Returns the path to the custom dictionary file located directly in the companion binary's directory.
pub fn binary_dictionary_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join("dictionary.txt");
        }
    }
    PathBuf::from("bin/dictionary.txt")
}

/// Converts a local filesystem path to an LSP `file://` URI.
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{}", s)
    } else {
        format!("file:///{}", s)
    }
}

/// Returns the path to the user's global custom dictionary file.
pub fn user_dictionary_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ZED_DATA_DIR") {
        return Some(PathBuf::from(dir).join("dictionary.txt"));
    }
    if let Ok(dir) = std::env::var("ZED_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("dictionary.txt"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            return Some(PathBuf::from(local_app_data).join("Zed").join("dictionary.txt"));
        }
        if let Ok(app_data) = std::env::var("APPDATA") {
            return Some(PathBuf::from(app_data).join("Zed").join("dictionary.txt"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return Some(PathBuf::from(home).join("Library/Application Support/Zed/dictionary.txt"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("zed").join("dictionary.txt"));
        }
        if let Ok(home) = std::env::var("HOME") {
            return Some(PathBuf::from(home).join(".config/zed/dictionary.txt"));
        }
    }

    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        return Some(PathBuf::from(home).join(".zed").join("dictionary.txt"));
    }

    None
}

/// Converts a `file://` URI to a local file path.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let path_str = uri.strip_prefix("file://")?;

    #[cfg(windows)]
    let path_str = if path_str.starts_with('/') && path_str.len() > 3 && path_str.as_bytes()[2] == b':' {
        &path_str[1..]
    } else {
        path_str
    };

    let decoded = url_decode(path_str);
    Some(PathBuf::from(decoded))
}

fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h1 = bytes.next();
            let h2 = bytes.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                if let Ok(hex_val) = u8::from_str_radix(&format!("{}{}", h1 as char, h2 as char), 16) {
                    result.push(hex_val as char);
                    continue;
                }
            }
        } else if b == b'+' {
            result.push(' ');
            continue;
        }
        result.push(b as char);
    }
    result
}

/// Finds the root directory of a project/workspace given a file path.
pub fn find_workspace_root(start_path: &Path) -> PathBuf {
    let mut current = if start_path.is_file() {
        start_path.parent()
    } else {
        Some(start_path)
    };

    while let Some(dir) = current {
        if dir.join(".zed").is_dir()
            || dir.join(".git").exists()
            || dir.join("Cargo.toml").exists()
            || dir.join("package.json").exists()
        {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }

    start_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Returns the path to the workspace dictionary file for a given document URI.
pub fn workspace_dictionary_path(uri: &str) -> Option<PathBuf> {
    let path = uri_to_path(uri)?;
    let root = find_workspace_root(&path);
    Some(root.join(".zed").join("dictionary.txt"))
}

/// Loads a list of words from a dictionary file (skipping blank lines and `#` comments).
pub fn load_words_from_file(path: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };

    content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|w| !w.is_empty() && !w.starts_with('#'))
        .collect()
}

/// Appends a word to the given dictionary file, creating parent directories if needed.
pub fn append_word_to_file(path: &Path, word: &str) -> io::Result<()> {
    let trimmed = word.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    // Check if the word already exists in the file to avoid duplicates
    let existing = load_words_from_file(path);
    if existing.iter().any(|w| w.eq_ignore_ascii_case(trimmed)) {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    writeln!(file, "{}", trimmed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uri_to_path() {
        #[cfg(windows)]
        {
            let p = uri_to_path("file:///C:/Users/test/main.rs").unwrap();
            assert_eq!(p, PathBuf::from("C:/Users/test/main.rs"));
        }
        #[cfg(not(windows))]
        {
            let p = uri_to_path("file:///home/user/project/main.rs").unwrap();
            assert_eq!(p, PathBuf::from("/home/user/project/main.rs"));
        }
    }

    #[test]
    fn test_append_and_load_words() {
        let temp_dir = std::env::temp_dir().join("zed_spell_test_storage");
        let dict_file = temp_dir.join(".zed").join("dictionary.txt");
        let _ = fs::remove_file(&dict_file);

        append_word_to_file(&dict_file, "customword1").unwrap();
        append_word_to_file(&dict_file, "customword2").unwrap();
        // Duplicates should be skipped
        append_word_to_file(&dict_file, "customword1").unwrap();

        let loaded = load_words_from_file(&dict_file);
        assert_eq!(loaded, vec!["customword1".to_string(), "customword2".to_string()]);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
