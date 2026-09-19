#![allow(clippy::ptr_arg, clippy::single_char_add_str)] // Request assembly mutates an owned transcript buffer across helper boundaries.
//! Provider-neutral request construction and built-in tool schemas.

use crate::provider::{
    ModelContentPart, ModelInput, ModelRef, ModelRequest, PromptCacheRequest, PromptCacheScope,
    StablePromptPart, ToolDefinition,
};

pub(super) const REQUEST_USER_INPUT_TOOL: &str = "request_user_input";
pub(super) const UPDATE_PLAN_TOOL: &str = "update_plan";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageTokenEstimate {
    OpenAi,
    Anthropic { max_tokens: u64 },
    Gemini,
    Conservative,
}

impl ImageTokenEstimate {
    pub(super) fn for_model(model: &ModelRef, backend: Option<crate::ModelBackend>) -> Self {
        match backend {
            Some(crate::ModelBackend::OpenAiResponses) => Self::OpenAi,
            Some(crate::ModelBackend::AnthropicMessages) => Self::Anthropic {
                max_tokens: anthropic_image_token_limit(&model.model),
            },
            Some(crate::ModelBackend::Gemini) => Self::Gemini,
            Some(crate::ModelBackend::OpenAiCompatible) | None => {
                let model_name = model.model.to_ascii_lowercase();
                if model_name.starts_with("anthropic/") || model_name.contains("claude") {
                    Self::Anthropic {
                        max_tokens: anthropic_image_token_limit(&model_name),
                    }
                } else if model_name.starts_with("google/") || model_name.contains("gemini") {
                    Self::Gemini
                } else if model_name.starts_with("openai/")
                    || model_name.starts_with("gpt-")
                    || model_name.starts_with("o1")
                    || model_name.starts_with("o3")
                    || model_name.starts_with("o4")
                {
                    Self::OpenAi
                } else {
                    Self::Conservative
                }
            }
        }
    }
}

fn estimate_text_tokens(text: &str) -> u64 {
    u64::try_from(text.len().div_ceil(4)).unwrap_or(u64::MAX)
}

fn anthropic_image_token_limit(model: &str) -> u64 {
    let model = model.to_ascii_lowercase();
    if model.contains("opus-4-7")
        || model.contains("opus-4.7")
        || model.contains("opus-4-8")
        || model.contains("opus-4.8")
        || model.contains("sonnet-5")
        || model.contains("fable-5")
        || model.contains("mythos-5")
    {
        4_784
    } else {
        1_568
    }
}

fn scale_to_short_edge(width: u64, height: u64, short_edge: u64) -> (u64, u64) {
    let shortest = width.min(height);
    if shortest <= short_edge {
        return (width, height);
    }
    (
        width.saturating_mul(short_edge).div_ceil(shortest),
        height.saturating_mul(short_edge).div_ceil(shortest),
    )
}

fn scale_to_max_edge(width: u64, height: u64, max_edge: u64) -> (u64, u64) {
    let longest = width.max(height);
    if longest <= max_edge {
        return (width, height);
    }
    (
        width.saturating_mul(max_edge).div_ceil(longest),
        height.saturating_mul(max_edge).div_ceil(longest),
    )
}

fn estimate_image_tokens(width: u32, height: u32, estimate: ImageTokenEstimate) -> u64 {
    let width = u64::from(width.max(1));
    let height = u64::from(height.max(1));
    match estimate {
        ImageTokenEstimate::OpenAi => {
            let (width, height) = scale_to_max_edge(width, height, 2_048);
            let (width, height) = scale_to_short_edge(width, height, 768);
            let tiles = width.div_ceil(512).saturating_mul(height.div_ceil(512));
            85_u64.saturating_add(tiles.saturating_mul(170))
        }
        ImageTokenEstimate::Anthropic { max_tokens } => width
            .div_ceil(28)
            .saturating_mul(height.div_ceil(28))
            .min(max_tokens),
        ImageTokenEstimate::Gemini => {
            if width <= 384 && height <= 384 {
                258
            } else {
                width
                    .div_ceil(768)
                    .saturating_mul(height.div_ceil(768))
                    .saturating_mul(258)
            }
        }
        ImageTokenEstimate::Conservative => {
            let tiles = width.div_ceil(512).saturating_mul(height.div_ceil(512));
            85_u64.saturating_add(tiles.saturating_mul(170))
        }
    }
}

