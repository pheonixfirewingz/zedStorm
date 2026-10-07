> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.

# ZedStorm

ZedStorm is a stripped-down, opinionated version of Zed, focused on a fast editor with integrated Codex CLI chat.

It uses a single fixed Islands Dark theme and removes Zed's account, collaboration, built-in agent, model-provider, and edit-prediction features from the application. Codex CLI handles the AI tools and authentication.

Crash dumps and hang reports are kept locally; ZedStorm does not upload them or collect usage telemetry. Zed account, update, feedback, and remote-development integrations are removed. Networking remains available for editor tools and Codex CLI.

The aim is a smaller, simpler editing experience with fewer settings and a consistent look.

### Builds

The **Build ZedStorm** GitHub Actions workflow builds Linux and Windows x86_64 versions on pushes to `custom` and pull requests targeting `custom`, and can also be run manually on that branch. Successful builds on `custom` replace the downloads in the [latest release](https://github.com/pheonixfirewingz/zedStorm/releases/tag/latest).

Linux produces a Flatpak bundle. Install it with `flatpak install --user zedstorm-linux-x86_64.flatpak` and launch it with `flatpak run dev.zedstorm.ZedStorm`. Install Codex CLI on your host to use the chat panel.

On Windows, extract the ZIP and launch `zed.exe`, keeping the terminal helper files alongside it.

To build a Flatpak locally, install Rust with rustup, `flatpak`, and `flatpak-builder`, then run:

```sh
./script/build-flatpak
```

On Ubuntu/Debian, install the Flatpak tools with `sudo apt-get install -y flatpak flatpak-builder`. The script installs the user SDK, generates licences, builds the editor, and writes `dist/zedstorm-linux-x86_64.flatpak` (or the matching host architecture).

### Licensing

Zed source code is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked.

Licence information for third-party dependencies is generated locally with `script/generate-licenses`.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If licence generation fails, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).
