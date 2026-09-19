---
title: MCP
description: Connect Cagent to external Model Context Protocol tools.
---

MCP servers add external tools to Cagent. Cagent supports local stdio servers and Streamable HTTP
servers. Tool descriptions and results are external input, not trusted instructions. Availability
does not imply permission: agent policy, server assignment, explicit rules, mode behavior, and
workspace trust are evaluated separately.

## Manage servers

Run `/mcp` to add, edit, enable, disable, inspect, or remove servers. The screen shows each server's
scope, assigned agents, status, package/custom origin, and discovered tools.

- **Add** creates a custom stdio or Streamable HTTP MCP, or imports a JSON definition.
- **Add from catalog** installs a bundled package or one from a catalog manifest URL. It asks for
  the package's declared parameters and keeps a package reference instead of copying its fields.

You can also use the CLI:

```sh
cagent mcp add docs -- docs-mcp
cagent mcp add internal --url https://example.test/mcp
cagent mcp catalog
cagent mcp install builtin:github
cagent mcp install https://example.test/cagent-mcp.toml --name docs
cagent mcp list
cagent mcp get docs --json
cagent mcp update docs
cagent mcp detach docs
cagent mcp oauth connect github
cagent mcp oauth status github
cagent mcp oauth disconnect github
cagent mcp disable docs
cagent mcp remove docs --yes
```

Global servers are the default. Add `--scope project` to store a server in the trusted workspace's
`.cagent/config.toml`. A project definition overrides a global definition with the same name.

## Configure a local server

```toml
[mcp.servers.docs]
transport = "stdio"
command = "docs-mcp-server"
cwd = "${workspace}"
enabled = true
startup_timeout_seconds = 10
request_timeout_seconds = 60
read_only_tools = ["issues.list", "pull_requests.get"]

[mcp.servers.docs.env]
DOCS_TOKEN = { value = "${secret:mcp/docs/token}", description = "Documentation service token" }
```

Use `${secret:mcp/<server>/<key>}` for credentials managed through `/mcp`; this works for every
custom and packaged MCP. `${env:NAME}` remains available when you intentionally manage a value in
the parent environment. Environment and header values may be strings or
`{ value = "...", description = "..." }`; descriptions are shown when Cagent asks for setup values.

