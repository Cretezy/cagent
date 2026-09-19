---
title: Configuration
description: Configure Cagent from the UI, CLI, or TOML.
---

Most of Cagent can be configured within its UI. Use **`/settings`** for common settings, or feature menus such as `/providers`, `/agent`, `/skills`,
and `/mcp` for their resources. Use TOML for advanced options or a shareable setup.

Configuration has three practical scopes:

1. Global `config.toml` supplies defaults for every workspace.
2. A trusted workspace's `.cagent/config.toml` supplies project MCP server overrides; other settings
   remain in the global configuration.
3. CLI flags override the selected model, provider, effort, agent, mode, directory, worktree, trust,
   and configuration paths for one launch.

Conversation choices such as the active mode and model are stored with that conversation. Valid
file edits are watched and applied at the next safe request or tool boundary; `/reload` applies them
immediately. Prefer to use the built-in credencial input within `/providers` for better security. 

## Configuration files

| OS | `config.toml` | `permissions.toml` |
| --- | --- | --- |
| Linux | `${XDG_CONFIG_HOME:-~/.config}/cagent/config.toml` | `${XDG_CONFIG_HOME:-~/.config}/cagent/permissions.toml` |
| macOS | `~/.config/cagent/config.toml` | `~/.config/cagent/permissions.toml` |
| Windows | `%APPDATA%\cagent\config.toml` | `%APPDATA%\cagent\permissions.toml` |

Override the locations for one launch:

```sh
cagent --config ./cagent.toml --data-dir ./cagent-data
```

A trusted workspace may also provide `.cagent/config.toml`. It currently contributes project MCP
servers only; project definitions override global servers with the same name.

## Edit configuration

`/settings` saves common options immediately. Type to search settings across every tab, and press
`Ctrl+R` on a setting to remove its explicit value and restore the default. The CLI provides direct
access:

```sh
cagent config view --safe
cagent config list
cagent config get default_model
cagent config set default_model '{ model = "openai/gpt-5.6-luna", effort = "high" }'
cagent config edit
```

`config view --safe` redacts secrets. `config set` accepts dotted TOML paths. `config edit` uses
`$VISUAL` or `$EDITOR` and validates the file when the editor closes.

## Full example

All sections except `version` are optional. This example shows every configuration category and
the reusable table shapes; replace the illustrative model IDs, commands, and endpoints.

