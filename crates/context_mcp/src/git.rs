use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use std::{fs, path::Path, process::Command};

const DEFAULT_SEARCH_DEPTH: usize = 4;
const DEFAULT_MAX_COMMITS: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repos {
    #[serde(default = "root_path")]
    pub path: String,
    #[serde(default = "default_depth")]
    pub depth: usize,
    #[serde(default)]
    pub include_nested: bool,
    #[serde(default)]
    pub offset: usize,
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    #[serde(default = "root_path")]
    pub path: String,
    #[serde(default = "default_query")]
    query: Query,
    #[serde(default = "default_commits")]
    pub max_commits: usize,
    #[serde(default)]
    offset: usize,
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Query {
    Status,
    Diff,
    Log,
    Remotes,
}

fn root_path() -> String {
    ".".into()
}
fn default_depth() -> usize {
    DEFAULT_SEARCH_DEPTH
}
fn default_commits() -> usize {
    DEFAULT_MAX_COMMITS
}
fn default_query() -> Query {
    Query::Status
}

// Discovers git repositories under an already validated project directory.
pub fn repositories(root: &Path, directory: &Path, arguments: Repos) -> Result<String> {
    ensure!(
        (1..=8).contains(&arguments.depth),
        "depth must be between 1 and 8"
    );
    let mut found = Vec::new();
    let mut scanned = 0;
    search(directory, arguments.depth, arguments.include_nested, &mut found, &mut scanned)?;
    let mut output = crate::Output::new(arguments.max_bytes)?;
    let mut index = 0;
    for (path, linked) in found {
        index += 1;
        if index <= arguments.offset {
            continue;
        }
        let relative = crate::relative_name(path.strip_prefix(root)?)?;
        let row = format!(
            "{} {}\n",
            if linked { "link" } else { "repo" },
            serde_json::to_string(&relative)?
        );
        if !output.push(&row) {
            ensure!(
                !output.text.is_empty(),
                "max_bytes is too small for this path; increase it"
            );
            return Ok(output.finish(&format!("next_offset={}", index - 1)));
        }
    }
    Ok(output.finish("complete=true"))
}

// A repository found without descending into it is the enclosing repository's
// own record: descending again would only re-find the same nested entries that
// include_nested=true exposes deliberately.
fn search(
    directory: &Path,
    remaining_depth: usize,
    include_nested: bool,
    found: &mut Vec<(std::path::PathBuf, bool)>,
    scanned: &mut usize,
) -> Result<()> {
    *scanned += 1;
    ensure!(
        *scanned <= crate::MAX_SCAN_FILES,
        "Repository scan limit reached; narrow path or depth"
    );
    let linked = match fs::symlink_metadata(directory.join(".git")) {
        Ok(metadata) => Some(!metadata.is_dir()),
        Err(_) => None,
    };
    if let Some(linked) = linked {
        found.push((directory.to_path_buf(), linked));
    }
    if remaining_depth == 0 || (linked.is_some() && !include_nested) {
        return Ok(());
    }
    let mut children = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if crate::is_git_metadata(&entry.file_name()) {
            continue;
        }
        // DirEntry metadata never traverses symlinks, so linked directories are skipped.
        if entry.metadata()?.is_dir() {
            children.push(entry.path());
        }
    }
    children.sort_by_cached_key(|child| child.file_name().map(std::ffi::OsStr::to_owned));
    for child in children {
        search(&child, remaining_depth - 1, include_nested, found, scanned)?;
    }
    Ok(())
}

// Reads one repository's state via its installed git program; no shell is used.
pub fn repository(directory: &Path, arguments: Repository) -> Result<String> {
    ensure!(
        (1..=200).contains(&arguments.max_commits),
        "max_commits must be between 1 and 200"
    );
    let rows = match arguments.query {
        Query::Status => status_rows(directory)?,
        Query::Diff => diff_rows(directory)?,
        Query::Log => log_rows(directory, arguments.max_commits)?,
        Query::Remotes => remotes_rows(directory)?,
    };
    let mut output = crate::Output::new(arguments.max_bytes)?;
    for (index, row) in rows.iter().enumerate() {
        if index < arguments.offset {
            continue;
        }
        if !output.push(&format!("{row}\n")) {
            ensure!(
                !output.text.is_empty(),
                "max_bytes is too small; increase it"
            );
            return Ok(output.finish(&format!("next_offset={}", index)));
        }
    }
    Ok(output.finish("complete=true"))
}

