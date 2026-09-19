---
title: Providers and models
description: Connect providers and choose the model Cagent uses.
---

Cagent supports subscription, API-key, and Google Cloud providers behind the same model picker.
Providers are disabled by configuration until you explicitly enable them; enabled, authenticated,
and ready are separate states.

## Supported providers

| Provider | Authentication | Model catalog | Notes |
| --- | --- | --- | --- |
| ChatGPT subscription/Codex (`chatgpt`) | Browser PKCE or device-code login | Authenticated Codex API | Requires a ChatGPT account. Enables ChatGPT's [web search](/search-and-fetch/#chatgpt) provider. |
| GitHub Copilot (`github-copilot`) | GitHub device-code login | Provider API | Uses the models available to the connected Copilot subscription. |
| OpenAI (`openai`) | `OPENAI_API_KEY` or managed key | Provider API | |
| Anthropic (`anthropic`) | `ANTHROPIC_API_KEY` or managed key | Provider API | |
| OpenRouter (`openrouter`) | `OPENROUTER_API_KEY` or managed key | Provider API | |
| OpenCode Zen (`opencode`) | `OPENCODE_API_KEY` or managed key | Provider API | |
| Google Gemini (`google`) | `GOOGLE_API_KEY` or managed key | Provider API | |
| Google Vertex AI (`google-vertex`) | ADC via `GOOGLE_APPLICATION_CREDENTIALS`, or `GOOGLE_API_KEY` | Models.dev metadata | `auth = "adc"` requires a project. `auth = "api-key"` selects Vertex Express. Location defaults to `global`. |
| Custom OpenAI-compatible | Configured environment variable | Explicit list or configured endpoint | Multiple protocols available. |

Models.dev enriches provider API catalogs and supplies catalogs for integrations without a suitable
model endpoint. Cagent caches refreshed catalogs and metadata locally. Unknown capabilities remain
unavailable until discovery or an explicit override establishes them.

If an active model request loses its connection, Cagent keeps the turn active and retries
retryable provider failures automatically. The working-state line shows the reconnect countdown,
next attempt, four-attempt limit, and failure reason. Press `Esc` to cancel during the countdown.

For models that advertise a Fast service tier, Cagent requests priority routing in both the model
request and provider-specific routing metadata. ChatGPT/Codex connections are kept separate by
model and tier so switching Fast on or off does not reuse a connection from the other tier.
The ChatGPT backend may still label raw response usage as `service_tier: "default"`; upstream Codex
does not use that response field as confirmation of the requested tier.

## Connect a provider

Run `/providers`. Each provider is marked `enabled`, `disabled`, or `not setup`.

- Press `Enter` to enable or disable a configured provider.
- Select `not setup` to see setup instructions or enter an API key.
- Press `r` to reconnect or replace managed credentials.
- Press `d` to disconnect or remove managed credentials.

Keys entered in the UI are stored in the operating system credential service, not in TOML. You can
instead provide the provider's environment variable, such as `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`.

