---
name: skill-installer
description: Install Cagent skills into its native skills directory from the OpenAI curated list or a GitHub repository path. Use when a user asks to list installable skills or install a skill, including from private repositories.
---

# Skill Installer

Use the bundled scripts to install skills.

- `scripts/install-skill-from-github.py --repo owner/repo --path path/to/skill [...]`
- `scripts/install-skill-from-github.py --url https://github.com/owner/repo/tree/ref/path`

The scripts use the network and normal Cagent permissions apply. Public repositories download
directly; authentication failures fall back to a sparse Git checkout. `GITHUB_TOKEN` or `GH_TOKEN`
supports private downloads. Installation aborts rather than overwriting an existing directory.

The default destination follows Cagent's platform config directory and is normally
`${XDG_CONFIG_HOME:-~/.config}/cagent/skills` on Linux, `~/.config/cagent/skills` on macOS, and
`%APPDATA%\cagent\skills` on Windows. If Cagent was launched with `--config`, pass the effective
config file's parent plus `/skills` explicitly using `--dest`.

After installation, tell the user the skill was automatically loaded.
Bundled skills in `.system` are already installed and should not be overwritten.
