# ZedStorm context MCP

ZedStorm includes a native stdio MCP server in its own executable. The Codex panel
registers `zedstorm_context` automatically when it starts `codex app-server`.
There is no separate server to install and no change to `~/.codex/config.toml`.
Other configured MCP servers remain available.

The server provides filesystem, build, and semantic navigation tools.
Filesystem tools do not invoke a shell. Build tools invoke the
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
| `web_search` | Web search | Composed answer from the Codex web-search backend (hidden without Codex credentials) |

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
The default output budget is 4 KiB per tool call; `max_bytes` accepts 256–32768.
This bounds UTF-8 response text, not an exact model token count or the JSON-RPC
envelope. The server returns text once, without duplicating it in structured
content. Edit/delete receipts do not echo file contents.

Tool descriptions contain only tool-specific guidance. The server omits optional
initialization instructions because clients may prepend them to every tool,
multiplying their context cost. Position conventions, revision requirements,
and continuation guidance remain in the relevant tool descriptions and schemas.

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
Filesystem tools work on disk and do not coordinate with unsaved editor buffers.
Use `read_buffer` for the editor's current source.

Use semantic tools for code navigation and RTK for compact supported shell output.
Savings depend on the model choosing focused tools and appropriate budgets.

## Semantic navigation and external clients

Open the same project in an updated ZedStorm editor. Its workspace starts a local
semantic bridge automatically; a Codex login or active chat is not required.
The stdio MCP process discovers that bridge for its canonical project root and
reconnects on each call. Closing the workspace stops the bridge. Filesystem tools
continue working without the editor; semantic tools report an explicit error.
If several editor windows open the same root, the most recently started bridge
serves external clients. Use one window per root when querying unsaved changes.

| Tool | Result |
| --- | --- |
| `semantic_status` | Running language servers, pending work and diagnostic activity |
| `read_buffer` | Current source, including unsaved edits, with revision and continuation |
| `symbols` | Document symbols, nested name paths, full ranges and selection ranges |
| `workspace_symbols` | Symbols matching a query across running language servers |
| `definitions`, `declarations`, `type_definitions` | Semantic target locations with source excerpts |
| `references`, `implementations` | Referencing or implementing locations with source excerpts |
| `hover` | Type information, signatures and documentation |
| `incoming_calls`, `outgoing_calls` | Callers/callees and their call sites |
| `rename` | Revision-checked symbol rename in undoable, unsaved editor buffers |
| `open_file`, `open_tab` | Open a file in the active editor workspace tab, optionally navigating to line and column |

Source positions use **one-based lines and zero-based UTF-16 columns**. These
columns differ from filesystem search/read byte columns when a line contains
non-ASCII characters. Pass a symbol's `selection_range.start` to navigation tools.
`read_buffer` can start at `line` and `column`, or use a UTF-8 byte `offset` to
continue; do not combine a position with a nonzero offset. Subsequent pages use
`next_offset` alone. Other semantic tools paginate result records with `offset`.
Restart pagination after source or language-server index changes.

For example, call `symbols` with `{"path":"src/main.rs"}`, then `definitions`
with `{"path":"src/main.rs","line":12,"column":4}`. Use `read_buffer` with
`{"path":"src/main.rs","line":12}` to inspect live source around that result.
Dependency locations can be absolute paths; tool inputs remain scoped to the
selected project root. Results come from the first capable language server for
position-based requests. Workspace symbol search queries all supporting servers.

Language support follows the editor's configured language servers. Trust the
project in the editor when prompted, then open a source
file to start its server, then retry if it is still starting or indexing.
Unsupported requests and server failures are errors, rather than fabricated
text-search results. `semantic_status` exposes reported progress; a running server
does not guarantee that its index is complete. Requests have a bounded timeout.

`rename` requires both `--context-mcp-write` in the MCP client and
`ZEDSTORM_CONTEXT_MCP_WRITE=1` in the editor. Read the source with `read_buffer`
first and pass its `revision` along with `path`, `line`, `column`, and `new_name`.
The server checks the source revision before preparation and rejects source
changes during the rename request. It validates all proposed file paths before
applying edits, rejecting changes outside the root, symlinks, Git metadata, and
file creation/deletion/moves. A language server requiring a file rename must be
used through the editor instead. Edited files open for review and remain unsaved;
save or undo them in the editor. After a timeout or connection loss, inspect the
editor before retrying a mutation. Filesystem race protection remains best-effort.

