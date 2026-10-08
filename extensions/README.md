# Zed Extensions

This directory contains extensions for Zed that are largely maintained by the Zed team. They currently live in the Zed repository for ease of maintenance.

If you are looking for the Zed extension registry, see the [`zed-industries/extensions`](https://github.com/zed-industries/extensions) repo.

## Structure

Currently, Zed includes support for a number of languages without requiring installing an extension. Those languages can be found under [`crates/languages/src`](https://github.com/zed-industries/zed/tree/main/crates/languages/src).

Support for all other languages is done via extensions. This directory ([extensions/](https://github.com/zed-industries/zed/tree/main/extensions/)) contains some of the officially maintained extensions. These extensions use the same [zed_extension_api](https://docs.rs/zed_extension_api/latest/zed_extension_api/) available to all [Zed Extensions](https://zed.dev/extensions) for providing [language servers](https://zed.dev/docs/extensions/languages#language-servers), [tree-sitter grammars](https://zed.dev/docs/extensions/languages#grammar) and [tree-sitter queries](https://zed.dev/docs/extensions/languages#tree-sitter-queries).

You can find the other officially maintained extensions in the [zed-extensions organization](https://github.com/zed-extensions).

## Dev Extensions

See the docs for [Developing an Extension Locally](https://zed.dev/docs/extensions/developing-extensions#developing-an-extension-locally) for how to work with one of these extensions.

## Building extensions independently

Each Rust extension has its own Cargo workspace and lockfile and is excluded
from the core ZedStorm workspace. Core builds do not resolve or compile these
extension packages or their private dependencies. Package settings and lint
settings are local to each extension.

Build an extension from the repository root by selecting its manifest:

```sh
cargo build --manifest-path extensions/glsl/Cargo.toml --target wasm32-wasip2 --locked
```

Use `extensions/html/Cargo.toml`, `extensions/proto/Cargo.toml`,
`extensions/mermaid/Cargo.toml`, or `extensions/test-extension/Cargo.toml` for
those extensions. The test extension uses the local extension SDK and remains
available to the extension host's tests and compilation benchmark. The
`workflows` directory contains shared CI configuration and is not a Rust crate.
