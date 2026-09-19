#!/usr/bin/env bash

# Install the latest Cagent source build for the current user.
set -euo pipefail

readonly REPOSITORY_URL="https://github.com/Cretezy/cagent.git"
readonly BRANCH="main"

if [[ -z "${HOME:-}" ]]; then
    printf 'Cagent installer: $HOME must be set.\n' >&2
    exit 1
fi

case "$(uname -s)" in
    Darwin)
        data_dir="${XDG_DATA_HOME:-$HOME/Library/Application Support}/cagent"
        ;;
    *)
        data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/cagent"
        ;;
esac

readonly SOURCE_DIR="$data_dir/.source"

if ! command -v git >/dev/null 2>&1; then
    printf '%s\n' \
        'Cagent installer: Git is required to download and update the source.' \
        'Install Git for your platform, then rerun this installer: https://git-scm.com/downloads' >&2
    exit 1
fi

if [[ -e "$SOURCE_DIR" || -L "$SOURCE_DIR" ]]; then
    if [[ ! -d "$SOURCE_DIR/.git" ]]; then
        printf 'Cagent installer: %s exists but is not a Cagent source checkout.\n' "$SOURCE_DIR" >&2
        printf 'Move it aside or remove it, then rerun this installer.\n' >&2
        exit 1
    fi

    printf 'Updating Cagent source in %s...\n' "$SOURCE_DIR"
    git -C "$SOURCE_DIR" remote set-url origin "$REPOSITORY_URL"
    git -C "$SOURCE_DIR" fetch --depth 1 origin "$BRANCH"
    git -C "$SOURCE_DIR" checkout --force -B "$BRANCH" FETCH_HEAD
    git -C "$SOURCE_DIR" clean -ffd
else
    printf 'Cloning Cagent source into %s...\n' "$SOURCE_DIR"
    mkdir -p "$data_dir"
    git clone --depth 1 --branch "$BRANCH" "$REPOSITORY_URL" "$SOURCE_DIR"
fi

exec "$SOURCE_DIR/scripts/install-from-local.sh"