Use `inherit_env = false` for a clean child environment. `read_only_tools` is your explicit
attestation that those operations are safe for read-only or delegated work; Cagent cannot infer that
from an arbitrary server description. Failed local servers expose their startup error in `/mcp` and
can be restarted after configuration reload. See the
[MCP server configuration reference](/configuration/#mcp-servers) for every field.

## Configure an HTTP server

```toml
[mcp.servers.internal]
transport = "http"
url = "https://example.test/mcp"
enabled = true

[mcp.servers.internal.headers]
Authorization = { value = "Bearer ${secret:mcp/internal/token}", description = "Internal MCP bearer token" }
```

Remote servers require HTTPS unless `allow_insecure = true`. Plain HTTP is allowed by default only
for localhost and loopback addresses. Cagent does not follow redirects. Environment and header
values are redacted from logs and previews. HTTP fields are also listed in the
[MCP server configuration reference](/configuration/#mcp-servers).

`${env:NAME}` reads an externally managed value at runtime and `${workspace}` resolves to the active
working directory. Startup and request timeouts are configured per server.

HTTP MCPs can also use host-managed OAuth. Cagent performs protected-resource and authorization
server discovery, S256 PKCE, dynamic or pre-registered client selection, browser authorization with
a one-time loopback callback, token refresh, and managed credential storage. Configure static
issuer/authorization/token endpoints only when the provider does not publish RFC 8414 metadata.
Use `cagent mcp oauth connect NAME`, `status`, and `disconnect` to manage the connection. A described
`oauth.bearer_token` can provide a managed PAT/bearer fallback without persisting it in TOML.

## Install packages from the catalog

Packages are declarative templates. They describe the transport, setup parameters, and the exact
environment variables or HTTP headers populated by those values; they do not run installer scripts.
Parameters use keyed tables such as `[parameters.api_url]` and support string, string-list, boolean,
and integer values.

Secrets are separate keyed declarations such as `[secrets.pat]`. Package templates refer to them by
local ID (`${secret:pat}`); when installed as `github`, Cagent resolves that to the private managed
namespace `${secret:mcp/github/pat}`. A package or custom MCP cannot reference another server's
secret namespace. Labels and descriptions are setup metadata only, while values remain in Cagent's
credential store.

Package references use **Option B**: configuration retains the source, package ID, digest lock, and
your parameter values. Cagent resolves the separate package definition when loading the server.

```toml
[mcp.servers.github]
package = "builtin:github"

[mcp.servers.docs]
package = "https://example.test/cagent-mcp.toml"
package_digest = "sha256:4d967…"

[mcp.servers.docs.parameters]
base_url = "https://docs.example.test"
token = "${secret:mcp/docs/token}"
```

Bundled definitions follow your installed Cagent release. Their OCI images are pinned by immutable
image digest. URL manifests are downloaded as data, cached, and SHA-256 locked. Cagent continues to
use the locked cached bytes and only reports remote changes. Choose **Update package** (or run
`cagent mcp update NAME`) to preview and explicitly accept a new manifest digest. There are no
silent package updates.

Choose **Convert to custom MCP** (or `cagent mcp detach NAME`) to resolve the current package into a
normal server definition and detach it. The custom MCP keeps its current behavior and parameters,
but no longer follows package updates.

The bundled **GitHub** package connects to GitHub's hosted Streamable HTTP endpoint at
`https://api.githubcopilot.com/mcp/`; it has no Docker image. It supports host-managed OAuth
using Cagent's registered public client ID and a managed PAT at `mcp/github/pat` as an alternative.
Its `toolsets` string-list parameter controls GitHub's `X-MCP-Toolsets` header and defaults to
`repos`, `issues`, and `pull_requests`.

GitHub login uses Device Flow, like GitHub Copilot login. Cagent ships the registered public client
ID, opens `https://github.com/login/device`, copies the short code, polls in the background, and
stores only the resulting token. End users configure no OAuth client ID, client secret, or callback
URL. `/mcp` keeps both Device Flow and browser OAuth in a dedicated login screen while polling, so
the verification URL, short code, completion, and sanitized failures remain visible. The OAuth App
used by Cagent must have **Enable Device Flow** selected. Device Flow copies the short code
automatically. After credentials are saved and the MCP has successfully restarted, the screen
changes to a completion view where Enter or Esc continues.

Enabled MCP servers start when a session starts. Installing or enabling one from `/mcp` starts it
immediately. For an HTTP MCP, open its server menu and select **Login with OAuth**; Cagent discovers
standard OAuth metadata, opens the browser, handles the loopback callback, stores the credential in
managed secret storage, and restarts the server. Optional `[mcp.oauth]` fields provide client
registration or interoperability hints when server discovery is incomplete. **Log out OAuth**
removes the managed credential. The menu shows **Login with OAuth** only while disconnected and
**Log out OAuth** only while connected.

Catalog installation opens a package setup screen before writing configuration. It lists secrets
first and supports typed string, string-list, integer, and boolean parameters plus masked input and
configured/not-configured status for every declared secret. Parameter and secret descriptions appear
below their input, and HTTP(S) URLs in descriptions are clickable terminal links. Open
**Configure package** from an installed package-backed
server to change the same values later. Secret values are written directly to that server's managed
namespace and are never included in TOML or the transcript; submitting an empty secret removes it.
If an unset package secret is the complete value of an environment variable or HTTP header, Cagent
omits that field. References embedded inside a larger value still require the secret to be set.

For other MCP authorization servers, Cagent's generic browser flow uses
`http://127.0.0.1:<random-port>/auth/callback`.

The bundled **GitLab** package runs `zereight/gitlab-mcp` in Cagent's hardened OCI runner. Its image
source and publishing workflow live in the Cagent repository. Store the GitLab PAT as
`mcp/gitlab/pat`.

## Run stdio in Docker or Podman

Any custom stdio MCP—not only a catalog package—can use `runner = "oci"` with Docker or Podman and
an image pinned as `repository@sha256:<digest>`. Cagent connects over container stdio.

OCI servers default to a read-only root filesystem, dropped capabilities, no-new-privileges, no
host namespaces or engine socket, bounded resources, explicit environment only, and no network or
workspace mount. `/mcp` displays and confirms each requested network mode, writable path, or
workspace mount. Parameters and secrets are passed directly, never through a shell.

## Assign servers to agents

By default a server is available to every agent. Restrict it with:

```toml
[mcp.servers.github]
# ...
agents = ["general", "review"]
```

Agent `[mcp].allow` and `[mcp].deny` policies can narrow this set further; deny wins. Provider-safe
dynamic tool IDs begin with `mcp__<server>__<tool>` after sanitization and may be shortened with a
hash. Inspect the server's tools in `/mcp` for exact names.

## Inspecting calls

MCP calls appear as one truncated transcript line with the server, tool, duration, and dimmed JSON
parameters. The dot pulses while running, then turns green on success or red on failure. Click the
line to open the full-height detail view with formatted, syntax-highlighted **Parameters** and
**Output** sections. Use the usual expanded-view scrolling controls and `Esc` to close it.
The duration measures execution, not time spent waiting for permission approval. This applies to
both the transcript row and the expanded detail header.
Responding to an approval also restarts the working line's inactivity clock, so time spent waiting
for you does not produce a stale “No activity” warning when execution resumes.

## Permissions

MCP calls ask for permission by default. Save an allow rule when a tool should run without asking:

```toml
[[global.rule]]
id = "allow-doc-search"
effect = "allow"
tool = "mcp"
server = "docs"
operation = "search"
access = "execute"
```

In auto mode, unresolved calls are reviewed automatically. Explicit allow and deny rules still win.

Project MCP servers load only after you trust the workspace. If a server is missing, check trust,
its assignment, and `/mcp` for startup errors.

## Related

- [Tools](/tools/) explains native tools and dynamic MCP tool IDs.
- [Agents](/agents/) explains agent-level MCP allow and deny policies.
- [Permissions](/permissions/) explains saved rules and auto review.
- [Troubleshooting](/troubleshooting/#a-project-mcp-server-is-missing) covers startup failures.