```toml
# Required schema version. All other fields are optional.
version = 1
# Set false to omit the non-reversible machine fingerprint from conversation IDs.
machine_fingerprint = true

# Defaults for new conversations. Model IDs use provider/model.
default_agent = "general"
default_mode = "edit"
default_plan_exit_mode = "auto"
default_model = { model = "openai/gpt-5.6-luna", effort = "high" }
# Request priority service for supporting models. `/fast` and Alt+S toggle this globally.
fast = false
favourite_models = ["openai/gpt-5.6-luna"]
# Summarize eligible conversations after three idle minutes.
automatic_recaps = true
recap_idle_seconds = 180

[tiers]
# Explicit user candidates. The system small-model list is appended at runtime.
small = [{ model = "openai/gpt-5.6-luna", effort = "low" }]
# Arbitrary additional tiers contain concrete models.
powerful = [{ model = "openai/gpt-5.6-terra", effort = "high" }]

[title_generation]
enabled = true
# Valid range: 1–300 seconds.
timeout_seconds = 15

[subagents]
# off, on_demand, complex, aggressive, or always
strategy = "complex"
# Set to 0 to disable delegation.
max_concurrent = 10

[compaction]
enabled = true
# Compact when this percentage of the context window is used.
threshold_percent = 90

[conversation_cleanup]
automatic = true
# A positive human-readable size/duration, or false to disable that limit.
max_size = "10gb"
max_age = "1y"
max_conversations = false
# trash or delete
action = "trash"

[shell]
# Omit to use the platform default shell.
executable = "bash"
# -1 disables classification; 0 reads; 1 checks; 2 builds/tests; 3 formatters/fixers.
safe_level = 3
safe_write = false
# normal or dumb
terminal_mode = "normal"
login = true
# Additional process variables to expose to child shells.
forward_env = ["CI"]
timeout_seconds = 600
# Maximum output returned to the model and retained terminal output, in bytes.
output_bytes = 2097152
buffer_bytes = 8388608

[worktree]
# fresh, head, or another Git/Jujutsu revision
base = "fresh"
# Used for generated Git branch names.
branch_prefix = "worktree"
# false, "include", or "all"
copy = false

[limits]
# Attachment limits are bytes; the hard cap must be at least attachment_bytes.
attachment_bytes = 262144
attachment_hard_cap_bytes = 1048576
resize_images = true

[ui]
# dark or light
theme = "dark"
show_tips = true
scrollback_reflow_rows = 2000
composer_max_rows = 12
diff_context_lines = 3
diff_mode = "conversation" # or "git" for the Git/Jujutsu working-copy diff
# Report active work with terminal OSC 9;4 sequences.
progress_osc = true
# builtin, false, a known preset, a command array, or a command table
editor = "builtin"

[ui.colors]
# Colors accept a named terminal color or #RRGGBB.
background = "#101010"
foreground = "#E5E7EB"
surface = "#303030"
highlight = "cyan"
bash = "#FFA500"
muted = "#9CA3AF"
diff_added_background = "#213A2B"
diff_removed_background = "#4A221D"

[ui.file_picker]
respect_gitignore = true
hide_hidden_files = true

[ui.files]
width = 32

[ui.title]
# requested_input may also be "warnings", "triangle", "triangles", "exclamation", custom text, or an array of frames.
requested_input = "warning"
# false, a built-in spinner name, or an array of custom frames
progress = false

[ui.bell]
requested_input = true
completed_turn = true
# osc777 sends a desktop notification; bell emits BEL.
method = "osc777"

[ui.statusline]
# Modules appear in this order; remove a name to hide it.
modules = ["mode", "agent", "model", "provider", "fast", "context", "provider_usage", "cost", "hint"]

[ui.statusline.colors]
# Each entry overrides the named module's foreground color.
model = "light-cyan"
provider = "magenta"
fast = "red"
context = "yellow"

[keys]
# Each action accepts one or more chords; [] unbinds it.
insert_newline = ["shift+enter", "alt+enter"]
model_picker = ["alt+p"]
toggle_fast = ["alt+s"]
next_mode = ["shift+tab", "shift+right"]

[providers.openai]
enabled = true
# This is an environment-variable name, not the API key itself.
api_key_env = "OPENAI_API_KEY"
# Provider-local model ID, without the provider/ prefix.

[providers.google-vertex]
enabled = false
# adc or api-key; ADC requires project when enabled.
auth = "adc"
project = "example-project"
location = "global"

# Custom providers currently use the OpenAI-compatible adapter.
[providers.custom.company]
type = "openai-compatible"
enabled = false
display_name = "Company AI"
base_url = "https://api.example.com/v1"
# responses or open_ai_compatible
protocol = "open_ai_compatible"
api_key_env = "COMPANY_API_KEY"
# Relative to base_url, or an absolute URL. Omit to disable discovery.
models_endpoint = "/models"
# Explicit models remain available even without discovery.
models = ["company-model"]

[providers.custom.company.aliases]
fast = "company-model"

[providers.custom.company.capability_overrides.company-model]
# Use overrides only to correct missing or inaccurate discovery metadata.
context_window = 128000
supports_streaming = true
supports_tools = true
supports_structured_output = true
supports_text_input = true
supports_image_input = false
supports_text_output = true
reasoning_control = "effort"
reasoning_efforts = ["low", "medium", "high"]

[mode]
# Picker and keyboard-cycle order; omitted modes follow in their normal order.
order = ["edit", "auto", "plan", "read", "review"]

# A custom mode. The same fields can override a built-in mode.
[modes.review]
description = "Review without editing"
prompt = "Report prioritized findings with file references."
color = "light-cyan"
model = { model = "openai/gpt-5.6-luna", effort = "high" }
enabled = true
cycleable = true
# read/write/run: allow, ask, auto, or deny.
read = "allow"
write = "deny"
run = "ask"
plan = false

# Inline rules apply only while this mode is active.
[[modes.review.permissions]]
effect = "deny"
tool = "apply_patch"

# A custom profile inheriting the built-in general agent.
[agents.reviewer]
extends = "general"
description = "Read-focused reviewer"
prompt = "Check correctness, maintainability, and security."
# append or replace inherited prompt text
prompt_merge = "append"
model = { tier = "small", effort = "high" }
mode = "review"
# user, subagent, or both
availability = "both"
enabled = true
# Set true to discard inherited inline permission rules.
permissions_replace = false

# Per-mode model and effort overrides for this agent.
[agents.reviewer.modes.review]
model = { model = "openai/gpt-5.6-luna", effort = "high" }

[agents.reviewer.tools]
# Deny entries win over allow entries.
allow = ["bash"]
deny = ["apply_patch"]

[[agents.reviewer.permissions]]
effect = "deny"
tool = "bash"
command = ["rm", "*"]

# A global stdio MCP server. HTTP servers use transport = "http" and url.
[mcp.servers.docs]
transport = "stdio"
command = "npx"
args = ["-y", "@example/docs-server"]
# ${workspace} resolves to the active workspace.
cwd = "${workspace}"
# Secret values can be read from the environment at runtime.
env = { DOCS_TOKEN = { value = "${secret:mcp/docs/token}", description = "Documentation API token" } }
env_remove = []
inherit_env = true
enabled = true
# An empty array makes the server available to every agent.
agents = ["general", "reviewer"]
startup_timeout_seconds = 10
request_timeout_seconds = 60
# User attestation: only list tools you know are read-only.
read_only_tools = ["search"]

[web_search]
# searxng, exa, or chatgpt
provider = "searxng"
timeout_seconds = 20

[web_search.searxng]
# Alternatively set url = "https://search.example.com" directly.
url_env_var = "SEARXNG_URL"

[web_search.exa]
api_key_env_var = "EXA_API_KEY"

[web_fetch.redirects]
# Reuse the original approval for these redirect classes.
generally_safe = true
same_site = true

[skills]
# Skill names to keep installed but unavailable.
disabled = []

[skills.bundled]
enabled = true

[compatibility]
# Read compatible Agent, Claude, Codex, and OpenCode instruction/skill roots.
external_agents = true
```

