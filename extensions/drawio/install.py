#!/usr/bin/env python3
"""Build the native companion into ZedStorm's managed tools directory."""
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
            data = Path(os.environ["LOCALAPPDATA"]) / "Zed"
        else:
            data = Path(os.environ.get("FLATPAK_XDG_DATA_HOME") or os.environ.get("XDG_DATA_HOME") or home / ".local/share") / "zed"
        tools = data / "zedstorm-tools"
    destination = tools / "extensions/drawio"
    destination.mkdir(parents=True, exist_ok=True)
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("Install Rust with rustup before building this extension")
    command = [cargo, "install", "--locked", "--path", str(source / "native"), "--root", str(destination)]
    if sys.platform.startswith("linux"):
        available = subprocess.run(["pkg-config", "--exists", "webkit2gtk-4.1"], check=False).returncode == 0
        if not available:
            if not shutil.which("flatpak"):
                raise RuntimeError("Install Flatpak to download WebKitGTK, or install libwebkit2gtk-4.1-dev using your distribution's package manager")
            environment = dict(os.environ, FLATPAK_USER_DIR=str(destination / "webview/flatpak"))
            flatpak = ["flatpak"]
            subprocess.run(flatpak + ["remote-add", "--user", "--if-not-exists", "flathub", "https://flathub.org/repo/flathub.flatpakrepo"], env=environment, check=True)
            subprocess.run(flatpak + ["install", "--user", "--noninteractive", "--assumeyes", "--no-related", "flathub", "org.gnome.Sdk//50", "org.gnome.Platform//50"], env=environment, check=True)
            # Build against the same ABI as the downloaded runtime, rather than the host distribution.
            command = flatpak + ["run", "--user", "--share=network", "--filesystem=home", f"--filesystem={source.parent.parent}",
                f"--filesystem={destination}", f"--env=PATH={os.environ['PATH']}", f"--env=CARGO_HOME={os.environ.get('CARGO_HOME', home / '.cargo')}",
                f"--env=RUSTUP_HOME={os.environ.get('RUSTUP_HOME', home / '.rustup')}", f"--command={cargo}", "org.gnome.Sdk//50"] + command[1:]
            subprocess.run(command, env=environment, check=True)
        else:
            subprocess.run(command, check=True)
    else:
        subprocess.run(command, check=True)
    print(f"Installed diagrams.net companion in {destination / 'bin'}")
    print("Rebuild ZedStorm, then open a .drawio file to edit it in an embedded workspace tab.")


if __name__ == "__main__":
    try:
        install()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Installation failed: {error}", file=sys.stderr)
        sys.exit(1)
