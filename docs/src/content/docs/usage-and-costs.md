---
title: Usage and costs
description: Inspect token usage, provider limits, and estimated cost.
---

Run `/usage` to view usage for the current conversation, recent time windows, and all-time history.
You can open project and model breakdowns or reset global tracking.

Cagent records input, output, reasoning, and cache tokens when providers report them. Cost is shown
only when trustworthy provider or catalog pricing is available; estimated values are marked with
`~`. Historical entries keep the pricing metadata used when they were recorded.

These are provider usage fields, not a tokenizer run over the visible transcript. Hidden
instructions, tool protocol records, summaries, and provider-specific counting can make input totals
differ from what you see on screen. Missing fields remain unknown rather than being inferred.

Enable usage modules with `/statusline`:

- `provider_usage` shows provider-reported account limits when available. It is not calculated from
  Cagent's local token history.
- `context` shows the percentage of the model's context window in use.
- `cost`, `tokens`, and cache modules show local conversation totals.

Usage and cost metadata are stored locally with the conversation. Cagent does not invent prices for
unknown models.

Resetting global usage from `/usage` sets a local reporting watermark; it does not delete
conversations, change provider billing, or reset an account quota. Provider dashboards remain the
authority for invoices and enforced limits.

## Related

- [Status line](/status-line/#modules) explains each live usage module.
- [Context and compaction](/context-and-compaction/) explains context-window usage.
- [Providers and models](/providers-and-models/) explains catalog and provider discovery.
- [Conversations](/conversations/) explains where local history is retained.
