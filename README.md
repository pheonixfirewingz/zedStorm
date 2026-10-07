> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.

# ZedStorm

A Zed-based editor with an integrated chat interface for Codex CLI. Zed accounts, the built-in agent panel, model providers, edit predictions, and collaboration UI are removed from the application flow.

Install [Codex CLI](https://learn.chatgpt.com/docs/codex/cli) separately and make sure `codex` is on your shell's `PATH`. Open a project, then click **Codex** in the title bar, choose **View → Codex Chat**, or run **zed: open codex** from the command palette. The shortcut is **Ctrl+Alt+J** on Linux/Windows and **Cmd+Alt+J** on macOS.

The Codex button opens a native chat panel in the right dock. Send prompts with **Enter** and add new lines with **Shift+Enter**. Replies stream with Markdown and code blocks; command activity, file changes, approvals, and errors appear in the panel. **Stop** interrupts a turn, **New Chat** starts a fresh conversation, and **Reconnect** resumes the current chat after a connection failure. The panel uses the installed CLI's `codex app-server` process and existing authentication/configuration. If needed, **Sign in to Codex** opens Codex's browser sign-in. Local projects are supported; remote projects currently show an explicit message.

The upstream AI crates and provider SDK dependencies are removed from the workspace and application dependency graph. This includes Zed agents, edit predictions, model providers, web search, sandbox launchers, and editor-managed MCP/ACP servers. Codex CLI owns the AI runtime, authentication, tools, and server configuration. Shared collaboration libraries remain for editor infrastructure, with their account and collaboration UI disabled.

ZedStorm uses a single compiled **Islands Dark** colour theme. Edit `assets/themes/islands/islands.json` and rebuild to change the palette. Theme selection, OS light/dark switching, user theme files, theme overrides, and Markdown preview theme selection do not change the application palette. Font and file icon settings remain configurable.

### Developing Zed

See [ZedStorm development scope](./docs/src/development/zedstorm.md) for optional tools, generated files, and the fixed theme location.

- [Building Zed for macOS](./docs/src/development/macos.md)
- [Building Zed for Linux](./docs/src/development/linux.md)
- [Building Zed for Windows](./docs/src/development/windows.md)

### Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for ways you can contribute to Zed.

Also... we're hiring! Check out our [jobs](https://zed.dev/jobs) page for open roles.

### Licensing

Zed source code is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked.

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

## Sponsorship

Zed is developed by **Zed Industries, Inc.**, a for-profit company.

If you’d like to financially support the project, you can do so via GitHub Sponsors.
Sponsorships go directly to Zed Industries and are used as general company revenue.
There are no perks or entitlements associated with sponsorship.
