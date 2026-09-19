use super::*;

pub(super) fn mcp_definition_template(transport: usize) -> cagent_agent::mcp::McpServerDefinition {
    let transport = if transport == 1 {
        cagent_agent::mcp::McpTransportConfig::Stdio {
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            env_remove: Vec::new(),
            inherit_env: true,
        }
    } else {
        cagent_agent::mcp::McpTransportConfig::StreamableHttp {
            url: "https://".into(),
            headers: BTreeMap::new(),
            allow_insecure: false,
        }
    };
    cagent_agent::mcp::McpServerDefinition {
        transport,
        enabled: true,
        agents: Vec::new(),
        eager: false,
        startup_timeout_seconds: 10,
        request_timeout_seconds: 60,
        read_only_tools: Vec::new(),
        ..cagent_agent::mcp::McpServerDefinition::default()
    }
}

pub(super) const fn mcp_form_maximum(definition: &cagent_agent::mcp::McpServerDefinition) -> usize {
    match definition.transport {
        cagent_agent::mcp::McpTransportConfig::Stdio { .. } => 14,
        cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. } => 11,
    }
}

pub(super) fn mcp_form_field(
    definition: &cagent_agent::mcp::McpServerDefinition,
    selected: usize,
) -> Option<McpFormField> {
    match selected {
        2 => Some(McpFormField::Name),
        5 => Some(McpFormField::Agents),
        6 => Some(McpFormField::StartupTimeout),
        7 => Some(McpFormField::RequestTimeout),
        8 => Some(McpFormField::ReadOnlyTools),
        9 => Some(match definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { .. } => McpFormField::Command,
            cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. } => McpFormField::Url,
        }),
        10 => Some(match definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { .. } => McpFormField::Arguments,
            cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. } => McpFormField::Headers,
        }),
        11 if matches!(
            definition.transport,
            cagent_agent::mcp::McpTransportConfig::Stdio { .. }
        ) =>
        {
            Some(McpFormField::Cwd)
        }
        12 => Some(McpFormField::Environment),
        13 => Some(McpFormField::RemovedEnvironment),
        _ => None,
    }
}

pub(super) const fn mcp_field_uses_json(field: McpFormField) -> bool {
    matches!(
        field,
        McpFormField::Agents
            | McpFormField::ReadOnlyTools
            | McpFormField::Arguments
            | McpFormField::Environment
            | McpFormField::RemovedEnvironment
            | McpFormField::Headers
    )
}

pub(super) const fn is_mcp_form_boolean(
    definition: &cagent_agent::mcp::McpServerDefinition,
    selected: usize,
) -> bool {
    match definition.transport {
        cagent_agent::mcp::McpTransportConfig::Stdio { .. } => selected == 14,
        cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. } => selected == 11,
    }
}

pub(super) fn toggle_mcp_form_boolean(
    definition: &mut cagent_agent::mcp::McpServerDefinition,
    selected: usize,
) {
    match &mut definition.transport {
        cagent_agent::mcp::McpTransportConfig::Stdio { inherit_env, .. } if selected == 14 => {
            *inherit_env = !*inherit_env
        }
        cagent_agent::mcp::McpTransportConfig::StreamableHttp { allow_insecure, .. }
            if selected == 11 =>
        {
            *allow_insecure = !*allow_insecure
        }
        _ => {}
    }
}

pub(super) fn next_mcp_location(
    location: &cagent_agent::mcp::McpLocation,
    active_agent: String,
) -> cagent_agent::mcp::McpLocation {
    match location.scope {
        cagent_agent::mcp::McpScope::Global => cagent_agent::mcp::McpLocation::project(),
        cagent_agent::mcp::McpScope::Project => cagent_agent::mcp::McpLocation::agent(active_agent),
        cagent_agent::mcp::McpScope::Agent => cagent_agent::mcp::McpLocation::global(),
    }
}

fn json_value(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

pub(super) fn mcp_form_field_value(draft: &McpFormDraft, field: McpFormField) -> String {
    match field {
        McpFormField::Name => draft.name.clone(),
        McpFormField::Agents => json_value(&draft.definition.agents),
        McpFormField::StartupTimeout => draft.definition.startup_timeout_seconds.to_string(),
        McpFormField::RequestTimeout => draft.definition.request_timeout_seconds.to_string(),
        McpFormField::ReadOnlyTools => json_value(&draft.definition.read_only_tools),
        McpFormField::Command => match &draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { command, .. } => command.clone(),
            _ => String::new(),
        },
        McpFormField::Arguments => match &draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { args, .. } => json_value(args),
            _ => String::new(),
        },
        McpFormField::Cwd => match &draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { cwd, .. } => {
                cwd.clone().unwrap_or_default()
            }
            _ => String::new(),
        },
        McpFormField::Environment => match &draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { env, .. } => json_value(env),
            _ => String::new(),
        },
        McpFormField::RemovedEnvironment => match &draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { env_remove, .. } => {
                json_value(env_remove)
            }
            _ => String::new(),
        },
        McpFormField::Url => match &draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::StreamableHttp { url, .. } => url.clone(),
            _ => String::new(),
        },
        McpFormField::Headers => match &draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::StreamableHttp { headers, .. } => {
                json_value(headers)
            }
            _ => String::new(),
        },
    }
}

