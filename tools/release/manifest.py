#!/usr/bin/env python3
"""Build numbering and the public updater manifest share one path filter."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import tomllib

# Git pathspecs, deliberately shared by counting and the commit subjects.
APP_PATHS = ["apps", "crates", "Cargo.toml", "Cargo.lock", ".cargo",
             "rust-toolchain*", "tools/cross", "tools/release", ".github/workflows/release.yml"]


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def metadata():
    version = json.loads(Path("apps/desktop/src-tauri/tauri.conf.json").read_text())["version"]
    npm = json.loads(Path("apps/desktop/package.json").read_text())["version"]
    cargo = tomllib.loads(Path("apps/desktop/src-tauri/Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", version) or version != npm or version != cargo:
        raise ValueError("Set the same X.Y.Z version in tauri.conf.json, package.json and Cargo.toml")
    # First-parent history counts app-changing main commits, including merges,
    # once each. Documentation-only commits don't change the build number.
    commits = git("log", "--first-parent", "--format=%H%x09%s", "--", *APP_PATHS).splitlines()
    number = len(commits)
    if not 1 <= number <= 65535:
        raise ValueError("The Windows build number must be between 1 and 65535")
    commit = git("rev-parse", "HEAD")
    return {"version": f"{version}+{number}", "releaseVersion": version,
            "build": {"number": number, "commit": commit,
                      "commits": [dict(zip(("sha", "subject"), line.split("\t", 1)))
                                  for line in commits[:100]]}}


def manifest(info, assets, repository, tag):
    platforms = {}
    for arch, target in (("arm64", "aarch64"), ("x64", "x86_64")):
        name = f"spark-rs-{info['version']}-windows-{arch}-setup.exe"
        installer = assets / name
        signature = assets / f"{name}.sig"
        if not installer.is_file() or installer.stat().st_size == 0:
            raise ValueError(f"Missing installer: {installer}")
        sig = signature.read_text().strip()
        if not sig:
            raise ValueError(f"Empty signature: {signature}")
        platforms[f"windows-{target}-nsis"] = {
            "url": f"https://github.com/{repository}/releases/download/{tag}/{name}",
            "signature": sig,
        }
    return {"version": info["version"], "notes": "Updates from main",
            "pub_date": datetime.now(timezone.utc).isoformat(),
            "build": info["build"], "platforms": platforms}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    meta = sub.add_parser("metadata")
    meta.add_argument("--output", type=Path, required=True)
    release = sub.add_parser("manifest")
    release.add_argument("--info", type=Path, required=True)
    release.add_argument("--assets", type=Path, required=True)
    release.add_argument("--repository", required=True)
    release.add_argument("--tag", required=True)
    release.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "metadata":
        result = metadata()
    else:
        result = manifest(json.loads(args.info.read_text()), args.assets, args.repository, args.tag)
    args.output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
