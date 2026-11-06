use std::collections::HashSet;
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
    let path = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::with_capacity(path.len() + 8);
    uri.push_str(if path.starts_with('/') { "file://" } else { "file:///" });
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
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

/// Returns the path to the per-user backup of the binary dictionary.
///
/// The binary dictionary lives inside the extension's installed directory, which an upgrade
/// replaces wholesale, so the backup keeps the user's saved words recoverable.
pub fn dictionary_backup_path() -> Option<PathBuf> {
    let dictionary_directory = user_dictionary_path()?.parent()?.to_path_buf();
    Some(
        dictionary_directory
            .join("english-spell-checker")
            .join("dictionary-backup.txt"),
    )
}

/// Converts a `file://` URI to a local file path.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    // Decode first: clients may percent-encode the drive colon (`/c%3A/...`).
    let decoded = percent_decode(encoded);

    #[cfg(windows)]
    let decoded = match decoded.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => decoded[1..].to_string(),
        _ => decoded,
    };

    Some(PathBuf::from(decoded))
}

/// Decodes `%XX` escapes as UTF-8. `+` is literal in URI paths, unlike in form data.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(value) = bytes
                .get(index + 1..index + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
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
        .strip_prefix('\u{feff}')
        .unwrap_or(&content)
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

    // A hand-edited file may lack a final newline, which would glue the word onto the last line.
    let needs_separator = fs::read(path)
        .map(|content| !content.is_empty() && !content.ends_with(b"\n"))
        .unwrap_or(false);
    if needs_separator {
        writeln!(file)?;
    }
    writeln!(file, "{}", trimmed)?;
    Ok(())
}

/// Appends every word from `backup_path` that `dictionary_path` is missing, returning how many
/// words were restored. A missing or unreadable backup yields zero words.
pub fn restore_dictionary_from_backup(
    dictionary_path: &Path,
    backup_path: &Path,
) -> io::Result<usize> {
    let backup_words = load_words_from_file(backup_path);
    if backup_words.is_empty() {
        return Ok(0);
    }

    let existing_words: HashSet<String> = load_words_from_file(dictionary_path)
        .into_iter()
        .map(|word| word.to_lowercase())
        .collect();

    let mut restored = 0;
    for word in backup_words {
        if existing_words.contains(&word.to_lowercase()) {
            continue;
        }
        // `append_word_to_file` also skips words the file already has, so a stale backup cannot
        // duplicate entries.
        append_word_to_file(dictionary_path, &word)?;
        restored += 1;
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_words_strips_bom() {
        let path = std::env::temp_dir().join(format!("spell-bom-{}.txt", std::process::id()));
        fs::write(&path, "\u{feff}first\nsecond\n").unwrap();
        let words = load_words_from_file(&path);
        fs::remove_file(&path).unwrap();
        assert_eq!(words, vec!["first", "second"]);
    }

    #[test]
    fn test_append_adds_separator_after_unterminated_line() {
        let path = std::env::temp_dir().join(format!("spell-append-{}.txt", std::process::id()));
        fs::write(&path, "first").unwrap();
        append_word_to_file(&path, "second").unwrap();
        let words = load_words_from_file(&path);
        fs::remove_file(&path).unwrap();
        assert_eq!(words, vec!["first", "second"]);
    }

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
    fn test_uri_round_trip_preserves_special_characters() {
        #[cfg(windows)]
        let path = PathBuf::from("C:/Users/José/C++ notes/a#b.md");
        #[cfg(not(windows))]
        let path = PathBuf::from("/home/José/C++ notes/a#b.md");
        let uri = path_to_uri(&path);
        assert!(!uri.contains(' ') && !uri.contains('#'), "{uri}");
        assert_eq!(uri_to_path(&uri), Some(path));
        #[cfg(windows)]
        assert_eq!(
            uri_to_path("file:///c%3A/a+b/x.md"),
            Some(PathBuf::from("c:/a+b/x.md"))
        );
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

    #[test]
    fn test_restore_dictionary_from_backup() {
        let temp_dir = std::env::temp_dir().join(format!(
            "spell-restore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let dictionary = temp_dir.join("bin").join("dictionary.txt");
        let backup = temp_dir.join("user").join("dictionary-backup.txt");

        fs::create_dir_all(temp_dir.join("bin")).unwrap();
        fs::create_dir_all(temp_dir.join("user")).unwrap();
        fs::write(&dictionary, "already\n").unwrap();
        fs::write(&backup, "already\nrestored\nAlsoRestored\n").unwrap();

        let restored = restore_dictionary_from_backup(&dictionary, &backup).unwrap();
        assert_eq!(restored, 2);
        let words = load_words_from_file(&dictionary);
        assert_eq!(words, vec!["already", "restored", "AlsoRestored"]);

        // Running again must not duplicate the restored words, and an absent dictionary must be
        // recreated from the backup alone.
        assert_eq!(
            restore_dictionary_from_backup(&dictionary, &backup).unwrap(),
            0
        );
        fs::remove_file(&dictionary).unwrap();
        assert_eq!(
            restore_dictionary_from_backup(&dictionary, &backup).unwrap(),
            3
        );
        assert_eq!(
            load_words_from_file(&dictionary),
            vec!["already", "restored", "AlsoRestored"]
        );

        fs::remove_dir_all(&temp_dir).unwrap();
    }
}
