#!/usr/bin/env python3
import argparse
import json
import os
import pathlib
import sys
import urllib.error
from github_utils import github_api_contents_url, github_request

def skills_root() -> pathlib.Path:
    if sys.platform == "darwin":
        return pathlib.Path.home() / ".config/cagent/skills"
    if os.name == "nt":
        return pathlib.Path(os.environ.get("APPDATA", pathlib.Path.home())) / "cagent/skills"
    return pathlib.Path(os.environ.get("XDG_CONFIG_HOME", pathlib.Path.home() / ".config")) / "cagent/skills"

parser = argparse.ArgumentParser(description="List installable Cagent skills")
parser.add_argument("--repo", default="openai/skills")
parser.add_argument("--path", default="skills/.curated")
parser.add_argument("--ref", default="main")
parser.add_argument("--format", choices=("text", "json"), default="text")
args = parser.parse_args()
try:
    data = json.loads(github_request(github_api_contents_url(args.repo, args.path, args.ref), "cagent-skill-list"))
    names = sorted(item["name"] for item in data if item.get("type") == "dir")
except (urllib.error.URLError, ValueError, TypeError) as error:
    raise SystemExit(f"Failed to list skills: {error}")
installed = {path.name for path in skills_root().iterdir() if path.is_dir()} if skills_root().is_dir() else set()
if args.format == "json":
    print(json.dumps([{"name": name, "installed": name in installed} for name in names]))
else:
    for index, name in enumerate(names, 1):
        print(f"{index}. {name}{' (already installed)' if name in installed else ''}")