## Core options

| Key | Values and default | Description |
| --- | --- | --- |
| `version` | `1`; required | Configuration schema version. |
| `machine_fingerprint` | Boolean; `true` | Include a non-reversible machine-derived byte in new conversation IDs. |
| `default_agent` | Agent name; `general` | Agent for new conversations. |
| `default_mode` | Enabled mode name; `edit` | Mode for new conversations. |
| `default_plan_exit_mode` | Enabled, cycleable, non-planning mode; inherits `default_mode` | Mode initially selected in the plan acceptance menu. If `default_mode` is a planning or non-cycleable mode, the first eligible mode is used. |
| `default_model` | Structured `{ model = "provider/model", effort = "..." }`; automatic when absent | Default model and optional reasoning effort. A tier target may also be used. |
| `tiers.<name>` | Array of `{ model = "provider/model", effort = "..." }`; `tiers.small = []` | Concrete candidates for a reusable tier. System candidates are appended only to `small` at runtime. |
| `fast` | Boolean; `false` | Request the selected model's Fast service tier when its catalog metadata supports it. This global preference may use higher pricing. |
| `favourite_models` | Array of `provider/model`; `[]` | Models highlighted in the picker. |
| `automatic_recaps` | Boolean; `true` | Generate a short recap after an eligible completed conversation becomes idle and is not waiting for user input. |
| `recap_idle_seconds` | Positive integer; `180` | Conversation inactivity before an automatic recap. |

