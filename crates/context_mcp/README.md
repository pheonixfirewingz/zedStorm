# ZedStorm context MCP

ZedStorm includes a native stdio MCP server in its own executable. The Codex panel
registers `zedstorm_context` automatically when it starts `codex app-server`.
There is no separate server to install and no change to `~/.codex/config.toml`.
Other configured MCP servers, including Serena, remain available.

The server abstracts common filesystem commands and project builds into eleven
native tools. Filesystem tools do not invoke a shell. Build tools invoke the
project's installed build program and keep normal build logs out of model context.

| Tool | Command abstraction | Result |
| --- | --- | --- |
| `list` | `ls`, `tree` | Entry types, sizes, and paths; depth defaults to one |
| `stat` | `stat` | Type, byte size, and read-only flag without reading contents |
| `files` | `find` | Paths matching a substring, with offset pagination |
| `search` | `grep`, `rg` | Matching lines and byte columns with short previews |
| `read` | `cat`, `head`, `sed` ranges | Selected lines, revision, and continuation cursor |
| `edit` | Text replacement | One exact replacement or creation; short receipt |
| `delete` | `rm` | One revision-checked text file deletion |
| `mkdir` | `mkdir`, `mkdir -p` | Create a directory, optionally including parents |
| `copy` | `cp` | Copy one revision-checked text file without overwriting |
| `move` | `mv` | Copy then delete one revision-checked text file |
| `build` | Project build | Only `built` on success, or bounded compiler errors |

The panel starts the server read-only. To expose mutation and build tools, launch
ZedStorm with `ZEDSTORM_CONTEXT_MCP_WRITE=1` in its environment. Reconnect the
Codex panel after restarting with the variable set. This grants the MCP process
write access within the project; it does not change Codex's shell sandbox or
approval policy. MCP writes are performed by the server and are not routed through
Codex's shell/file-edit approval mechanism. Leave the variable unset when those
approvals are required for every file mutation.

For direct MCP use, launch the actual editor executable (rather than its CLI
launcher) with:

```sh
/path/to/zed --context-mcp /absolute/project/path
/path/to/zed --context-mcp /absolute/project/path --context-mcp-write
```

The process handles newline-delimited JSON-RPC on stdin/stdout and exits at EOF.
It starts before the editor UI and logging initialization, so stdout contains only
protocol responses. Errors are returned as tool errors or written to stderr if
the server cannot start.

## Using less context

Find paths first, search a specific directory, and read only the relevant range.
The default output budget is 8 KiB per tool call; `max_bytes` accepts 256–32768.
This bounds UTF-8 response text, not an exact model token count or the JSON-RPC
envelope. The server returns text once, without duplicating it in structured
content. Edit/delete receipts do not echo file contents.

For `list`, `files`, and `search`, pass `next_offset` back as `offset` with the same
arguments. Use `list` with `depth: 1` for `ls` or a larger depth (up to eight) for
`tree`; `include_hidden: true` includes hidden entries. Listing includes ignored
entries, while `files` and `search` filter them. Listed paths are JSON-quoted;
entry types are `d` (directory), `f` (file), `l` (symlink), or `other`.
For `read`, pass `next_line` as `start_line` and `next_column` as
`start_column`; columns are zero-based UTF-8 byte offsets. `end_line` is inclusive.
Search previews also include a zero-based byte column so a long line can be read
near the match. `[line truncated]` means the preview is incomplete. Restart paged
searches if the project changes between calls. `complete=true` applies to the
requested range/query, subject to reported encoding/size exclusions.

Save the revision from a read and pass it to `edit` or `delete`. Stale revisions
are rejected. For an existing file, `old_text` must occur exactly once, including
overlapping matches. To create a file, pass `revision: "new"`, `old_text: ""`, and
the contents in `new_text`; its parent directory must already exist. Existing
files are never overwritten by creation. Replacements use a temporary file in
the same directory and preserve existing permissions.

Pass `if_revision` to `read` only when that exact content is already present in
the model's current context. A matching revision yields `unchanged=true`. There
is no implicit suppression of repeated reads, so compaction or reconnection
cannot cause the server to hide content the model no longer has.

