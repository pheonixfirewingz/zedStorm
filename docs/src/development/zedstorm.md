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

## Fixed theme

The Islands Dark palette lives in `assets/themes/islands/islands.json` and is
compiled into the application. Edit it and rebuild to change colours.
Fonts and file icon themes remain configurable.

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
