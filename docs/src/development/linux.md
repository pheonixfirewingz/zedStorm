---
title: Building ZedStorm for Linux
description: "Build ZedStorm locally on Linux with Cargo."
---

# Building ZedStorm for Linux

## Repository

Open your ZedStorm checkout and run these commands from the repository root.

## Dependencies

- Install [rustup](https://www.rust-lang.org/tools/install)

- Install the necessary system libraries:

  ```sh
  script/linux
  ```

  If you prefer to install the system libraries manually, you can find the list of required packages in the `script/linux` file.

## Building from source

Once the dependencies are installed, you can build Zed using [Cargo](https://doc.rust-lang.org/cargo/).

For a debug build of the editor:

```sh
cargo run -p zed
```

And to run the tests:

```sh
cargo test -p <package>
```

In release mode, the primary user interface is the `cli` crate. You can run it in development with:

```sh
cargo run -p cli
```

## Local builds

Build the editor and command-line launcher with Cargo:

```sh
cargo build -p zed -p cli
```

The upstream installer and release packaging scripts are removed. Run the editor
from the checkout with `cargo run -p zed`.

## Wayland & X11

Zed supports both X11 and Wayland. By default, we pick whichever we can find at runtime. If you're on Wayland and want to run in X11 mode, use the environment variable `WAYLAND_DISPLAY=''`.

## Memory profiling

[`heaptrack`](https://github.com/KDE/heaptrack) is quite useful for diagnosing memory leaks. To install it:

```sh
$ sudo apt install heaptrack heaptrack-gui
$ cargo install cargo-heaptrack
```

Then, to build and run Zed with the profiler attached:

```sh
$ cargo heaptrack -b zed
```

When this zed instance is exited, terminal output will include a command to run `heaptrack_interpret` to convert the `*.raw.zst` profile to a `*.zst` file which can be passed to `heaptrack_gui` for viewing.

## Perf recording

How to get a flamegraph with resolved symbols from a running Zed instance.
Use this when Zed is using a lot of CPU. It is not useful for hangs.

### During the incident

- Find the PID (process ID) using:
  `ps -eo size,pid,comm | grep zed | sort | head -n 1 | cut -d ' ' -f 2`
  Or find the PID of `zed-editor` with the highest RAM usage in something
  like htop/btop/top.

- Install perf:
  On Ubuntu (derivatives) run `sudo apt install linux-tools`.

- Perf record:
  Run `sudo perf record -g --call-graph dwarf -p <pid you just found>`, wait a few seconds to gather data, then press Ctrl+C. You should now have a `perf.data` file.
  The `--call-graph dwarf` option records callers of every sampled function and unwinds through the `.eh_frame` data kept in stripped release binaries; plain `-g` does not work without frame pointers.

- Make the output file user owned:
  run `sudo chown $USER:$USER perf.data`

- Get build info:
  Run zed again and type {#action zed::About} in the command pallet to get the exact commit.

Keep the `perf.data` file together with the exact commit used for your build.

### Local symbols

Alternatively, rebuild the binary with symbols:

- Check out the commit found previously and modify `Cargo.toml`.
  Apply the following diff, then make a release build.

```diff
[profile.release]
-debug = "limited"
+debug = "full"
```

- Add the symbols to the perf database:
  `perf buildid-cache -v -a <path to release zed binary>`

- Resolve the symbols from the db:
  `perf inject -i perf.data -o perf_with_symbols.data`

- Install flamegraph:
  `cargo install cargo-flamegraph`

- Render the flamegraph:
  `flamegraph --perfdata perf_with_symbols.data`

## Troubleshooting

### Cargo errors claiming that a dependency is using unstable features

Try `cargo clean` and `cargo build`.
