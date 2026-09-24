#!/usr/bin/env python3
"""Check that release-please can bump every Cagent crate together."""

import pathlib
import sys
import tomllib


root = pathlib.Path(__file__).resolve().parent.parent
version = (root / "version.txt").read_text().strip()
manifest = tomllib.loads((root / "Cargo.toml").read_text())
lock = tomllib.loads((root / "Cargo.lock").read_text())
packages = ("cagent-acp", "cagent-agent", "cagent-cli")
locked = {package["name"]: package["version"] for package in lock["package"]}

if manifest["workspace"]["package"]["version"] != version or any(
    locked.get(package) != version for package in packages
):
    sys.exit("Cagent's workspace and lockfile versions must match version.txt")

# Cargo rewrites Cargo.lock without preserving comments, so its versions are
# checked above instead of relying on release-please annotations in the lockfile.
if (root / "Cargo.toml").read_text().count("x-release-please-version") != 1:
    sys.exit("Restore the release-please annotation in Cargo.toml")