pub(super) fn estimate_model_input_tokens(
    input: &ModelInput,
    image_estimate: ImageTokenEstimate,
) -> u64 {
    match input {
        ModelInput::Message { content, .. } => estimate_text_tokens(content),
        ModelInput::MultimodalMessage { content, .. } => content
            .iter()
            .map(|part| match part {
                ModelContentPart::Text { text } => estimate_text_tokens(text),
                ModelContentPart::Image { width, height, .. } => {
                    estimate_image_tokens(*width, *height, image_estimate)
                }
            })
            .sum(),
        ModelInput::ToolCall { .. }
        | ModelInput::ProviderReasoning { .. }
        | ModelInput::ToolResult { .. }
        | ModelInput::ConfigurationUpdate { .. } => serde_json::to_vec(input)
            .map(|value| u64::try_from(value.len().div_ceil(4)).unwrap_or(u64::MAX))
            .unwrap_or_default(),
    }
}

pub(super) fn estimate_model_inputs_tokens(
    input: &[ModelInput],
    image_estimate: ImageTokenEstimate,
) -> u64 {
    input
        .iter()
        .map(|input| estimate_model_input_tokens(input, image_estimate))
        .sum()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // Shared request helpers follow image-estimation tests.
mod image_token_tests {
    use super::*;

    #[test]
    fn common_provider_image_estimates_follow_their_native_units() {
        assert_eq!(
            estimate_image_tokens(1_456, 816, ImageTokenEstimate::OpenAi),
            1_105
        );
        assert_eq!(
            estimate_image_tokens(
                1_456,
                816,
                ImageTokenEstimate::Anthropic { max_tokens: 1_568 }
            ),
            1_560
        );
        assert_eq!(
            estimate_image_tokens(1_456, 816, ImageTokenEstimate::Gemini),
            1_032
        );
    }

    #[test]
    fn openai_estimate_accounts_for_server_side_downscaling() {
        assert_eq!(
            estimate_image_tokens(4_000, 1_000, ImageTokenEstimate::OpenAi),
            765
        );
    }

    #[test]
    fn backend_and_routed_model_choose_the_expected_estimate() {
        assert_eq!(
            ImageTokenEstimate::for_model(
                &ModelRef {
                    provider: "github-copilot".into(),
                    model: "claude-sonnet-4.6".into(),
                },
                Some(crate::ModelBackend::AnthropicMessages),
            ),
            ImageTokenEstimate::Anthropic { max_tokens: 1_568 }
        );
        assert_eq!(
            ImageTokenEstimate::for_model(
                &ModelRef {
                    provider: "openrouter".into(),
                    model: "google/gemini-3.6-flash".into(),
                },
                Some(crate::ModelBackend::OpenAiCompatible),
            ),
            ImageTokenEstimate::Gemini
        );
    }
}

pub(super) fn estimate_request_tokens(
    request: &ModelRequest,
    image_estimate: ImageTokenEstimate,
) -> u64 {
    // Image data is transported as base64 but providers charge for decoded
    // image units, not the encoded byte count. Measure non-input request
    // structure separately, then apply the provider-aware input estimator.
    let mut envelope = request.clone();
    envelope.input.clear();
    let bytes = serde_json::to_vec(&envelope).map_or(0, |encoded| encoded.len());
    let continuation_tokens = request
        .response_transport_continuation
        .as_ref()
        .map_or(0, |continuation| {
            32_u64.saturating_add(estimate_text_tokens(&continuation.response_id))
        });
    u64::try_from(bytes.div_ceil(4))
        .unwrap_or(u64::MAX)
        .saturating_add(continuation_tokens)
        .saturating_add(estimate_model_inputs_tokens(&request.input, image_estimate))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn model_request(
    request_id: crate::RequestId,
    attempt_id: crate::AttemptId,
    conversation_id: crate::ConversationId,
    model: ModelRef,
    backend: Option<crate::ModelBackend>,
    backend_candidates: Vec<crate::ModelBackend>,
    effort: Option<String>,
    input: Vec<ModelInput>,
    agent: Option<&crate::AgentProfile>,
    mode: Option<&crate::ModeProfile>,
    web_search: Option<&crate::WebSearchConfig>,
    tool_policy: Option<&crate::ToolPolicy>,
) -> ModelRequest {
    let shell_inventory =
        crate::ShellCommandInventory::synthetic(&crate::safe_shell_command_names());
    model_request_with_delegation_policy(
        request_id,
        attempt_id,
        conversation_id,
        model,
        backend,
        backend_candidates,
        effort,
        input,
        agent,
        mode,
        web_search,
        web_search
            .and_then(crate::WebSearchConfig::resolve)
            .is_some(),
        tool_policy,
        crate::DelegationPolicy::Complex,
        true,
        tool_policy.is_none(),
        &shell_inventory,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn model_request_with_delegation_policy(
    request_id: crate::RequestId,
    attempt_id: crate::AttemptId,
    conversation_id: crate::ConversationId,
    model: ModelRef,
    backend: Option<crate::ModelBackend>,
    backend_candidates: Vec<crate::ModelBackend>,
    effort: Option<String>,
    input: Vec<ModelInput>,
    agent: Option<&crate::AgentProfile>,
    mode: Option<&crate::ModeProfile>,
    _web_search: Option<&crate::WebSearchConfig>,
    web_search_available: bool,
    tool_policy: Option<&crate::ToolPolicy>,
    delegation_policy: crate::DelegationPolicy,
    subagents_enabled: bool,
    interactive_approvals_available: bool,
    shell_inventory: &crate::ShellCommandInventory,
    delegated_agents: Option<&crate::AgentCatalog>,
    scratchpad: Option<&std::path::Path>,
) -> ModelRequest {
    // Keep the most broadly shared core policy first, followed by the selected
    // agent and finally workspace-specific instructions.
    let mut stable_prompt = Vec::new();
    if delegation_policy != crate::DelegationPolicy::Always
        && tools_will_include_apply_patch(agent, tool_policy)
    {
        stable_prompt.push(StablePromptPart {
            identity: "cagent:file-mutation-policy".into(),
            content: crate::prompts::FILE_MUTATION_POLICY.into(),
        });
    }
    stable_prompt.push(StablePromptPart {
        identity: "cagent:tool-use-policy".into(),
        content: crate::prompts::TOOL_USE_POLICY.into(),
    });
    if delegation_policy != crate::DelegationPolicy::Always
        && !interactive_approvals_available
        && tools_will_include_bash(agent, tool_policy)
    {
        stable_prompt.push(StablePromptPart {
            identity: "cagent:approval-less-bash".into(),
            content: crate::prompts::APPROVALLESS_BASH_POLICY.into(),
        });
    }
    stable_prompt.push(StablePromptPart {
        identity: "cagent:user-communication".into(),
        content: crate::prompts::USER_COMMUNICATION_POLICY.into(),
    });
    stable_prompt.push(StablePromptPart {
        identity: "cagent:path-references".into(),
        content: crate::prompts::PATH_REFERENCE_POLICY.into(),
    });
    stable_prompt.push(StablePromptPart {
        identity: "cagent:local-context".into(),
        content: crate::prompts::LOCAL_CONTEXT_POLICY.into(),
    });
    if let Some(scratchpad) = scratchpad {
        stable_prompt.push(StablePromptPart {
            identity: "cagent:scratchpad".into(),
            content: crate::prompts::scratchpad_policy(scratchpad),
        });
    }
    stable_prompt.push(StablePromptPart {
        identity: format!(
            "cagent:delegation-coordination:{}",
            delegation_policy.as_str()
        ),
        content: delegation_policy_prompt(delegation_policy).into(),
    });
    if let Some(agent) = agent.filter(|agent| !agent.prompt.is_empty()) {
        stable_prompt.push(StablePromptPart {
            identity: format!("agent:{}", agent.name),
            content: agent.prompt.clone(),
        });
    }
    let tools: Vec<_> = builtin_tool_definitions_with_agents(shell_inventory, delegated_agents)
        .into_iter()
        .filter(|tool| tool_policy.is_none_or(|policy| policy.allows(&tool.name)))
        .filter(|tool| agent.is_none_or(|agent| agent.allows_tool(&tool.name)))
        .filter(|tool| {
            subagents_enabled || !matches!(tool.name.as_str(), "delegate_agent" | "wait_join")
        })
        .filter(|tool| !mode.is_some_and(|mode| mode.plan && tool.name == UPDATE_PLAN_TOOL))
        .collect();
    let mut tools = tools;
    if web_search_available
        && agent.is_none_or(|agent| agent.name != "explore")
        && agent.is_none_or(|agent| agent.allows_tool("web_search"))
        && tool_policy.is_none_or(|policy| policy.allows("web_search"))
    {
        tools.push(web_search_tool_definition());
    }
    if agent.is_none_or(|agent| agent.allows_tool("web_fetch"))
        && tool_policy.is_none_or(|policy| policy.allows("web_fetch"))
    {
        tools.push(web_fetch_tool_definition());
    }
    retain_primary_tools_for_delegation_policy(&mut tools, delegation_policy);
    // A provider cache keys the serialized tool definitions, not Rust's
    // semantic representation. Keep both built-in and subsequently merged
    // definitions in a canonical order so harmless map insertion order does
    // not create a cache boundary.
    canonicalize_tools(&mut tools);
    ModelRequest {
        request_id,
        attempt_id,
        model,
        backend,
        backend_candidates,
        effort,
        service_tier: None,
        input,
        tools,
        stable_prompt,
        prompt_cache: Some(PromptCacheRequest {
            key: format!("cagent:conversation:{conversation_id}"),
            scope: PromptCacheScope::Conversation,
        }),
        response_transport_continuation: None,
        allow_parallel_tools: true,
        structured_output: None,
    }
}

pub(super) fn retain_primary_tools_for_delegation_policy(
    tools: &mut Vec<ToolDefinition>,
    policy: crate::DelegationPolicy,
) {
    if policy == crate::DelegationPolicy::Always {
        tools.retain(|tool| {
            matches!(
                tool.name.as_str(),
                "delegate_agent" | "wait_join" | REQUEST_USER_INPUT_TOOL | UPDATE_PLAN_TOOL
            )
        });
    }
}

fn tools_will_include_apply_patch(
    agent: Option<&crate::AgentProfile>,
    tool_policy: Option<&crate::ToolPolicy>,
) -> bool {
    agent.is_none_or(|agent| agent.allows_tool("apply_patch"))
        && tool_policy.is_none_or(|policy| policy.allows("apply_patch"))
}

fn tools_will_include_bash(
    agent: Option<&crate::AgentProfile>,
    tool_policy: Option<&crate::ToolPolicy>,
) -> bool {
    agent.is_none_or(|agent| agent.allows_tool("bash"))
        && tool_policy.is_none_or(|policy| policy.allows("bash"))
}

pub(super) fn canonicalize_tools(tools: &mut Vec<ToolDefinition>) {
    for tool in tools.iter_mut() {
        canonicalize_json(&mut tool.input_schema);
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
}

fn canonicalize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (_, value) in &mut entries {
                canonicalize_json(value);
            }
            object.extend(entries);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                canonicalize_json(value);
            }
        }
        _ => {}
    }
}

pub(super) const fn delegation_policy_prompt(policy: crate::DelegationPolicy) -> &'static str {
    match policy {
        crate::DelegationPolicy::Off => crate::prompts::DELEGATION_COORDINATION_OFF,
        crate::DelegationPolicy::OnDemand => crate::prompts::DELEGATION_COORDINATION_ON_DEMAND,
        crate::DelegationPolicy::Complex => crate::prompts::DELEGATION_COORDINATION_COMPLEX,
        crate::DelegationPolicy::Aggressive => crate::prompts::DELEGATION_COORDINATION_AGGRESSIVE,
        crate::DelegationPolicy::Always => crate::prompts::DELEGATION_COORDINATION_ALWAYS,
    }
}

