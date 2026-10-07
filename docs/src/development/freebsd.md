---
title: Building Zed for FreeBSD
description: "Guide to building zed for freebsd for Zed development."
---

# Building Zed for FreeBSD

FreeBSD is not currently a supported platform, so this guide is a work in progress.

## Repository

Clone the [Zed repository](https://github.com/zed-industries/zed).

## Dependencies

- Install the necessary system packages and rustup:

  ```sh
  sudo pkg install cmake gcc git llvm protobuf rustup-init libX11 alsa-lib
  rustup-init
  ```

  If preferred, you can inspect [`script/freebsd`](https://github.com/zed-industries/zed/blob/main/script/freebsd) and perform the steps manually.

## Building from source

Once the dependencies are installed, you can build Zed using [Cargo](https://doc.rust-lang.org/cargo/).

For a debug build of the editor:

```sh
cargo run
```

And to run the tests:

```sh
cargo test --workspace
```

In release mode, the primary user interface is the `cli` crate. You can run it in development with:

```sh
cargo run -p cli
```

## Troubleshooting

### Cargo errors claiming that a dependency is using unstable features

Try `cargo clean` and `cargo build`.