ChatGPT subscription/Codex and GitHub Copilot use browser or device-code login. Vertex AI can use
Google Application Default Credentials and requires a project ID in that mode. If setup fails, see
[Troubleshooting](/troubleshooting/#a-provider-is-disabled-or-not-set-up).

## Choose a model

Run `/model` or press `Alt+P`. Type to search across all enabled providers, choose a model, then
choose a reasoning effort when available. Press `f` to favourite a model. Can also use `/model <query>` to search and switch.

Your choice applies to the current [mode](/modes/). Switch modes and Cagent restores that mode's
model. Use `/model default` to clear the manual choice and return to configured defaults.

Set defaults in TOML:

```toml
default_model = { model = "openai/example-model", effort = "high" }
favourite_models = ["openai/example-model"]
```

See [core model options](/configuration/#core-options) for all defaults and valid shapes.

Or override one launch:

```sh
cagent --provider openai --model example-model --effort high
```

## Configure a provider

Built-in providers can usually be configured through `/providers`. TOML is useful for custom
endpoints or explicit model lists:

```toml
[providers.openai]
enabled = true
api_key_env = "OPENAI_API_KEY"

[providers.custom.company]
type = "openai-compatible"
enabled = true
base_url = "https://api.example.com/v1"
api_key_env = "COMPANY_API_KEY"
models = ["company-model"]
```

Provider entries may also define `display_name`, `models_endpoint`, aliases, and capability
overrides. Use capability overrides only when discovery is incomplete:

```toml
[providers.custom.company.capability_overrides.company-model]
supports_image_input = true
supports_structured_output = true
```

Unknown capabilities are treated as unsupported.

## GPT-6 Astra protocol features

OpenAI and ChatGPT GPT-6 Astra requests use the provider's native Responses features:

- `terminal_output`, `wait_join`, `web_search`, and `web_fetch` are declared asynchronous on standard
  Responses requests, allowing Astra to continue reasoning while client work is pending; mutation, approval, and interactive
  tools stay synchronous;
- plain-text input submitted during an active Astra WebSocket response is durably queued before
  Cagent sends `response.steer`. It leaves the queue only after provider acceptance, so a rejection
  or disconnect falls back to the normal next request boundary;
- changing reasoning effort between compatible retained ChatGPT responses uses `configuration_update`
  only when the provider catalog advertises Responses Lite, preserving the request-level effort and
  cached prompt prefix. Other models start a fresh request with the new effort; and
- ChatGPT models that advertise the upstream Codex Responses Lite protocol use stable input-prefix
  items for tools and base instructions, with IDs bounded to the provider's 64-character limit,
  plus all-turn reasoning context. Lite requests explicitly disable API-level parallel tool calls
  (including text-only requests) and omit image detail fields on both HTTP and WebSocket transports.
  Lite also omits the unsupported `async` tool flag; the same tools remain available as synchronous calls.
  Other models retain the standard Responses request shape and parallel-tool-calls setting.

Attachments, mode changes, special commands, unsupported models, and non-WebSocket providers keep
the existing boundary-queue behavior.

ChatGPT's `ultra` option is hidden because it selects Codex-specific orchestration rather than a
native reasoning effort. Cagent does not implement that mode. Other advertised efforts remain
available. Existing explicit `ultra` selections are omitted with an unsupported-effort notice;
requests that bypass that validation are rejected locally rather than sent to ChatGPT.

OpenAI and ChatGPT Responses retain encrypted reasoning as opaque provider state. This helps the
model continue after history is resent, including HTTP fallback or session resume. Cagent cannot
read it and does not display it in the transcript or log raw Responses payloads. It is replayed
only to the originating provider and model; switching providers does not send it elsewhere.
Completed primary responses persist it alongside the conversation, separately from visible history.
Delegated runs currently retain it only within their live request loop. Explicit `/context save`
exports can include these opaque envelopes and should be treated as private.

Custom providers use one OpenAI-compatible adapter. The `responses` and `open_ai_compatible` values
select request and streaming codecs within that adapter; they are not separate provider types.
Verify the endpoint's request, streaming, tool-call, and model-discovery compatibility. A provider
that merely uses an OpenAI-like URL is not guaranteed to implement every supported capability;
declare explicit models and use capability overrides only to correct discovery metadata.

See the [provider configuration reference](/configuration/#providers-and-models) for every provider
and capability field.

## Small model

Cagent uses a small, fast model for tasks such as title generation, automatic conversation recaps,
and automatic permission review. Recaps use only the most recent three user/assistant turns, run
without tools after three minutes of inactivity by default, do not run while waiting for user input,
and appear as a dedicated **Recap**
block. Configure or disable them in `/settings`.
Choose explicit candidates in `/settings`, or configure:

```toml
[tiers]
small = [{ model = "openai/example-fast-model", effort = "low" }]
```

The displayed default is `[]`. Cagent appends its provider-qualified system candidates at runtime,
so user configuration remains separate from built-in policy. You may define additional named tiers
and select them from agents and modes.

See [core model options](/configuration/#core-options) for precedence and defaults.

## Refresh models

Model catalogs are cached locally and refreshed in the background. Run `/reload` after changing
provider configuration. If a model is still missing, check that the provider is enabled and its
credentials are available to the Cagent process.

For providers with an integrated model endpoint—including ChatGPT, GitHub Copilot, OpenAI,
Anthropic, OpenRouter, OpenCode Zen, and Google—that endpoint is authoritative for availability.
Cagent may enrich the returned models with Models.dev metadata, but does not add Models.dev-only
IDs. ChatGPT therefore shows no synthetic fallback models before authenticated discovery completes.

## Related

- [Search and fetch](/search-and-fetch/) configures SearXNG, Exa, or ChatGPT/Codex search.
- [Modes](/modes/#models-by-mode) explains per-mode model selection.
- [Usage and costs](/usage-and-costs/) explains provider limits, token accounting, and pricing.
- [Configuration](/configuration/#providers-and-models) lists every provider field.
