# ZedStorm development scope

Build the desktop editor with `cargo build -p zed`. Building the whole workspace also builds tools and services that the desktop editor does not need.

The colour palette lives in `assets/themes/islands/islands.json`. It is compiled into the application; editing it requires a rebuild. Fonts and file icon themes remain configurable.

## Optional files for desktop development

The following are outside the normal desktop application dependency graph or support a separate workflow. They are retained unless specifically removed below.

| Files | Purpose | Needed when |
| --- | --- | --- |
| `target/` | Generated build outputs and caches | Cargo recreates these; retaining them speeds up builds |
| `crates/collab/`, `Dockerfile-collab`, `.cargo/collab-config.toml`, `Procfile`, `compose.yml` | Upstream server and local service infrastructure | Developing or deploying upstream services |
| `Procfile.web` | Runs a sibling upstream website checkout | Developing that website |
| `crates/theme_importer/` | Converts external themes | Maintaining upstream theme tooling; ZedStorm uses its compiled palette |
| `crates/benchmarks/`, `crates/editor_benchmarks/`, `crates/fs_benchmarks/`, `crates/project_benchmarks/`, `crates/worktree_benchmarks/` | Performance benchmarks | Measuring regressions |
| `crates/docs_preprocessor/`, `crates/schema_generator/` | Documentation and schema tooling | Building docs or exporting schemas |
| `crates/extension_cli/`, `crates/extension_api/`, `extensions/glsl/`, `extensions/html/` | Extension development packages | Building or publishing extensions |
| `nix/`, `flake.nix`, `flake.lock`, `default.nix`, `shell.nix` | Nix development environment | Using Nix |
| `ci/`, `.github/` | CI and release automation | Running hosted checks or releasing |

The crate assessment follows local workspace dependencies from `zed`, including normal and build dependencies and excluding dev dependencies. It is not proof that a package can be deleted: tests, scripts, packaging, cross-platform builds, and Cargo workspace membership may still refer to it. Remove the corresponding manifest entries and workflow references before deleting a crate. Server deployment files do not determine whether shared editor libraries are removable.

## Removed for the fixed theme

- The unused Ayu, Gruvbox, and One JSON palettes, and the Islands Light variant.
- The colour-theme picker in the command palette and Settings, including previews and persistence.
- The colour-theme mode action, shortcuts, menu item, and OS appearance reload.
- User-theme directory loading and watching, and active extension colour-theme loading.
- Runtime colour-theme overrides and separate Markdown preview themes.

Keep `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`, `rust-toolchain.toml`, editor dependencies, language resources, fonts, icons, build scripts, and licence/attribution files. Shared libraries such as `client`, `call`, and collaboration data types still support editor infrastructure even though account and collaboration UI are disabled.
