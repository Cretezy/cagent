---
title: Search and fetch
description: Search the web and fetch pages from Cagent.
---

Cagent has two web tools: `web_search` finds relevant pages, and `web_fetch` reads a URL. Results
are treated as untrusted reference material, not as instructions.

## Search the web

Run `/search` to choose a provider. Once enabled, search is made available to the agent.

You can also use the command with a query to manually execute a web search:
```text
/search What changed in Rust 1.97?
```

Searches ask for permission unless a rule or auto mode allows them.

Search queries are limited to 1k characters and returns at most 10 normalized results.

### SearXNG

Choose an instance from [searx.space](https://searx.space) or use your own:

```toml
[web_search]
provider = "searxng"

[web_search.searxng]
url = "https://search.example.com"
```

Use `url_env_var = "SEARXNG_URL"` instead if you do not want the URL in TOML.

### Exa

```toml
[web_search]
provider = "exa"

[web_search.exa]
api_key_env_var = "EXA_API_KEY"
```

You can also enter an Exa key from `/search`; it is stored in Cagent's credential backend.

See the [search configuration reference](/configuration/#search-and-fetch) for provider, endpoint,
credential-variable, and timeout fields.

### ChatGPT

ChatGPT search needs a connected ChatGPT account with an active Codex entitlement. Connect it from
`/providers`, then select ChatGPT in `/search`. No separate key or URL is needed.

## Fetch a page

`web_fetch` works without a search provider. Give Cagent a URL and ask it to read or summarize the
page. It supports Markdown, text, and raw HTML output. Timeout values range from 1 to 120 seconds and
default to 30. A fetch follows at most 10 redirects, accepts textual MIME types only, and rejects a
response above 5 MiB. Normalized results remain bounded and may not preserve page layout or scripts.

Every redirect destination is authorized before Cagent requests it. Common HTTP-to-HTTPS, `www`, and
same-site redirects can match the built-in safe redirect policy and reuse the initial approval. To
require separate review, configure:

```toml
[web_fetch.redirects]
generally_safe = false
same_site = false
```

See the [fetch redirect settings](/configuration/#search-and-fetch) for their defaults.

## Permission rules

Search rules match the provider; fetch rules can match exact URLs or patterns:

```toml
[[global.rule]]
id = "allow-docs-search"
effect = "allow"
tool = "web_search"
server = "searxng"

[[global.rule]]
id = "allow-rust-docs"
effect = "allow"
tool = "web_fetch"
operation = "fetch"
command = ["https://doc.rust-lang.org/**"]
```

In auto mode, unresolved searches, fetches, and redirect destinations are reviewed automatically.
This is a policy decision, not a trust signal. Search snippets and fetched pages can contain prompt
injection or incorrect code; ask the agent to compare primary sources before changing your project.

## Related

- [Tools](/tools/) lists availability and input schemas for both web tools.
- [Permissions](/permissions/) explains saved rules, auto review, and deny precedence.
- [Providers and models](/providers-and-models/#supported-providers) explains ChatGPT/Codex search.
- [Configuration](/configuration/#search-and-fetch) lists endpoints, credentials, and redirect rules.
