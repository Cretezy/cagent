#!/usr/bin/env sh

# Install the latest Cagent release for the current user.
set -eu

readonly REPOSITORY="Cretezy/cagent"

if [ -z "${HOME:-}" ]; then
    printf 'Cagent installer: $HOME must be set.\n' >&2
    exit 1
fi

os=$(uname -s)
arch=$(uname -m)

case "$os-$arch" in
    Linux-x86_64|Linux-amd64)
        target="x86_64-unknown-linux-gnu"
        ;;
    Linux-aarch64|Linux-arm64)
        target="aarch64-unknown-linux-gnu"
        ;;
    Darwin-x86_64|Darwin-amd64)
        target="x86_64-apple-darwin"
        ;;
    Darwin-arm64|Darwin-aarch64)
        target="aarch64-apple-darwin"
        ;;
    *)
        printf 'Cagent installer: unsupported platform %s (%s).\n' "$os" "$arch" >&2
        printf 'Build from source instead: https://github.com/%s#run-from-source\n' "$REPOSITORY" >&2
        exit 1
        ;;
esac

if ! command -v curl >/dev/null 2>&1; then
    printf 'Cagent installer: curl is required to download Cagent.\n' >&2
    exit 1
fi

archive="cagent-$target.tar.gz"
bin_dir="${CAGENT_BIN_DIR:-$HOME/.local/bin}"
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/cagent-install.XXXXXX")
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

# Resolve the latest published release once so the archive and checksum cannot
# come from different releases if a new version appears mid-install.
latest_url=$(curl --fail --location --head --silent --show-error --retry 3 \
    --proto '=https' --proto-redir '=https' --tlsv1.2 \
    --output /dev/null --write-out '%{url_effective}' \
    "https://github.com/$REPOSITORY/releases/latest")
case "$latest_url" in
    "https://github.com/$REPOSITORY/releases/tag/"v*)
        version=${latest_url##*/}
        ;;
    *)
        printf 'Cagent installer: could not determine the latest published release.\n' >&2
        exit 1
        ;;
esac
case "$version" in
    *[!a-zA-Z0-9._+-]*|'')
        printf 'Cagent installer: invalid release version: %s\n' "$version" >&2
        exit 1
        ;;
esac
release_url="https://github.com/$REPOSITORY/releases/download/$version"

printf 'Downloading Cagent %s for %s...\n' "$version" "$target"
curl --fail --location --silent --show-error --retry 3 \
    --proto '=https' --proto-redir '=https' --tlsv1.2 \
    "$release_url/$archive" -o "$temp_dir/$archive"
curl --fail --location --silent --show-error --retry 3 \
    --proto '=https' --proto-redir '=https' --tlsv1.2 \
    "$release_url/SHA256SUMS" -o "$temp_dir/SHA256SUMS"

expected=$(awk -v archive="$archive" '$2 == archive { print $1; exit }' "$temp_dir/SHA256SUMS")
if [ -z "$expected" ]; then
    printf 'Cagent installer: no checksum was published for %s.\n' "$archive" >&2
    exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temp_dir/$archive" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temp_dir/$archive" | awk '{ print $1 }')
else
    printf 'Cagent installer: sha256sum or shasum is required to verify the download.\n' >&2
    exit 1
fi

if [ "$actual" != "$expected" ]; then
    printf 'Cagent installer: checksum verification failed for %s.\n' "$archive" >&2
    exit 1
fi

tar -xzf "$temp_dir/$archive" -C "$temp_dir" cagent
mkdir -p "$bin_dir"
install -m 755 "$temp_dir/cagent" "$bin_dir/cagent"
printf 'Installed cagent to %s/cagent\n' "$bin_dir"

case ":${PATH:-}:" in
    *:"$bin_dir":*)
        ;;
    *)
        printf '\n%s is not on your PATH. Add it before running cagent:\n' "$bin_dir"
        printf '  bash: add  export PATH="%s:$PATH"  to ~/.bashrc, then run  source ~/.bashrc\n' "$bin_dir"
        printf '  zsh:  add  export PATH="%s:$PATH"  to ~/.zshrc, then run  source ~/.zshrc\n' "$bin_dir"
        printf '  fish: run  fish_add_path "%s"\n' "$bin_dir"
        ;;
esac
