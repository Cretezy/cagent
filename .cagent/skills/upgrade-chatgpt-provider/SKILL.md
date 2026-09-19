---
name: upgrade-chatgpt-provider
description: Upgrade or diagnose Cagent's compatibility-sensitive ChatGPT/Codex subscription provider, especially when its models endpoint returns no models, requires a newer client_version, or new ChatGPT models are missing.
---

# Upgrade ChatGPT Provider

Use this workflow when ChatGPT model discovery is empty or the Codex backend rejects Cagent's
client version.

## Source of truth

The ChatGPT provider is compatibility-sensitive. Use the latest stable OpenAI Codex CLI release as
the source for the protocol-compatible version; do not use Cagent's package version.

1. Read the latest stable release from `openai/codex` and record its numeric version, stripping the
   `rust-v` tag prefix (for example, `rust-v0.153.4` becomes `0.153.4`). Ignore prereleases unless
   the user explicitly requests one.
2. Inspect `references/codex/codex-rs/codex-api/src/endpoint/models.rs` and the current upstream
   equivalent to confirm that model discovery still sends `client_version` as a query parameter.
3. Update `CODEX_CLIENT_VERSION` in
   `crates/cagent-agent/src/provider/adapters/chatgpt.rs`. Keep it a pinned Codex compatibility
   version; never derive it from `CARGO_PKG_VERSION`.

## Verification

Run:

```sh
cargo fmt --all
cargo test -p cagent-agent provider::adapters::chatgpt::tests
```

The request-contract test must continue to verify all of these without exposing real credentials:

- `GET /codex/models?client_version=<version>`;
- bearer authentication;
- `ChatGPT-Account-Id`;
- dynamic parsing of a newly advertised model without a Cagent allowlist.

If local diagnostics show a missing `client_version`, an HTTP error, or an empty catalog, inspect
`~/.local/share/cagent/diagnostic.log` without printing tokens. A successful response with zero
models usually means the pinned Codex version is below the backend's minimum supported version.

When credentials and network access are available, verify the live endpoint through the adapter
and report only response status, model count, and model IDs. Never print access tokens, refresh
tokens, ID tokens, account IDs, or full response payloads.

## Keep behavior provider-owned

Do not add a hard-coded ChatGPT model list to fix discovery. The authenticated Codex models API is
the availability authority; Models.dev may enrich matching IDs but must not synthesize ChatGPT
availability. Update `SPEC.md` and provider docs if this contract changes.
