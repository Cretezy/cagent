# Cagent

Cagent is a terminal coding agent for understanding a workspace, proposing guarded changes,
running commands, and reviewing results. Conversations are stored locally and can be resumed,
branched, or compacted. Use Cagent interactively for day-to-day development or headlessly in
scripts and CI.

- Review exact file changes and permission requests before they run.
- Choose providers, models, modes, agents, skills, instructions, and MCP servers to suit your
  workflow.
- Control access to files, commands, websites, and external tools with explicit, inspectable rules.

## Install

### Latest release

Release installers are available for x86-64 and Arm64 Linux and macOS. They download the matching
binary from the latest GitHub release, verify its SHA-256 checksum, and install `cagent` to
`~/.local/bin` by default:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/Cretezy/cagent/main/scripts/install.sh | sh
```

Set `CAGENT_BIN_DIR` to choose another installation directory.

### Source builds

To clone, build, and install the latest `main` branch, run:

```sh
curl https://raw.githubusercontent.com/Cretezy/cagent/main/scripts/install-from-source.sh | bash
```

This requires Git and Rust. To build the checkout you already have instead, run:

```sh
./scripts/install-from-local.sh
```

Cagent requires Rust 1.97.1 or newer. Both source-install scripts install the resulting binary to
`~/.local/bin` by default and honor `CAGENT_BIN_DIR`.

## Get started

Open the project you want to work on and start Cagent:

```sh
cd my-project
cagent
```

On first use, Cagent asks whether to trust the workspace, then guides you through connecting a
provider and choosing a model. API-key, subscription, Google Cloud, and custom
OpenAI-compatible providers are supported.

For scripts and CI, use headless execution after trusting the workspace:

```sh
cagent --trust exec --output json "Run the tests and summarize failures"
```

## Documentation

- [Getting started](docs/src/content/docs/getting-started.md)
- [Providers and models](docs/src/content/docs/providers-and-models.md)
- [Permissions](docs/src/content/docs/permissions.md)
- [Headless execution](docs/src/content/docs/headless.md)
- [Full documentation source](docs/src/content/docs/index.mdx)

## Releases

Commits on `main` following [Conventional Commits](https://www.conventionalcommits.org/)
feed Release Please. Merge its release PR to update the changelog and versions and publish a
`vX.Y.Z` release. CI then builds Linux and macOS binaries and uploads them with `SHA256SUMS`.
The installer resolves the latest published version and checks the archive against that release's
checksums. Configure a repository secret named `RELEASE_PLEASE_TOKEN` with a fine-grained GitHub
token for this repository with **Contents** and **Pull requests** read/write permissions. Using a
separate token rather than `GITHUB_TOKEN` ensures the generated tag triggers the binary and MCP
image publishing workflows. When updating dependencies, keep the
`x-release-please-version` annotations on Cagent's entries in `Cargo.lock` so release PRs can
update the workspace lockfile.