#[allow(
    clippy::disallowed_methods,
    reason = "This blocking runner executes only in the dedicated MCP server process, never on the editor thread or through smol command conversion"
)]
fn run_git(directory: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("-C")
        .arg(directory)
        .args(arguments);
    command.env("NO_COLOR", "1");
    let output = command
        .output()
        .context("Cannot run git; is it installed and on PATH?")?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    bail!(
        "git {} failed: {}",
        arguments.first().copied().unwrap_or("request"),
        if stderr.is_empty() {
            format!("{}", output.status)
        } else {
            let end = crate::floor_boundary(&stderr, 512);
            stderr[..end].to_owned()
        }
    );
}

fn status_rows(directory: &Path) -> Result<Vec<String>> {
    let bytes = run_git(directory, &["status", "--porcelain=v1", "-z", "--branch"])?;
    let text = String::from_utf8_lossy(&bytes);
    let mut records = text.split('\0');
    let header = records.next().unwrap_or_default();
    ensure!(
        header.starts_with("##"),
        "git status did not report a branch header"
    );
    let mut rows = vec![header.to_owned()];
    let mut staged = 0;
    let mut worktree = 0;
    let mut untracked = 0;
    let mut unmerged = 0;
    while let Some(record) = records.next() {
        let record = record.trim_end();
        if record.is_empty() {
            continue;
        }
        ensure!(
            record.len() >= 3 && record.as_bytes()[2] == b' ',
            "git status produced an unexpected record"
        );
        let code = &record[..2];
        if code == "??" {
            untracked += 1;
            rows.push(record.to_owned());
            continue;
        }
        if matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
            unmerged += 1;
            rows.push(record.to_owned());
            continue;
        }
        if code.contains('R') || code.contains('C') {
            // In -z records a rename/copy entry is followed by its second path.
            let original = records
                .next()
                .context("git status ended inside a rename record")?;
            ensure!(
                !original.trim_end().is_empty(),
                "git status rename record is missing its original path"
            );
            rows.push(format!("{code} {} -> {}", original.trim_end(), &record[3..]));
        } else {
            rows.push(record.to_owned());
        }
        if code.as_bytes()[0] != b' ' {
            staged += 1;
        }
        if code.as_bytes()[1] != b' ' {
            worktree += 1;
        }
    }
    rows.insert(
        1,
        format!("staged={staged} worktree={worktree} untracked={untracked} unmerged={unmerged}"),
    );
    Ok(rows)
}

fn diff_rows(directory: &Path) -> Result<Vec<String>> {
    let bytes = run_git(directory, &["diff", "--no-color", "--numstat", "HEAD"])?;
    let rows: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_owned)
        .collect();
    if rows.is_empty() {
        return Ok(vec!["no changes".into()]);
    }
    Ok(rows)
}

fn log_rows(directory: &Path, maximum: usize) -> Result<Vec<String>> {
    let bytes = run_git(
        directory,
        &[
            "log",
            "--date=iso-strict",
            "--pretty=format:%H%x09%ad%x09%an%x09%s",
            &format!("-n{maximum}"),
        ],
    )?;
    let rows: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_owned)
        .collect();
    if rows.is_empty() {
        return Ok(vec!["no commits yet".into()]);
    }
    Ok(rows)
}

fn remotes_rows(directory: &Path) -> Result<Vec<String>> {
    let bytes = run_git(directory, &["remote", "-v"])?;
    let rows: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect();
    if rows.is_empty() {
        return Ok(vec!["none".into()]);
    }
    Ok(rows)
}
