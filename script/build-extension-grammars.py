#!/usr/bin/env python3
"""Compile the tree-sitter grammars an extension declares into `<extension>/grammars/<name>.wasm`.

Installed extensions load grammars from `grammars/<name>.wasm`; nothing compiles them after
download, so release archives must include them. The compiler flags match
`ExtensionBuilder::compile_grammar` in crates/extension/src/extension_builder.rs.

Usage: WASI_SDK_PATH=/path/to/wasi-sdk build-extension-grammars.py extensions/<id>
"""

import os
import subprocess
import sys
import tomllib
from pathlib import Path


def run(command, cwd=None):
    subprocess.run(command, cwd=cwd, check=True)


def checkout(repository, revision, directory):
    if not (directory / ".git").is_dir():
        directory.mkdir(parents=True, exist_ok=True)
        run(["git", "init", "--quiet"], cwd=directory)
        run(["git", "remote", "add", "origin", repository], cwd=directory)
    # Grammar repositories can be large, so only the pinned revision is fetched when the server
    # allows fetching a commit directly; otherwise fall back to the full history.
    shallow = subprocess.run(
        ["git", "fetch", "--quiet", "--depth", "1", "origin", revision], cwd=directory
    )
    if shallow.returncode == 0:
        run(["git", "checkout", "--quiet", "--force", "FETCH_HEAD"], cwd=directory)
    else:
        run(["git", "fetch", "--quiet", "origin"], cwd=directory)
        run(["git", "checkout", "--quiet", "--force", revision], cwd=directory)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    extension_directory = Path(sys.argv[1]).resolve()
    manifest = tomllib.loads((extension_directory / "extension.toml").read_text(encoding="utf-8"))
    grammars = manifest.get("grammars", {})
    if not grammars:
        print(f"{extension_directory.name}: no grammars declared")
        return

    sdk_path = os.environ.get("WASI_SDK_PATH")
    if not sdk_path:
        sys.exit("WASI_SDK_PATH must point at a wasi-sdk installation")
    clang = Path(sdk_path) / "bin" / ("clang.exe" if os.name == "nt" else "clang")
    if not clang.is_file():
        sys.exit(f"clang not found at {clang}")

    output_directory = extension_directory / "grammars"
    output_directory.mkdir(exist_ok=True)
    for name, grammar in grammars.items():
        revision = grammar.get("rev") or grammar.get("commit")
        if not revision:
            sys.exit(f"grammar {name} has no commit or rev")
        repository_directory = output_directory / name
        checkout(grammar["repository"], revision, repository_directory)
        source_directory = repository_directory / grammar.get("path", "") / "src"
        sources = [source_directory / "parser.c"]
        scanner = source_directory / "scanner.c"
        if scanner.is_file():
            sources.append(scanner)
        output = output_directory / f"{name}.wasm"
        run(
            [
                str(clang),
                "-fPIC",
                "-shared",
                "-Os",
                f"-Wl,--export=tree_sitter_{name}",
                "-o",
                str(output),
                "-I",
                str(source_directory),
                *map(str, sources),
            ]
        )
        print(f"{extension_directory.name}: built {output.relative_to(extension_directory)}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, subprocess.CalledProcessError, tomllib.TOMLDecodeError, KeyError) as error:
        sys.exit(f"grammar build failed: {error}")
