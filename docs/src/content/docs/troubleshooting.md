---
title: Troubleshooting
description: Fix common provider, configuration, editor, MCP, and history problems.
---

## A provider is disabled or not set up

Run `/providers`, select it, and follow the setup instructions. For an API-key provider, enter a
managed key or set its configured environment variable. Restart Cagent if the parent shell did not
have the variable when Cagent started.

## No model appears

Confirm the provider is enabled and authenticated, then run `/reload`. Custom providers may need an
explicit `models` list or `models_endpoint`. For provider-discovered catalogs, verify network access
and credentials; for Models.dev metadata, a cached catalog may remain available when refresh fails.
Unknown models can be listed explicitly, but unknown capabilities stay disabled until discovery or
an override establishes them.

## A configuration edit was rejected

The last valid configuration remains active. Run `cagent config edit` to see validation errors, or
inspect the notice shown after `/reload`.

## `$EDITOR` does not open

Set `$VISUAL` or `$EDITOR` to a valid executable. Cagent launches it directly rather than through a
shell. A failed launch leaves your composer draft unchanged.

## A project MCP server is missing

Project MCP requires a trusted workspace. Check `/mcp` for its scope, assignment, and startup error.
Verify that referenced commands and environment variables are available to Cagent.

For an HTTP server, verify HTTPS, authentication headers, and request timeout. Plain remote HTTP is
rejected unless `allow_insecure = true`; redirects are not followed. For stdio, set
`inherit_env = false` only if every required variable is listed explicitly.

For a package-backed MCP, inspect its source, package ID, parameters, and SHA-256 lock. URL packages
continue to use matching cached manifest bytes; a changed remote manifest is only an available
update. Use **Update package** to review and accept it. If the locked bytes are unavailable, restore
network access or the cache—Cagent will not silently run different manifest content. Missing
`${secret:mcp/...}` values can be replaced in `/mcp` without putting plaintext in configuration.

For `runner = "oci"`, verify Docker or Podman is installed and can pull the digest-pinned image.
Then inspect `/mcp` for denied network, workspace mount, writable path, or resource requirements;
these are secure-by-default exceptions and must be explicitly confirmed.

## A headless tool was denied

Headless execution cannot open permission prompts. Trust only loads project configuration; it does
not approve writes, commands, web access, or MCP calls. Add a narrow saved permission rule, choose a
mode that already allows the operation, or adjust `--allow-tool` and `--deny-tool`. Remember that
tool filters only remove tools—they never grant permission.

## Context is still too large after compaction

One message, attachment, or protocol-safe tool group may exceed the selected model's usable context
window by itself. Attach a narrower line range, fork before a large result, shorten the request, or
choose a model with a larger window. See [Context and compaction](/context-and-compaction/).

## Terminal output is truncated

Open `/background` to inspect the retained terminal buffer. Agent tools should read incrementally
with `terminal_output` cursors. Increase `shell.output_bytes` or `shell.buffer_bytes` only when the
extra model context and local memory use are acceptable; `output_bytes` cannot exceed the buffer.

## A conversation is read-only

Another Cagent process owns it. You can inspect the live conversation but cannot send or change
messages, change branch state, or answer its pending interactions. Press `/` to access the reduced
observer command set. Close the owner, then use the takeover action in the observer session. See
[Read-only observers](/conversations/#read-only-observers) for the available commands.

## View diagnostics

Logs are stored as `diagnostic.log` in the data directory. Reproduce the issue with a known path:

```sh
CAGENT_LOG=cagent_agent=debug cagent --data-dir ./cagent-data
tail -n 100 ./cagent-data/diagnostic.log
```

`CAGENT_LOG` accepts standard Rust tracing filters. Avoid sharing logs before checking them for
sensitive project content.

For an HTTP MCP that reports authentication is required, run
`cagent mcp oauth status NAME`, then `cagent mcp oauth connect NAME`. OAuth needs a browser-accessible
loopback callback and either a pre-registered client, client metadata, or dynamic registration from
the authorization server. If the server does not publish RFC 8414 metadata, configure its explicit
issuer, authorization endpoint, token endpoint, and public client ID. PAT-based packages can instead
use `cagent mcp secret set mcp/<server>/pat`; the value is read from stdin and is not written to TOML.

## Related

- [Providers and models](/providers-and-models/) covers authentication and catalog sources.
- [Configuration](/configuration/) lists defaults, validation, and reload behavior.
- [Permissions](/permissions/) explains trust, explicit rules, and auto review.
- [Tools](/tools/) links each tool to its focused workflow guide.