## Scope and limits

File discovery/search follows git/ignore rules and skips hidden files. Explicit reads can
address hidden project files. Paths must be relative to the project, without
parent traversal or symlinks. Git metadata, directories, binary files, and
non-UTF-8 files cannot be read, edited, or deleted. Text files are limited to
8 MiB. Discovery stops after 100,000 files and reports the scan limit. Search
reports excluded files and propagates I/O failures instead of silently hiding
them. Narrow the path when a scan limit is reached. `stat` can inspect binary
files without reading their content. `list` shows symlinks but never traverses
them. Git metadata is excluded from listings, including hidden listings.

`copy` and `move` accept `source`, `destination`, and the source's read `revision`.
They handle UTF-8 text files within the same limits as reads, preserve permissions,
and require an existing destination parent. They never overwrite a destination
or operate recursively on directories. `move` creates the destination before
removing the source; if source removal fails, its error explicitly reports that
the destination was created and the source was retained. It is not an atomic rename.

The root and revision checks protect against accidental operations, but are not
an OS sandbox: another process can change filesystem paths between checks and
operations. The MCP process runs with the editor user's filesystem permissions.
It works on disk and does not coordinate with unsaved editor buffers.

RTK and Serena are complementary: use RTK for compact supported shell output and
Serena for language-server symbol definitions/references. This built-in server
does text search; it does not claim semantic code navigation or increase the
model's context window. Savings depend on the model choosing focused tools and
appropriate budgets.

```sh
cargo test -p context_mcp
./script/clippy -p context_mcp
```

## Project builds

Call `build` with no arguments to build the project root, or specify a
project-relative `path`. `backend` defaults to `auto`, detecting Cargo.toml,
package.json, CMakeLists.txt, or a .NET solution/project. Set `backend` explicitly
to `cargo`, `npm`, `dotnet`, or `cmake` when multiple manifests are present.
`release: true` selects release builds for Cargo, .NET, and CMake. npm invokes
the existing `build` script and does not accept the release option. CMake uses
`.zedstorm-build` for its build directory.

The tool waits for completion and returns exactly `built` on success. On failure,
it returns error diagnostics as an MCP tool error, excluding normal progress and
warning logs. Cargo compiler diagnostics are extracted from its JSON output.
For other builders, recognizable error lines and their indented details are
extracted. A failure without a recognizable diagnostic reports the exit status.
`max_bytes` bounds returned errors (default 8192); truncation is explicit.
Percentages are not fabricated when a builder cannot report reliable progress.

`timeout_seconds` defaults to 600 and accepts 1–1800; timed-out builds have their
process tree terminated. Build tools must already be installed and on PATH.
Build scripts run with the MCP process's permissions, including project build
scripts and dependency build scripts. Builds are not run through Codex's shell
approval mechanism. Normal compiler logs are captured in temporary files and
discarded after the command; the output budget limits response text, not log disk
space. The panel sets the MCP tool timeout to 1900 seconds so long builds can finish.

## Windows

The same native tools are compiled into `zed.exe`. Both `/` and `\` are accepted
in project-relative inputs; discovery always returns `/`. Paths with spaces and
UTF-8 names are supported, and edits preserve existing CRLF line endings.
Windows device names, alternate data streams, trailing-dot/space components,
absolute/drive-relative inputs, and case variants of Git metadata are rejected.
Symlinks and directory junctions are not traversed. Copies restore the Windows
read-only attribute after temporary-file persistence.

To enable build and mutation tools when launching the editor from PowerShell:

```powershell
$env:ZEDSTORM_CONTEXT_MCP_WRITE = '1'
& 'C:\path\to\zed.exe'
```

The component restrictions follow
[Windows filename rules](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file).

The Windows build workflow runs crate tests and
`script/test-context-mcp.ps1` against the actual release editor executable. The
smoke test checks all eleven tools, successful/failed Cargo builds, UTF-8 paths,
CRLF, read-only copies, junction restrictions, piped stdio, and EOF shutdown.