pub(super) fn mode_context(mode: &crate::ModeProfile) -> String {
    let mut content = format!(
        "<cagent:collaboration-mode name=\"{}\">\n{}",
        mode.name, mode.prompt
    );
    if mode.plan {
        content.push_str("\n");
        content.push_str(crate::prompts::PROPOSED_PLAN_CONTRACT);
    }
    content.push_str("\n</cagent:collaboration-mode>");
    content
}

fn bash_tool_description(inventory: &crate::ShellCommandInventory) -> String {
    let guidance = crate::safe_bash_guidance(inventory, 0);
    format!(
        "Run a Bash command as a supervised terminal. Set wait to true to return its completion, or false to return immediately with a joinable terminal ID. Run unrelated commands as separate Bash tool calls in the same response so they can execute in parallel; combine commands only when they are dependent. Use apply_patch for project-file changes. Proven read-safe foreground commands with inherited environment run without command approval. {guidance}"
    )
}

#[allow(clippy::too_many_lines)]
pub(super) fn builtin_tool_definitions(
    shell_inventory: &crate::ShellCommandInventory,
) -> Vec<ToolDefinition> {
    builtin_tool_definitions_with_agents(shell_inventory, None)
}

#[allow(clippy::too_many_lines)]
pub(super) fn builtin_tool_definitions_with_agents(
    shell_inventory: &crate::ShellCommandInventory,
    agents: Option<&crate::AgentCatalog>,
) -> Vec<ToolDefinition> {
    let mut tools = system_tool_definitions(shell_inventory, agents);
    tools.extend(agent_control_tool_definitions(shell_inventory, agents));
    tools
}