pub(super) fn apply_mcp_form_field(
    draft: &mut McpFormDraft,
    field: McpFormField,
    value: &str,
) -> Result<(), String> {
    match field {
        McpFormField::Name => value.trim().clone_into(&mut draft.name),
        McpFormField::Agents => {
            draft.definition.agents = parse_json_field(value, "array of assigned agents")?
        }
        McpFormField::StartupTimeout => {
            draft.definition.startup_timeout_seconds = value
                .trim()
                .parse()
                .map_err(|_| "startup timeout must be a positive integer".to_owned())?
        }
        McpFormField::RequestTimeout => {
            draft.definition.request_timeout_seconds = value
                .trim()
                .parse()
                .map_err(|_| "request timeout must be a positive integer".to_owned())?
        }
        McpFormField::ReadOnlyTools => {
            draft.definition.read_only_tools = parse_json_field(value, "array of tool names")?
        }
        McpFormField::Command => match &mut draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { command, .. } => {
                value.clone_into(command)
            }
            _ => return Err("command is only valid for stdio".into()),
        },
        McpFormField::Arguments => match &mut draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { args, .. } => {
                *args = parse_json_field(value, "array of arguments")?
            }
            _ => return Err("arguments are only valid for stdio".into()),
        },
        McpFormField::Cwd => match &mut draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { cwd, .. } => {
                *cwd = (!value.trim().is_empty()).then(|| value.to_owned())
            }
            _ => return Err("working directory is only valid for stdio".into()),
        },
        McpFormField::Environment => match &mut draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { env, .. } => {
                *env = parse_json_field(value, "object of environment variables")?
            }
            _ => return Err("environment is only valid for stdio".into()),
        },
        McpFormField::RemovedEnvironment => match &mut draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::Stdio { env_remove, .. } => {
                *env_remove = parse_json_field(value, "array of variable names")?
            }
            _ => return Err("removed variables are only valid for stdio".into()),
        },
        McpFormField::Url => match &mut draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::StreamableHttp { url, .. } => {
                value.clone_into(url)
            }
            _ => return Err("URL is only valid for HTTP".into()),
        },
        McpFormField::Headers => match &mut draft.definition.transport {
            cagent_agent::mcp::McpTransportConfig::StreamableHttp { headers, .. } => {
                *headers = parse_json_field(value, "object of HTTP headers")?
            }
            _ => return Err("headers are only valid for HTTP".into()),
        },
    }
    Ok(())
}

fn parse_json_field<T: serde::de::DeserializeOwned>(
    value: &str,
    expected: &str,
) -> Result<T, String> {
    simd_json::serde::from_slice(&mut value.as_bytes().to_vec())
        .map_err(|error| format!("expected {expected}: {error}"))
}

pub(super) const fn mcp_form_field_index(field: McpFormField) -> usize {
    match field {
        McpFormField::Name => 2,
        McpFormField::Agents => 5,
        McpFormField::StartupTimeout => 6,
        McpFormField::RequestTimeout => 7,
        McpFormField::ReadOnlyTools => 8,
        McpFormField::Command | McpFormField::Url => 9,
        McpFormField::Arguments | McpFormField::Headers => 10,
        McpFormField::Cwd => 11,
        McpFormField::Environment => 12,
        McpFormField::RemovedEnvironment => 13,
    }
}

pub(super) fn mcp_form_mutations(draft: &McpFormDraft) -> Vec<cagent_agent::mcp::McpMutation> {
    let put = cagent_agent::mcp::McpMutation::Put {
        location: draft.location.clone(),
        name: draft.name.clone(),
        definition: draft.definition.clone(),
    };
    let Some((original_location, original_name)) = &draft.original else {
        return vec![put];
    };
    if original_location == &draft.location && original_name == &draft.name {
        return vec![put];
    }
    vec![
        cagent_agent::mcp::McpMutation::Move {
            from: original_location.clone(),
            name: original_name.clone(),
            to: draft.location.clone(),
            new_name: draft.name.clone(),
        },
        put,
    ]
}
