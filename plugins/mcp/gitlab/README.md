# GitLab MCP package

This directory contains Cagent's bundled GitLab MCP package and its lightweight, non-root OCI
image. The image builds [`zereight/gitlab-mcp`](https://github.com/zereight/gitlab-mcp) at the
stable `v2.1.53` release and copies only the built server and production dependencies into the
runtime stage. Update `GITLAB_MCP_VERSION` in the `Containerfile` deliberately when upgrading.

Build and protocol-smoke-test the current platform locally:

```sh
bash ./scripts/publish-mcp-image gitlab
```

The script auto-detects Docker or Podman (or uses `OCI_RUNTIME`) and sends an MCP `initialize`
request to the locally built image. The smoke test uses a placeholder token and disables container
networking, so it does not access GitLab.

Publish the multi-platform image after authenticating to GHCR:

```sh
bash ./scripts/publish-mcp-image gitlab --push --version 0.1.0
```

Publishing requires Docker Buildx and pushes `linux/amd64` and `linux/arm64` by default. The command
prints the pushed manifest digest but never edits `package.toml`. Update `package.toml` separately to
an `@sha256:...` image reference before releasing Cagent; built-in packages should not execute
mutable registry tags.