Use `/recap` in an idle conversation to generate an eligible recap immediately instead of waiting
for the automatic idle delay. Recaps are unavailable while a permission, question, plan decision,
or other interaction is waiting for your response.

### Title generation and delegation

| Key | Values and default | Description |
| --- | --- | --- |
| `title_generation.enabled` | Boolean; `true` | Refine a conversation title after its first message. |
| `title_generation.timeout_seconds` | `1..=300`; `15` | Title-generation deadline. |
| `subagents.strategy` | `off`, `on_demand`, `complex`, `aggressive`, `always`; `complex` | How readily the primary agent delegates work. `always` limits the primary agent to coordination tools and delegates every substantive request. |
| `subagents.max_concurrent` | Non-negative integer; `10` | Concurrent sub-agent limit; `0` disables delegation. |

## Providers and models

Built-in IDs are `chatgpt`, `github-copilot`, `openai`, `opencode`, `anthropic`, `openrouter`,
`google`, and `google-vertex`. Configure one at `[providers.<id>]`. Custom providers use
`[providers.custom.<id>]` and must set `type = "openai-compatible"`.

| Provider key | Values and default | Description |
| --- | --- | --- |
| `type` | `openai-compatible`; required for custom providers | Provider adapter type. Built-in providers already define their type. |
| `enabled` | Boolean; `false` | Make the provider selectable. |
| `api_key_env` | Environment-variable name | Read the API key from this variable; never put the key itself here. |
| `display_name` | String | Override the picker label. |
| `base_url` | Absolute HTTP(S) URL | API root, usually ending in `/v1`; required for custom providers. Plain HTTP is accepted, so use it only for a trusted endpoint. |
| `protocol` | `responses` or `open_ai_compatible` | Request protocol for a custom provider. |
| `models_endpoint` | Relative path or absolute HTTP(S) URL | Override model discovery. A relative path is joined to `base_url`; an absolute URL bypasses it. When absent, built-ins use canonical discovery and custom discovery is disabled. |
| `usage_limit` | Provider usage-window ID | Choose the usage window shown in the statusline. |
| `models` | Array of model IDs; `[]` | Add explicit models. |
| `aliases` | String map; `{}` | Map short aliases to model IDs. |
| `capability_overrides` | Table keyed by model ID | Correct incomplete discovered capabilities. |
| `auth` | `adc` or `api-key`; `adc` | Google Vertex authentication mode. |
| `project` | String | Google Cloud project; required for enabled Vertex ADC. |
| `location` | String; provider default is `global` | Google Vertex location. |

Each `capability_overrides.<model>` table accepts `context_window`, `supports_streaming`,
`supports_tools`, `supports_structured_output`, `supports_text_input`, `supports_image_input`,
`supports_text_output`, `reasoning_control` (`effort` or `toggle`), and `reasoning_efforts` (an array
of strings).

See [Providers and models](/providers-and-models/) for authentication, discovery, selection, and
the complete supported-provider matrix.

## Modes

Configure built-in or custom modes at `[modes.<name>]`.

| Key | Values and default for custom modes | Description |
| --- | --- | --- |
| `description` | String; empty | Picker description. |
| `prompt` | String; empty | Additional mode instructions. |
| `color` | Named color or `#RRGGBB`; assigned automatically | Mode indicator color. |
| `model` | `{ model = "provider/model", effort = "..." }` or `{ tier = "name", effort = "..." }`; inherited default | Combined model target and optional effort used in this mode. An inherited override may contain only effort. |
| `enabled` | Boolean; `true` | Make the mode selectable. |
| `cycleable` | Boolean; `true` | Include it in keyboard cycling. |
| `read` | `allow`, `ask`, `auto`, `deny`; `allow` | Fallback read policy; `auto` allows project reads and reviews unresolved external tool reads. |
| `write` | `allow`, `ask`, `auto`, `deny`; `ask` | Fallback write policy; `auto` allows project writes and reviews unresolved external writes. |
| `run` | `allow`, `ask`, `auto`, `deny`; `ask` | Fallback command policy. |
| `auto_level` | `medium`, `high`; `high` | Reviewer guidance: `high` prefers automatic approval through medium risk; `medium` prompts starting at medium risk. |
| `plan` | Boolean; `false` | Apply plan-mode behavior. |
| `permissions` | Array of rule tables; `[]` | Mode-specific permission rules. |

