#!/usr/bin/env bash

# Build and install Cagent from this source checkout for the current user.
set -euo pipefail

if [[ -z "${HOME:-}" ]]; then
    printf 'Cagent installer: $HOME must be set.\n' >&2
    exit 1
fi

readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SOURCE_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly BIN_DIR="${CAGENT_BIN_DIR:-$HOME/.local/bin}"

print_rustup_instructions() {
    printf '\nRust is required.\n' >&2
    case "$(uname -s)" in
        Linux|Darwin)
            printf '%s\n' \
                'Install Rust with rustup, then rerun this installer:' \
                '  curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh' \
                '' \
                'After installation, open a new terminal or run:' \
                '  . "$HOME/.cargo/env"' \
                '' \
                'More installation options: https://rustup.rs/' >&2
            ;;
        MINGW*|MSYS*|CYGWIN*)
            printf '%s\n' \
                'Install rustup for Windows from https://rustup.rs/, then rerun this installer from Git Bash or WSL.' >&2
            ;;
        *)
            printf '%s\n' 'Install Rust with rustup: https://rustup.rs/' >&2
            ;;
    esac
}

if ! command -v cargo >/dev/null 2>&1; then
    print_rustup_instructions
    exit 1
fi

printf 'Building Cagent from %s...\n' "$SOURCE_DIR"
(
    cd -- "$SOURCE_DIR"
    cargo build --release --package cagent-cli --manifest-path "$SOURCE_DIR/Cargo.toml"
)

mkdir -p "$BIN_DIR"
install -m 755 "$SOURCE_DIR/target/release/cagent" "$BIN_DIR/cagent"
printf 'Installed cagent to %s/cagent\n' "$BIN_DIR"

path_contains_bin_dir() {
    local path_entry
    local -a path_entries

    IFS=: read -r -a path_entries <<< "${PATH:-}"
    for path_entry in "${path_entries[@]}"; do
        [[ "$path_entry" == "$BIN_DIR" ]] && return 0
    done
    return 1
}

if ! path_contains_bin_dir; then
    printf '\n%s is not on your PATH. Add it before running cagent:\n' "$BIN_DIR"
    printf '  bash: add  export PATH="%s:$PATH"  to ~/.bashrc, then run  source ~/.bashrc\n' "$BIN_DIR"
    printf '  zsh:  add  export PATH="%s:$PATH"  to ~/.zshrc, then run  source ~/.zshrc\n' "$BIN_DIR"
    printf '  fish: run  fish_add_path %s\n' "$BIN_DIR"
fi