External MCP clients use the same executable and stdio arguments:

```json
{
  "mcpServers": {
    "zedstorm_context": {
      "command": "/absolute/path/to/zed",
      "args": ["--context-mcp", "/absolute/project/path"]
    }
  }
}
```

For T3 Code's Codex provider, configure the server in the Codex configuration
loaded by that provider. A TOML entry has the following form:

```toml
[mcp_servers.zedstorm_context]
command = "/absolute/path/to/zed"
args = ["--context-mcp", "/absolute/project/path"]
```

Use the actual editor executable, not a launcher. The provider and editor must
run on the same host/user account and share ZedStorm's data directory. Launch the
client with `--user-data-dir /absolute/data/path` when the editor uses a custom
data directory; the built-in panel passes its data directory automatically. Start a
new provider session after changing its MCP configuration. A fixed root entry
always queries that project, even from a thread in another directory. T3 Code
versions that replace the per-thread `mcp_servers` table can discard servers added
only through launch arguments; see the
[upstream configuration issue](https://github.com/pingdotgg/t3code/issues/15828).
This implementation supplies the stdio server; it does not modify T3 Code or the
user's global provider configuration. Validate the loaded tools before disabling
Serena. It replaces semantic navigation and symbol rename, not Serena's memory
or arbitrary symbol-body editing features.

The bridge listens only on loopback and authenticates requests with a random
project-specific credential stored in a private endpoint file in ZedStorm's data
directory. Credentials are not returned in tool output. Reopening a workspace
publishes a fresh endpoint; stale processes report an unavailable bridge.

```sh
cargo test -p context_mcp
./script/clippy -p context_mcp
cargo test -p ai
node script/test-semantic-mcp.mjs /absolute/path/to/zed
```

The semantic smoke test requires Node.js, a graphical desktop session, and a
working Rust language server. Pass an installed rust-analyzer path as an optional
second argument to avoid downloading another server. It starts an isolated editor with a temporary data
directory/project, tests two stdio clients and unsaved rename, and cleans up its
own processes and fixtures. It does not use the user's current workspace.

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
`max_bytes` bounds returned errors (default 4096); truncation is explicit.
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
smoke test checks filesystem/build tools, semantic tool discovery and unavailable-editor
errors, successful/failed Cargo builds, UTF-8 paths,
CRLF, read-only copies, junction restrictions, piped stdio, and EOF shutdown.

## Web search

`web_search` uses the same Codex web-search backend as the Codex CLI (`codex-rs/ext/web-search` posting to the provider-relative `alpha/search` endpoint in `codex-api`'s `SearchClient`).

The server reads the saved Codex CLI credentials from `$CODEX_HOME/auth.json` (default `~/.codex/auth.json`), so a Codex sign-in or an OpenAI API key is required. With a ChatGPT subscription it sends the saved access token plus `chatgpt-account-id`; with an API key it sends that key as a bearer token. The tool is exposed only when credentials can be found, so it is hidden from `tools/list` otherwise and calls fail with an actionable message.

If the Codex CLI stores its selected model in `$CODEX_HOME/config.toml`, the backend composes the answer with that model unless the call overrides it via the `model` argument.

Environment overrides:

| Variable | Effect |
| --- | --- |
| `ZEDSTORM_WEB_SEARCH_BASE_URL` | Overrides the Codex backend base URL |
| `ZEDSTORM_WEB_SEARCH_MODEL` | Overrides the composed-answer model |
| `OPENAI_BASE_URL` | Overrides the base URL for API-key credentials |

Notes:

- Requests carry only the tool call's search commands; the MCP server does not see the conversation, so no conversation history is forwarded.
- Response text is returned verbatim, bounded to the server's output budget, and truncated explicitly when the limit is hit.
- 401/403 responses point at expired credentials; run `codex login` and retry.
- A credential store backed by a system keyring rather than the auth file may leave the tool unavailable; use the auth file.