/// System-facing native tools: mutations and supervised Bash terminals.
pub(super) fn system_tool_definitions(
    shell_inventory: &crate::ShellCommandInventory,
    agents: Option<&crate::AgentCatalog>,
) -> Vec<ToolDefinition> {
    native_tool_definitions(shell_inventory, agents)
        .into_iter()
        .filter(|tool| {
            matches!(
                tool.name.as_str(),
                "apply_patch"
                    | "bash"
                    | "terminal_output"
                    | "terminal_write"
                    | "terminal_kill"
                    | "change_working_directory"
                    | "enter_worktree"
            )
        })
        .collect()
}

/// Agent-control native tools: delegation, joining, user questions, and plans.
pub(super) fn agent_control_tool_definitions(
    shell_inventory: &crate::ShellCommandInventory,
    agents: Option<&crate::AgentCatalog>,
) -> Vec<ToolDefinition> {
    native_tool_definitions(shell_inventory, agents)
        .into_iter()
        .filter(|tool| {
            matches!(
                tool.name.as_str(),
                "delegate_agent" | "wait_join" | REQUEST_USER_INPUT_TOOL | UPDATE_PLAN_TOOL
            )
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn native_tool_definitions(
    shell_inventory: &crate::ShellCommandInventory,
    agents: Option<&crate::AgentCatalog>,
) -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition {
            name: "change_working_directory".into(),
            description: "Change this conversation's working directory. Relative paths resolve from the current working directory. Use path \".\" to return to the directory where this conversation was launched. That launch directory remains the project used to group conversation history and decide whether a target is external; changing to an external path requires approval.".into(),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string", "minLength": 1 } },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "enter_worktree".into(),
            description: "Create or enter an isolated Git worktree or Jujutsu workspace and make it this conversation's working directory. Call this tool directly; no VCS preflight is needed. Supply either name or a custom path, not both. Omit both to generate a name. An omitted base uses the configured default (`fresh` by default). `fresh` uses the repository's default branch, `head` uses the current revision, and other values may be Git or Jujutsu references such as a branch, commit ID, change ID, or revset. Use path \".\" to return to the directory where this conversation was launched.".into(),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": ["string", "null"] },
                    "path": { "type": ["string", "null"] },
                    "base": { "type": ["string", "null"] }
                },
                "required": ["name", "path", "base"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "apply_patch".into(),
            description: "Use the `apply_patch` tool to edit project files. Put the patch text in the `patch` field. The patch language is a stripped-down, file-oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high-level envelope:\n\n*** Begin Patch\n[ one or more file sections ]\n*** End Patch\n\nEmit exactly one patch envelope. Within that envelope, you get a sequence of one or multiple file operations. You MUST include a header to specify the action you are taking. Each operation starts with one of three headers:\n\n*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).\n*** Delete File: <path> - remove an existing file. Nothing follows.\n*** Update File: <path> - patch an existing file in place (optionally with a rename).\n\nExample patch:\n\n*** Begin Patch\n*** Add File: hello.txt\n+Hello world\n*** Update File: src/app.py\n*** Move to: src/main.py\n@@ def greet():\n-print(\"Hi\")\n+print(\"Hello, world!\")\n*** Delete File: obsolete.txt\n*** End Patch\n\nIt is important to remember:\n\n- You must include a header with your intended action (Add/Delete/Update).\n- You must prefix new lines with `+`, even when creating a new file.\n- When ordinary context is ambiguous and you know the exact or approximate line, you may use `@321@ def greet():` instead of `@@ def greet():`. This is an optional best-effort hint: it tries the indicated line, then a few following lines, then backwards, and falls back to normal context matching. Prefer ordinary `@@` context whenever it identifies the target unambiguously.\n- Put the complete patch, including its envelope, in the `patch` field.".into(),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "The full patch text that describes all changes to be made." }
                },
                "required": ["patch"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "bash".into(),
            description: bash_tool_description(shell_inventory),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "minLength": 1, "description": "Shell command to execute" },
                    "cwd": { "type": ["string", "null"], "description": "Optional working directory. Uses the current directory by default; use this instead of cd" },
                    "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Additional environment variables for the command" },
                    "forward_env": { "type": "array", "items": { "type": "string" }, "description": "Names of environment variables to forward to the command" },
                    "timeout": { "type": ["integer", "null"], "minimum": 1, "maximum": 600, "default": null, "description": "Optional timeout in seconds. Use null for the configured default timeout" },
                    "wait": { "type": "boolean", "description": "Wait for terminal completion before continuing" }
                },
                "required": ["command", "cwd", "env", "forward_env", "timeout", "wait"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "terminal_output".into(),
            description: "Read new output from a supervised terminal. Start with cursor 0 or null, then continue with the returned cursor.".into(),
            asynchronous: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Background terminal ID" }, "cursor": { "type": ["integer", "null"], "minimum": 0, "description": "Output cursor, starting at 0 or null" } },
                "required": ["id", "cursor"], "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "terminal_write".into(),
            description: "Send text or control sequences to a supervised terminal. Use this only for an existing terminal that needs interactive input.".into(),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Background terminal ID" }, "data": { "type": "string", "description": "Text or control sequence to write to the terminal" } },
                "required": ["id", "data"], "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "terminal_kill".into(),
            description: "Gracefully terminate a supervised terminal, or force termination when graceful shutdown is insufficient.".into(),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Background terminal ID" }, "force": { "type": "boolean", "description": "Force termination instead of graceful shutdown" } },
                "required": ["id", "force"], "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "delegate_agent".into(),
            description: delegate_agent_description(agents),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "minLength": 1, "description": "Concrete bounded task for the sub-agent" },
                    "agent": { "type": "string", "description": "Configured delegated-agent profile name" },
                    "provider": { "type": ["string", "null"], "description": "Optional provider override" },
                    "model": { "type": ["string", "null"], "description": "Optional model override" },
                    "effort": { "type": ["string", "null"], "minLength": 1, "description": "Optional reasoning effort override for this run; null preserves the default effort" },
                    "wait": { "type": "boolean", "description": "Wait for the terminal result before continuing" }
                },
                "required": ["task", "agent", "provider", "model", "effort", "wait"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "wait_join".into(),
            description: "Wait concurrently for one or more terminal or delegated-agent IDs and return typed completion envelopes in the supplied order.".into(),
            asynchronous: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "ids": { "type": "array", "minItems": 1, "items": { "type": "string" } } },
                "required": ["ids"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: REQUEST_USER_INPUT_TOOL.into(),
            description: "Request user input through one to three short multiple-choice questions and wait for the response. Use this when you need clarification, are choosing between approaches, or the user's preference would materially change the work. Do not use it for information you can determine by inspecting the codebase. Prefer one question; provide 2-3 mutually exclusive choices, put the recommended option first, and suffix its label with \"(Recommended)\" when there is a recommended option. Do not include an Other option; the client will add a free-form Other option automatically.".into(),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array", "minItems": 1, "maxItems": 3,
                        "description": "One to three questions to show the user",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "minLength": 1, "description": "Stable question identifier" },
                                "header": { "type": "string", "minLength": 1, "description": "Short question header" },
                                "question": { "type": "string", "minLength": 1, "description": "Question text" },
                                "options": {
                                    "type": "array", "minItems": 2, "maxItems": 3, "description": "Provide 2-3 mutually exclusive choices. Put the recommended option first and suffix its label with (Recommended) when there is a recommended option. Do not include an Other option; the client adds one automatically.",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string", "minLength": 1, "description": "Short option label" },
                                            "description": { "type": "string", "minLength": 1, "description": "Option tradeoff or impact" }
                                        },
                                        "required": ["label", "description"],
                                        "additionalProperties": false
                                    }
                                }
                            },
                            "required": ["id", "header", "question", "options"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["questions"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: UPDATE_PLAN_TOOL.into(),
            description: "Updates the task plan. Provide an optional explanation and the complete ordered list of plan items, each with a step and status. At most one step can be in_progress at a time. Each call replaces the previous plan snapshot.".into(),
            asynchronous: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "explanation": { "type": ["string", "null"] },
                    "plan": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["step", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["explanation", "plan"],
                "additionalProperties": false
            }),
        },
    ];
    tools
        .iter_mut()
        .find(|tool| tool.name == "apply_patch")
        .expect("native apply_patch tool")
        .description
        .push_str(" Do not include no-op updates.");
    tools
}

