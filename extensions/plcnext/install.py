#!/usr/bin/env python3
"""Build and install the PLCnext Engineer viewer companion binary into ZedStorm's managed tools directory."""
import os
from pathlib import Path
import shutil
import subprocess
import sys


def install():
    source = Path(__file__).resolve().parent
    home = Path.home()
    if os.environ.get("ZEDSTORM_TOOLS_DIR"):
        tools = Path(os.environ["ZEDSTORM_TOOLS_DIR"])
    else:
        if os.environ.get("ZED_DATA_DIR"):
            data = Path(os.environ["ZED_DATA_DIR"])
        elif sys.platform == "darwin":
            data = home / "Library/Application Support/Zed"
        elif sys.platform == "win32":
            data = Path(os.environ.get("LOCALAPPDATA", home / "AppData/Local")) / "Zed"
        else:
            data = Path(os.environ.get("FLATPAK_XDG_DATA_HOME") or os.environ.get("XDG_DATA_HOME") or home / ".local/share") / "zed"
        tools = data / "zedstorm-tools"

    destination = tools / "extensions/plcnext"
    destination.mkdir(parents=True, exist_ok=True)
    bin_dest = destination / "bin"
    bin_dest.mkdir(parents=True, exist_ok=True)

    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("Install Rust before building this extension")

    print("Building zed-plcnext-viewer native companion...")
    # Use workspace target dir to satisfy any application control policies
    target_dir = source.parent.parent / "target" / "ext"
    command = [
        cargo, "build",
        "--manifest-path", str(source / "native" / "Cargo.toml"),
        "--target-dir", str(target_dir),
        "--release"
    ]
    subprocess.run(command, check=True)

    binary_name = "zed-plcnext-viewer.exe" if sys.platform == "win32" else "zed-plcnext-viewer"
    compiled_binary = target_dir / "release" / binary_name

    # Copy to managed tools destination
    installed_binary = bin_dest / binary_name
    shutil.copy2(compiled_binary, installed_binary)
    if sys.platform != "win32":
        installed_binary.chmod(0o755)

    # Also copy to local extension bin/ directory for dev extension loading
    local_bin = source / "bin"
    local_bin.mkdir(parents=True, exist_ok=True)
    local_dest = local_bin / binary_name
    shutil.copy2(compiled_binary, local_dest)
    if sys.platform != "win32":
        local_dest.chmod(0o755)

    print(f"Installed PLCnext companion to: {installed_binary}")
    print(f"Local dev extension binary ready at: {local_dest}")

    # Build extension WASM component
    print("Building PLCnext extension WASM...")
    wasm_command = [
        cargo, "build",
        "--manifest-path", str(source / "Cargo.toml"),
        "--target", "wasm32-wasip2",
        "--release"
    ]
    subprocess.run(wasm_command, check=True)

    wasm_source = source.parent.parent / "target" / "wasm32-wasip2" / "release" / "zed_plcnext.wasm"
    if not wasm_source.exists():
        # Check in local target if not in root
        wasm_source = source / "target" / "wasm32-wasip2" / "release" / "zed_plcnext.wasm"

    if wasm_source.exists():
        wasm_dest = source / "extension.wasm"
        shutil.copy2(wasm_source, wasm_dest)
        print(f"Extension WASM ready at: {wasm_dest}")
    else:
        print(f"Note: WASM compiled, search location: {wasm_source}")

    print("PLCnext Engineer extension installation complete!")


if __name__ == "__main__":
    try:
        install()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Installation failed: {error}", file=sys.stderr)
        sys.exit(1)