`mode.order` is an array of mode names that sets picker and cycle order; omitted names follow in
their normal order. See [Modes](/modes/#add-a-custom-mode) for an example.

## Agents

Configure profiles at `[agents.<name>]`.

| Key | Values and default | Description |
| --- | --- | --- |
| `extends` | Agent name | Inherit another profile. |
| `description` | String; empty | Picker and delegation description. |
| `prompt` | String; empty | Profile instructions. |
| `prompt_merge` | `append` or `replace`; `append` | Combine `prompt` with inherited instructions or replace them. |
| `model` | `{ model = "provider/model", effort = "..." }` or `{ tier = "name", effort = "..." }` | Combined model target and optional effort. A delegated profile with no target inherits the conversation's current effective model and effort; an effort-only inherited override keeps that model. |
| `mode` | Mode name | Default mode when this agent is selected. |
| `availability` | `user`, `subagent`, `both`; `user` | Where the profile can be selected. |
| `enabled` | Boolean; `true` | Keep or hide the profile. |
| `permissions` | Array of rule tables; `[]` | Profile-specific rules. |
| `permissions_replace` | Boolean; `false` | Discard inherited permission rules before adding this profile's rules. |
| `tools.allow`, `tools.deny` | Arrays of tool names | Allowlist or denylist tools; deny wins. |
| `modes.<mode>.model` | Structured model selection | Per-mode target and/or effort override for this agent. |

See [Agents and delegation](/agents/#add-a-custom-agent) for a complete profile example.

### Inline permission rules

Entries under `[[modes.<name>.permissions]]` and `[[agents.<name>.permissions]]` accept `effect`
(`allow`, `ask`, or `deny`) plus optional `tool`, `server`, `operation`, `path`, `command`,
`raw_command`, `cwd`, `access`, `external`, `mode`, and `agent` constraints. `id`, `source`, and
`created_at` are optional for inline rules. See [Permissions](/permissions/) for matching behavior
and the separate `permissions.toml` schema.

## Shell

| Key | Values and default | Description |
| --- | --- | --- |
| `shell.executable` | Executable path/name; platform shell when absent | Shell used for commands. |
| `shell.safe_level` | `-1..=3`; `3` | Highest cumulative built-in safe class: disabled, reads, checks, builds/tests, or formatters/fixers. |
| `shell.safe_write` | Boolean; `false` | Permit the narrow safe-write grammar when the mode uses `write = "allow"`. |
| `shell.terminal_mode` | `normal` or `dumb`; `normal` | Terminal capabilities advertised to commands. |
| `shell.login` | Boolean; `true` | Launch the configured shell as a login shell. |
| `shell.forward_env` | Array of names; `[]` | Additional environment variables passed to child commands. |
| `shell.timeout_seconds` | `1..=600`; `600` | Foreground command timeout. |
| `shell.output_bytes` | Positive integer; `2097152` | Maximum command output returned to the model. |
| `shell.buffer_bytes` | Integer at least `output_bytes`; `8388608` | Retained supervised-terminal buffer. |

See [Shell and background work](/shell-and-background-work/) for interactive operation and
[Permissions](/permissions/#safe-commands) for classifier behavior.

## Interface and files

| Key | Values and default | Description |
| --- | --- | --- |
| `ui.theme` | `dark` or `light`; `dark` | Base color theme. |
| `ui.show_tips` | Boolean; `true` | Show tips in new conversations. |
| `ui.collapse_tool_activity` | Boolean; `true` | Collapse consecutive reads, lists, local and web searches, web fetches, edits, and Bash commands into a clickable live summary. |
| `ui.open_links` | Boolean; `true` | Open HTTP(S) Markdown links in the default browser with a single left click. `false` disables app-handled web-link clicks; terminal OSC 8 links remain available through the terminal's own gesture. Local file hits still follow `ui.editor`. |
| `ui.scrollback_reflow_rows` | Positive integer; `2000` | Transcript rows reflowed after resize. |
| `ui.composer_max_rows` | Positive integer; unlimited when absent | Maximum composer height. |
| `ui.diff_context_lines` | Positive integer; `3` | Unchanged lines shown around edits. |
| `ui.diff_mode` | `conversation` or `git`; `conversation` | Default for `/diff`, also exposed as **Default diff mode** in `/settings`. Applies on the next invocation. `git` supports Git and Jujutsu; explicit subcommands do not change this setting. |
| `ui.progress_osc` | Boolean; `true` | Emit OSC 9;4 work progress. |
| `ui.editor` | See below; `builtin` | Program used to open paths. |
| `ui.file_picker.respect_gitignore` | Boolean; `true` | Hide ignored paths in completion. |
| `ui.file_picker.hide_hidden_files` | Boolean; `true` | Hide dotfiles in completion. |
| `ui.files.width` | Positive integer; `32` | Initial `/files` sidebar width. |
| `limits.attachment_bytes` | Positive bytes; `262144` | Implicit capture limit; larger un-ranged files remain path references. |
| `limits.attachment_hard_cap_bytes` | Positive bytes; `1048576` | Explicit attachment maximum; must be at least `attachment_bytes`. |
| `limits.resize_images` | Boolean; `true` | Resize model-bound images to a 2048px maximum dimension. |

`ui.editor` accepts `false`, `builtin`, or a preset: `vscode`, `vscode-insiders`, `cursor`, `zed`,
`zed-preview`, `intellij`, `webstorm`, `pycharm`, `rustrover`, `goland`, `clion`, `rider`, `fleet`,
`sublime`, `lapce`, `emacs`, `neovim`, `vim`, or `helix`. A command array launches a custom
foreground editor. For full control use
`{ command = ["editor"], line_command = ["editor", "{path}:{line}"], mode = "foreground" }`;
`mode` is `foreground` or `background`, and `line_command` is optional.

### Colors, title, bell, and statusline

`ui.colors` accepts `background`, `foreground`, `surface`, `highlight`, `bash`, `muted`,
`diff_added_background`, and `diff_removed_background`. Values are `#RRGGBB` or one of `black`,
`red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, `dark-gray`, `light-red`,
`light-green`, `light-yellow`, `light-blue`, `light-magenta`, `light-cyan`, and `white`.

| Key | Values and default | Description |
| --- | --- | --- |
| `ui.title.requested_input` | `warning`, `warnings`, `triangle`, `triangles`, `exclamation`, a string, or non-empty string array; `warning` | Terminal-title marker while input is needed. Plural presets and `exclamation` animate between multiple frames. |
| `ui.title.progress` | `false`, a style, or non-empty string array; `false` | Terminal-title spinner. Styles: `braille`, `quadrant`, `arc`, `circle`, `line`, `block`, `dots`, `nerd_circle`, `nerd_arrow`. |
| `ui.bell.requested_input` | Boolean; `true` | Notify when input is required. |
| `ui.bell.completed_turn` | Boolean; `true` | Notify after a normal turn. |
| `ui.bell.method` | `osc777` or `bell`; `osc777` | Desktop notification or terminal BEL. |
| `ui.statusline.modules` | Array; `["mode", "agent", "model", "provider", "fast", "context", "provider_usage", "cost", "hint"]` | Ordered visible modules. The `fast` module appears only while Fast is effective. |
| `ui.statusline.colors.<module>` | Color | Override a module color. |

Statusline modules are `mode`, `agent`, `model`, `provider`, `fast`, `provider_usage`, `context`, `cost`,
`tokens`, `token_rate`, `cache`, `cache_tokens`, `context_tokens`, and `hint`. The opt-in
`token_rate` module shows average provider-reported tokens per second over the trailing 60 seconds of
active model work as `in:123/s out:71/s`; it updates with newly reported token usage, resets on model
changes, and hides at zero.

### Keybindings

Each `keys.<action>` value is an array of chords; `[]` disables that action. Available actions and
defaults are:

| Action | Default |
| --- | --- |
| `submit` | `enter` |
| `insert_newline` | `shift+enter`, `alt+enter` |
| `model_picker` | `alt+p` |
| `toggle_fast` | `alt+s` |
| `tree` | `alt+t` |
| `fork` | `alt+f` |
| `rename` | `alt+n` |
| `retry_turn` | `alt+r` |
| `external_editor` | `ctrl+g` |
| `line_start`, `line_end`, `delete_word`, `undo` | `ctrl+a`, `ctrl+e`, `ctrl+w`, `ctrl+u` respectively |
| `cancel`, `exit`, `close_surface` | `ctrl+c`, `ctrl+d`, `esc` respectively |
| `previous_mode` | `shift+left` |
| `next_mode` | `shift+right`, `shift+tab` |
| `navigate_left`, `navigate_right`, `navigate_up`, `navigate_down` | Corresponding arrow key |
| `background` | `alt+down` |
| `complete` | `tab` |
| `toggle_chip` | `alt+e` |
| `delete_queued` | `d` |
| `promote_queued`, `scroll_to_message` | `s` |

## Search and fetch

| Key | Values and default | Description |
| --- | --- | --- |
| `web_search.provider` | `searxng`, `exa`, or `chatgpt`; none | Search backend. |
| `web_search.timeout_seconds` | `1..=120`; `20` | Search deadline. |
| `web_search.searxng.url` | HTTP(S) URL | Literal SearXNG endpoint. |
| `web_search.searxng.url_env_var` | Environment-variable name; `SEARXNG_URL` | Variable containing the endpoint. |
| `web_search.exa.api_key_env_var` | Environment-variable name; `EXA_API_KEY` | Variable containing the Exa key. |
| `web_fetch.redirects.generally_safe` | Boolean; `true` | Reuse approval for HTTP-to-HTTPS and `www` canonical redirects. |
| `web_fetch.redirects.same_site` | Boolean; `true` | Reuse approval for same-site path or query redirects. |

## MCP servers

Define servers at `[mcp.servers.<name>]`. See [MCP](/mcp/) for setup commands.

| Key | Values and default | Description |
| --- | --- | --- |
| `transport` | `stdio`, `http`, `streamable_http`, or `streamable-http`; required | Server transport. |
| `package` | `builtin:<id>` or HTTPS URL | Option B package reference. URL sources require `package_digest`; bundled sources follow the Cagent release. |
| `package_digest` | `sha256:<digest>` | Required content lock for an HTTPS package manifest; omitted for built-ins. |
| `parameters` | Map; `{}` | Values for keyed package `[parameters.<id>]` declarations. |
| `command` | String; required for `stdio` | Executable. |
| `args` | String array; `[]` | Command arguments. |
| `runner` | `host` or `oci`; `host` | Run a stdio MCP directly or through Docker/Podman. |
| `image` | OCI image with `@sha256:<digest>` | Required immutable image for `runner = "oci"`. |
| `cwd` | String | Command directory; `${workspace}` is supported. |
| `env` | String or `{ value, description }` map; `{}` | Command environment; values may reference managed `${secret:mcp/...}` or `${env:NAME}`. |
| `env_remove` | String array; `[]` | Inherited variables to remove. |
| `inherit_env` | Boolean; `true` | Inherit the Cagent process environment. |
| `url` | URL; required for HTTP | Streamable HTTP endpoint. |
| `headers` | String or `{ value, description }` map; `{}` | HTTP headers; values may reference managed `${secret:mcp/...}` or `${env:NAME}`. |
| `oauth` | Table; none | Optional host-managed OAuth hints for an HTTP MCP. Standard metadata is discovered without this table. Supports `client_id`, optional `client_secret`, `scopes`, optional `device_authorization_endpoint`, static `issuer`/`authorization_endpoint`/`token_endpoint`, and a described `bearer_token` fallback. |
| `allow_insecure` | Boolean; `false` | Permit an `http://` endpoint. |
| `enabled` | Boolean; `true` | Enable the server. |
| `agents` | Agent-name array; `[]` means all | Profiles allowed to activate it. |
| `startup_timeout_seconds` | Positive integer; `10` | Startup deadline. |
| `request_timeout_seconds` | Positive integer; `60` | Tool request deadline. |
| `read_only_tools` | Tool-name array; `[]` | User attestation that listed server tools are read-only. |

Package definitions are separate declarative catalog data; a server stores a source/ID/digest
reference plus parameter values rather than a copied definition. URL manifests are cached and
SHA-256 locked until an explicit update. OCI defaults deny network and mounts and use a read-only,
capability-dropped, no-new-privileges container; requested exceptions are shown for confirmation.

## Context, retention, skills, and worktrees

| Key | Values and default | Description |
| --- | --- | --- |
| `compaction.enabled` | Boolean; `true` | Compact context automatically. |
| `compaction.threshold_percent` | `1..=100`; `90` | Context usage that triggers compaction. |
| `conversation_cleanup.automatic` | Boolean; `true` | Run retention cleanup automatically. |
| `conversation_cleanup.max_size` | Positive size such as `10gb`, or `false`; `10gb` | Maximum retained conversation bytes. |
| `conversation_cleanup.max_age` | Positive duration such as `30d`, or `false`; `1y` | Maximum conversation age. |
| `conversation_cleanup.max_conversations` | Positive integer or `false`; `false` | Maximum retained count. |
| `conversation_cleanup.action` | `trash` or `delete`; `trash` | How expired conversations are removed. |
| `skills.disabled` | Skill-name array; `[]` | Disable matching skills. |
| `skills.bundled.enabled` | Boolean; `true` | Install and expose bundled system skills. |
| `compatibility.external_agents` | Boolean; `true` | Read compatible Claude, Codex, OpenCode, and Agent instruction/skill roots. |
| `worktree.base` | `fresh`, `head`, or revision; `fresh` | Base used by `--worktree`. |
| `worktree.branch_prefix` | String; `worktree` | Generated Git branch prefix. |
| `worktree.copy` | `false`, `include`, or `all`; `false` | Copy ignored files selected by `.gitignore`: none, included exceptions, or all. |

See [Context and compaction](/context-and-compaction/), [Conversations](/conversations/#cleanup),
[Skills](/skills/), and [Worktrees](/worktrees/) for the corresponding workflows.

## Session-only overrides and reloads

Global CLI options can override the model, provider, effort, agent, mode, directory, worktree,
trust, and configuration path without editing TOML. See [CLI arguments](/cli-arguments/).

Cagent watches configuration and applies valid edits at the next safe request or tool boundary. An
invalid edit leaves the last valid configuration active and reports an error. `/reload` immediately
reloads local configuration, instructions, and skills, reconciles MCP, and forces a model-catalog
refresh in the background.

## Related

- [Interactive reference](/interactive-reference/) lists commands and configurable key actions.
- [Customize the interface](/customize-interface/) covers the most common UI settings.
- [CLI arguments](/cli-arguments/) lists session-only overrides and configuration commands.
- [Troubleshooting](/troubleshooting/#a-configuration-edit-was-rejected) explains invalid edits.