pub(super) fn web_search_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".into(),
        description:
            "Search the configured web provider and return bounded, untrusted reference results."
                .into(),
        asynchronous: true,
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1, "maxLength": 1024, "description": "Search query" }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

pub(super) fn web_fetch_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "web_fetch".into(),
        description: "Fetch an HTTP(S) URL and return bounded, untrusted normalized content. Redirect destinations require approval unless they match the configured safe redirect policy.".into(),
        asynchronous: true,
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "minLength": 1, "description": "HTTP(S) URL to fetch" },
                "format": { "type": "string", "enum": ["markdown", "text", "html"], "default": "markdown", "description": "Desired response format" },
                "timeout": { "type": "integer", "minimum": 1, "maximum": 120, "default": 30, "description": "Request timeout in seconds" }
            },
            "required": ["url", "format", "timeout"],
            "additionalProperties": false
        }),
    }
}

pub(super) fn delegated_tool_definitions(
    profile: &crate::AgentProfile,
    shell_inventory: &crate::ShellCommandInventory,
) -> Vec<ToolDefinition> {
    builtin_tool_definitions(shell_inventory)
        .into_iter()
        .filter(|tool| {
            !matches!(tool.name.as_str(), "delegate_agent" | "wait_join")
                && tool.name != UPDATE_PLAN_TOOL
                && !matches!(tool.name.as_str(), "web_search" | "web_fetch")
                && profile.allows_tool(&tool.name)
        })
        .collect()
}

