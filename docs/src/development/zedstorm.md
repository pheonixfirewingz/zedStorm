# Developing ZedStorm

Build and run the desktop editor with Cargo from the repository root:

```sh
cargo build -p zed
cargo run -p zed
cargo check -p zed
```

Build the command-line launcher separately with `cargo build -p cli`.
The pinned Rust toolchain is declared in `rust-toolchain.toml`.
For native dependencies, see the guides for [Linux](./linux.md),
[macOS](./macos.md), [Windows](./windows.md), and [FreeBSD](./freebsd.md).

Run tests for the package you are changing with `cargo test -p <package>`.
Use `./script/clippy -p <package>` for Clippy checks.
Licence generation remains available through `script/generate-licenses`
and `script/generate-licenses.ps1`.

## Local builds with LTO {#local-builds-with-lto}

For repeated optimized builds, use:

```sh
cargo build-local
cargo run-local
```

These aliases build only the desktop editor with the `release-local` profile.
It keeps release optimization and cross-crate ThinLTO, enables incremental
compilation for workspace and path dependencies, and uses 16 code generation
units. Debug information remains limited. The aliases limit Cargo to two
concurrent jobs to reduce memory pressure; this does not limit every LLVM or
native compiler thread. To choose another job count, use the full command:

```sh
cargo run -p zed --bin zed --profile release-local --jobs 4
```

The repository's editor task uses `cargo run-local` and prevents concurrent
runs of that task. Build scripts and procedural macros use 16 code generation
units and omit debug information in this profile.

The first build populates `target/release-local/`. Keep that folder and use
the same profile, features, toolchain, and compiler flags to reuse its caches.
An unchanged build should reuse existing artifacts. Edits rebuild the affected
crates and their dependents, and the final executable still needs an LTO/link
step. Splitting Rust files into modules does not create separate compilation
boundaries; this workspace already uses separate crates for editor features.

Incremental compilation uses additional disk space. Release packaging continues
to use `--release`; `release-fast` remains available for benchmarks and builds
without cross-crate LTO. To inspect where build time goes, run
`cargo build-local --timings` and open the report in `target/cargo-timings/`.

## Fixed theme

The Islands Dark palette lives in `assets/themes/islands/islands.json` and is
compiled into the application. Edit it and rebuild to change colours.
Fonts and file icon themes remain configurable.

## Codex and Mistral chat {#codex-and-mistral-chat}

Use the existing chat model dropdown to choose **Mistral** or **Codex**.
Each provider keeps its own conversation and settings for the editor session.

Mistral runs entirely in Rust using the same HTTP API as its official SDKs.
No Vibe CLI, Python, or JavaScript runtime is required. Open **Settings → AI →
Vibe**, enable Vibe, enter your Mistral API key, and click **Save**. The masked
key field stores credentials in the system credential store. Use **Remove**
to delete the saved key. Disabling Vibe hides it from the model dropdown and
stops its active chat connection. Key changes reconnect Vibe automatically.
A saved key takes precedence over the optional `MISTRAL_API_KEY` environment
variable. Optionally set `MISTRAL_MODEL`
to change the default from `devstral-latest`. Available coding models load
from your Mistral account. Usage is billed to that account.

Mistral reads the original shared `SKILL.md` files from `$CODEX_HOME/skills`
(or `~/.codex/skills`), `~/.agents/skills`, and project or ancestor
`.agents/skills` directories. It reads project and ancestor `AGENTS.md`
instructions and loads skill contents and relative resources on demand.

Mistral uses zedStorm's native project tools. **Read-only** exposes only read
operations. **Default** and **Workspace write** request approval for modifying
operations. **Full access** approves native project tools automatically; these
tools still restrict file access to the project. API requests require network
access in every mode. Five-hour and weekly usage are unavailable for Mistral.

Replies stream into the existing chat. Follow-up messages, New Chat, Stop, and
provider switching use the existing controls. Conversations are retained in
memory while the panel is open. Stop cancels the API turn; an already-running
native build may continue until its existing timeout. Both providers require
a local project.

## Build infrastructure

The upstream GitHub workflows, release publishing, signing and packaging
scripts, server deployment files, Cloudflare deployment configuration, Nix
configuration, Corgi configuration, and workflow-generation tools are removed.
Builds use Cargo directly.

The local `scratch` dependency patch remains in `tooling/patches/scratch` because
native build scripts need separate working directories. Native `build.rs` files,
platform resources, local testing tools, and licence files remain part of the
project.

`target/` contains generated build output and caches. Cargo recreates it when
needed; retaining it speeds up local builds. Benchmark and extension-development
crates are optional for day-to-day editor work. Shared libraries such as `client`, RPC messages, and replicated buffer types
still support editor infrastructure and SSH remote development. Calls, channels,
contacts, project sharing, LiveKit, and WebRTC have been removed.
