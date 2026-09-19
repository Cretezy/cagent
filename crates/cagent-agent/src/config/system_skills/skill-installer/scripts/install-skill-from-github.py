#!/usr/bin/env python3
"""Install one or more Cagent skills from a GitHub repository."""
import argparse
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import zipfile
from github_utils import github_request

class InstallError(Exception):
    pass

def default_destination() -> pathlib.Path:
    if sys.platform == "darwin":
        return pathlib.Path.home() / ".config/cagent/skills"
    if os.name == "nt":
        return pathlib.Path(os.environ.get("APPDATA", pathlib.Path.home())) / "cagent/skills"
    return pathlib.Path(os.environ.get("XDG_CONFIG_HOME", pathlib.Path.home() / ".config")) / "cagent/skills"

def parse_url(url, default_ref):
    parsed = urllib.parse.urlparse(url)
    if parsed.netloc != "github.com":
        raise InstallError("only github.com URLs are supported")
    parts = [part for part in parsed.path.split("/") if part]
    if len(parts) < 2:
        raise InstallError("invalid GitHub URL")
    owner, repo = parts[:2]
    if len(parts) >= 4 and parts[2] in ("tree", "blob"):
        return owner, repo, parts[3], "/".join(parts[4:])
    return owner, repo, default_ref, "/".join(parts[2:])

def safe_extract(archive, destination):
    root = destination.resolve()
    for item in archive.infolist():
        target = (destination / item.filename).resolve()
        if target != root and root not in target.parents:
            raise InstallError("archive contains a path outside its destination")
    archive.extractall(destination)

def download(owner, repo, ref, temporary):
    payload = github_request(f"https://codeload.github.com/{owner}/{repo}/zip/{ref}", "cagent-skill-install")
    archive_path = temporary / "repo.zip"
    archive_path.write_bytes(payload)
    with zipfile.ZipFile(archive_path) as archive:
        roots = {name.split("/")[0] for name in archive.namelist() if name}
        if len(roots) != 1:
            raise InstallError("unexpected archive layout")
        safe_extract(archive, temporary)
    return temporary / roots.pop()

def run(command):
    result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise InstallError(result.stderr.strip() or "git command failed")

def sparse_checkout(owner, repo, ref, paths, temporary):
    target = temporary / "repo"
    urls = (f"https://github.com/{owner}/{repo}.git", f"git@github.com:{owner}/{repo}.git")
    last_error = None
    for url in urls:
        try:
            run(["git", "clone", "--filter=blob:none", "--depth", "1", "--sparse", url, str(target)])
            run(["git", "-C", str(target), "sparse-checkout", "set", *paths])
            run(["git", "-C", str(target), "checkout", ref])
            return target
        except InstallError as error:
            last_error = error
            shutil.rmtree(target, ignore_errors=True)
    raise last_error

def main():
    parser = argparse.ArgumentParser(description="Install Cagent skills from GitHub")
    parser.add_argument("--repo", help="owner/repo")
    parser.add_argument("--url")
    parser.add_argument("--path", nargs="+")
    parser.add_argument("--ref", default="main")
    parser.add_argument("--dest", type=pathlib.Path)
    parser.add_argument("--name")
    parser.add_argument("--method", choices=("auto", "download", "git"), default="auto")
    args = parser.parse_args()
    if args.url:
        owner, repo, ref, url_path = parse_url(args.url, args.ref)
        paths = args.path or ([url_path] if url_path else [])
    elif args.repo and len(args.repo.split("/")) == 2:
        owner, repo = args.repo.split("/")
        ref, paths = args.ref, args.path or []
    else:
        raise InstallError("provide --repo owner/repo or --url")
    if not paths:
        raise InstallError("provide at least one skill path")
    for path in paths:
        if pathlib.PurePosixPath(path).is_absolute() or ".." in pathlib.PurePosixPath(path).parts:
            raise InstallError("skill paths must stay inside the repository")
    if args.name and len(paths) != 1:
        raise InstallError("--name requires exactly one --path")
    destination = args.dest or default_destination()
    with tempfile.TemporaryDirectory(prefix="cagent-skill-install-") as temporary_name:
        temporary = pathlib.Path(temporary_name)
        root = None
        if args.method in ("auto", "download"):
            try:
                root = download(owner, repo, ref, temporary)
            except (InstallError, urllib.error.HTTPError):
                if args.method == "download":
                    raise
        if root is None:
            root = sparse_checkout(owner, repo, ref, paths, temporary)
        installed = []
        for relative in paths:
            source = root / relative
            if not source.is_dir() or not (source / "SKILL.md").is_file():
                raise InstallError(f"SKILL.md not found in {relative}")
            name = args.name or pathlib.PurePosixPath(relative.rstrip("/")).name
            if not name or name in (".", "..") or "/" in name or "\\" in name:
                raise InstallError("invalid destination skill name")
            target = destination / name
            if target.exists():
                raise InstallError(f"destination already exists: {target}")
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(source, target)
            installed.append(str(target))
        print("Installed " + ", ".join(installed))

if __name__ == "__main__":
    try:
        main()
    except (InstallError, OSError, urllib.error.URLError, zipfile.BadZipFile) as error:
        raise SystemExit(f"Error: {error}")