fn delegate_agent_description(agents: Option<&crate::AgentCatalog>) -> String {
    let mut description = "Delegate a bounded, self-contained supporting task to an isolated sub-agent. Do not delegate the primary implementation unless the user requests it or multiple independent workstreams can run in parallel. Choose an appropriate available sub-agent. Use wait=true by default and use the completed result before continuing. Use wait=false only for intentional background work. Separate delegations in the same response run concurrently. While the sub-agent runs, do not repeat any part of its investigation, commands, or edits in the parent agent; only perform clearly separate parent work.".to_owned();
    description.push_str("\n\nAvailable delegated agents:");
    match agents {
        Some(agents) => {
            for (name, profile) in agents.subagent_profiles() {
                description.push_str(&format!("\n- {name}: {}", profile.description));
            }
        }
        None => {
            description.push_str("\n- No delegated-agent profiles are available in this request.")
        }
    }
    description
}

pub(super) fn delegated_tool_definitions_with_web_search(
    profile: &crate::AgentProfile,
    web_search: Option<&crate::WebSearchConfig>,
    web_search_available: bool,
    shell_inventory: &crate::ShellCommandInventory,
) -> Vec<ToolDefinition> {
    let mut tools = delegated_tool_definitions(profile, shell_inventory);
    if profile.name != "explore"
        && web_search.is_some_and(|config| {
            web_search_available
                && (config.resolve().is_some()
                    || config.provider() == Some(crate::WebSearchProvider::Chatgpt))
        })
        && profile.allows_tool("web_search")
    {
        tools.push(web_search_tool_definition());
    }
    if profile.allows_tool("web_fetch") {
        tools.push(web_fetch_tool_definition());
    }
    tools
}
