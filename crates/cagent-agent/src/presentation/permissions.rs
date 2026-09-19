//! Frontend-neutral permission approval choices.

use crate::{
    InteractionRequest, InteractionRequestKind, PermissionAudit, PermissionEffect,
    PermissionResource, PermissionScope,
};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionApprovalChoice {
    pub decision: &'static str,
    pub label: String,
    pub detail: String,
    pub prepared_rule: Option<crate::PermissionRule>,
    /// Rule used when the request/conversation row has conversation scope.
    pub session_rule: Option<crate::PermissionRule>,
}

impl PermissionApprovalChoice {
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.prepared_rule.is_some()
    }

    #[must_use]
    pub fn supports_session(&self) -> bool {
        self.session_rule.is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionDenialSummary {
    pub resource: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionDecisionExplanation {
    pub operation: String,
    pub external: Option<String>,
    pub result: String,
}

/// Describes the independent operation and workspace-boundary decisions.
#[must_use]
pub fn permission_decision_explanation(
    decision: &crate::FilesystemPermissionDecision,
) -> PermissionDecisionExplanation {
    let describe = |label: &str, value: &crate::PermissionDecision| {
        format!(
            "{label}  {} · {}",
            format!("{:?}", value.effect).to_lowercase(),
            value.reason
        )
    };
    PermissionDecisionExplanation {
        operation: describe("Operation", &decision.operation),
        external: decision
            .external
            .as_ref()
            .map(|value| describe("Boundary ", value)),
        result: format!(
            "Result     {}",
            format!("{:?}", decision.effect).to_lowercase()
        ),
    }
}

/// Returns the human-facing operation label used by permission surfaces.
#[must_use]
pub fn permission_tool_label(tool: &str) -> String {
    match tool {
        "apply_patch" => "edit".into(),
        "grep" => "search".into(),
        "web_fetch" => "web fetch".into(),
        _ => tool.into(),
    }
}

#[must_use]
pub fn uses_file_permission_choices(request: &InteractionRequest) -> bool {
    matches!(
        &request.kind,
        InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule: Some(rule),
            ..
        } if matches!(resource.tool.as_str(), "read" | "grep")
            && resource.path.is_some()
            && rule.tool.as_deref() == Some(resource.tool.as_str())
    )
}

#[must_use]
pub fn uses_list_permission_choices(request: &InteractionRequest) -> bool {
    matches!(
        &request.kind,
        InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule: Some(rule),
            ..
        } if resource.tool == "list"
            && resource.path.is_some()
            && rule.tool.as_deref() == Some("list")
    )
}

#[must_use]
pub fn default_permission_approval_scope(request: &InteractionRequest) -> PermissionScope {
    if uses_conversation_first_choices(request) {
        PermissionScope::ConversationGlobal
    } else {
        PermissionScope::Project
    }
}

fn uses_conversation_first_choices(request: &InteractionRequest) -> bool {
    permission_resource(request).is_some_and(|resource| {
        matches!(resource.tool.as_str(), "bash" | "web_fetch" | "web_search")
    })
}

#[must_use]
pub fn permission_approval_choices(
    request: &InteractionRequest,
    scope: PermissionScope,
) -> Vec<PermissionApprovalChoice> {
    permission_approval_choices_with_session(
        request,
        match scope {
            PermissionScope::Conversation => PermissionScope::Project,
            PermissionScope::ConversationGlobal => PermissionScope::Global,
            scope => scope,
        },
        matches!(
            scope,
            PermissionScope::Conversation | PermissionScope::ConversationGlobal
        ),
    )
}

#[must_use]
pub fn permission_approval_choices_with_session(
    request: &InteractionRequest,
    scope: PermissionScope,
    for_session: bool,
) -> Vec<PermissionApprovalChoice> {
    let request_decision = if for_session {
        "allow_session"
    } else {
        "allow_once"
    };
    let request_detail = if for_session {
        "for this conversation"
    } else {
        "for this request"
    };
    let persistent_decision = match scope {
        PermissionScope::Conversation | PermissionScope::ConversationGlobal => "allow_session",
        PermissionScope::Project => "allow_project",
        PermissionScope::Global => "allow_global",
    };
    let scope_detail = match scope {
        PermissionScope::Conversation | PermissionScope::ConversationGlobal => {
            "for this conversation"
        }
        PermissionScope::Project => "for this project",
        PermissionScope::Global => "globally",
    };

    if uses_list_permission_choices(request) {
        let rule = suggested_rule(request).cloned().map(prepare_exact_rule);
        return vec![
            PermissionApprovalChoice {
                decision: request_decision,
                label: "Allow".into(),
                detail: request_detail.into(),
                prepared_rule: None,
                session_rule: rule.clone(),
            },
            PermissionApprovalChoice {
                decision: persistent_decision,
                label: "Always allow".into(),
                detail: format!("this directory {scope_detail}"),
                prepared_rule: rule,
                session_rule: None,
            },
            PermissionApprovalChoice {
                decision: "deny",
                label: "Deny".into(),
                detail: "reject this request".into(),
                prepared_rule: None,
                session_rule: None,
            },
        ];
    }

    if uses_file_permission_choices(request) {
        let containing = suggested_rule(request).and_then(containing_directory_rule);
        let exact = suggested_rule(request).cloned().map(prepare_exact_rule);
        return vec![
            PermissionApprovalChoice {
                decision: request_decision,
                label: "Allow".into(),
                detail: request_detail.into(),
                prepared_rule: None,
                session_rule: exact.clone(),
            },
            PermissionApprovalChoice {
                decision: persistent_decision,
                label: "Always allow containing dir".into(),
                detail: scope_detail.into(),
                prepared_rule: containing,
                session_rule: None,
            },
            PermissionApprovalChoice {
                decision: persistent_decision,
                label: "Always allow file".into(),
                detail: scope_detail.into(),
                prepared_rule: exact,
                session_rule: None,
            },
            PermissionApprovalChoice {
                decision: "deny",
                label: "Deny".into(),
                detail: "reject this request".into(),
                prepared_rule: None,
                session_rule: None,
            },
        ];
    }

    if uses_bash_read_permission_choice(request) {
        let rule = suggested_rule(request).cloned().map(prepare_bash_read_rule);
        let display_path = bash_read_permission_path(request).unwrap_or_else(|| "path".into());
        return vec![
            PermissionApprovalChoice {
                decision: request_decision,
                label: "Allow reading".into(),
                detail: request_detail.into(),
                prepared_rule: None,
                session_rule: rule.clone(),
            },
            PermissionApprovalChoice {
                decision: persistent_decision,
                label: format!("Always allow reading {display_path}"),
                detail: scope_detail.into(),
                prepared_rule: rule,
                session_rule: None,
            },
            PermissionApprovalChoice {
                decision: "deny",
                label: "Deny".into(),
                detail: "reject this request".into(),
                prepared_rule: None,
                session_rule: None,
            },
        ];
    }

    vec![
        PermissionApprovalChoice {
            decision: request_decision,
            label: "Allow".into(),
            detail: if uses_conversation_first_choices(request) && !for_session {
                "once"
            } else {
                request_detail
            }
            .into(),
            prepared_rule: None,
            session_rule: broad_web_session_rule(request)
                .or_else(|| suggested_rule(request).cloned()),
        },
        PermissionApprovalChoice {
            decision: persistent_decision,
            label: if uses_conversation_first_choices(request) {
                "Allow"
            } else {
                "Always allow"
            }
            .into(),
            detail: scope_detail.into(),
            prepared_rule: suggested_rule(request).cloned(),
            session_rule: None,
        },
        PermissionApprovalChoice {
            decision: "deny",
            label: "Deny".into(),
            detail: "reject this request".into(),
            prepared_rule: None,
            session_rule: None,
        },
    ]
}

fn broad_web_session_rule(request: &InteractionRequest) -> Option<crate::PermissionRule> {
    let resource = permission_resource(request)?;
    matches!(resource.tool.as_str(), "web_fetch" | "web_search").then_some(())?;
    let mut rule = suggested_rule(request)?.clone();
    rule.tool = Some(resource.tool.clone());
    rule.server = None;
    rule.path = None;
    rule.command = None;
    rule.raw_command = None;
    rule.cwd = None;
    rule.mode = None;
    rule.agent = None;
    Some(rule)
}

fn permission_resource(request: &InteractionRequest) -> Option<&PermissionResource> {
    let InteractionRequestKind::PermissionApproval { resource, .. } = &request.kind else {
        return None;
    };
    Some(resource)
}

fn suggested_rule(request: &InteractionRequest) -> Option<&crate::PermissionRule> {
    let InteractionRequestKind::PermissionApproval { suggested_rule, .. } = &request.kind else {
        return None;
    };
    suggested_rule.as_ref()
}

fn uses_bash_read_permission_choice(request: &InteractionRequest) -> bool {
    matches!(
        &request.kind,
        InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule: Some(rule),
            ..
        } if resource.tool == "bash"
            && resource.access == Some(crate::PermissionAccess::Read)
            && resource.path.is_some()
            && rule.tool.as_deref() == Some("bash")
    )
}

/// Returns the display-safe path for a specialized read-only Bash approval.
///
/// The persisted rule retains its canonical absolute path; home-directory
/// targets are abbreviated only for presentation.
#[must_use]
pub fn bash_read_permission_path(request: &InteractionRequest) -> Option<String> {
    uses_bash_read_permission_choice(request)
        .then(|| {
            permission_resource(request)?
                .path
                .as_deref()
                .map(display_path)
        })
        .flatten()
}

fn containing_directory_rule(rule: &crate::PermissionRule) -> Option<crate::PermissionRule> {
    let path = Path::new(rule.path.as_deref()?);
    let parent = path.parent()?;
    let mut rule = rule.clone();
    rule.path = Some(recursive_path_pattern(parent));
    Some(rule)
}

fn prepare_bash_read_rule(mut rule: crate::PermissionRule) -> crate::PermissionRule {
    if let Some(path) = rule.path.as_deref() {
        rule.path = Some(if Path::new(path).is_dir() {
            recursive_path_pattern(Path::new(path))
        } else {
            literal_path_pattern(Path::new(path))
        });
    }
    rule
}

fn prepare_exact_rule(mut rule: crate::PermissionRule) -> crate::PermissionRule {
    if let Some(path) = rule.path.as_deref() {
        rule.path = Some(literal_path_pattern(Path::new(path)));
    }
    rule
}

fn recursive_path_pattern(path: &Path) -> String {
    let path = literal_path_pattern(path);
    if path == "/" {
        "/**".into()
    } else {
        format!("{}/**", path.trim_end_matches('/'))
    }
}

fn literal_path_pattern(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('*', "\\*")
        .replace('?', "\\?")
}

fn display_path(path: &str) -> String {
    let path = Path::new(path);
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from);
    if let Some(relative) = home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        if relative.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", relative.to_string_lossy().replace('\\', "/"));
    }
    path.to_string_lossy().replace('\\', "/")
}

/// Formats a denied authorization decision for a frontend transcript.
///
/// The summary keeps the resource and the most relevant policy explanation
/// together while leaving terminal-specific styling and wrapping to the
/// frontend.
#[must_use]
pub fn permission_denial_summary(
    audit: &PermissionAudit,
    workspace: &Path,
) -> Option<PermissionDenialSummary> {
    if audit.outcome != PermissionEffect::Deny {
        return None;
    }

    let resource = permission_resource_summary(&audit.resource, workspace);
    let reason = denied_reason(audit).unwrap_or("denied by user");
    Some(PermissionDenialSummary {
        resource,
        reason: reason.into(),
    })
}

fn denied_reason(audit: &PermissionAudit) -> Option<&str> {
    if let Some(reason) = audit.user_reason.as_deref() {
        return Some(reason);
    }
    if audit.decision.operation.effect == PermissionEffect::Deny {
        return Some(&audit.decision.operation.reason);
    }
    audit
        .decision
        .external
        .as_ref()
        .filter(|decision| decision.effect == PermissionEffect::Deny)
        .map(|decision| decision.reason.as_str())
}

fn permission_resource_summary(resource: &PermissionResource, workspace: &Path) -> String {
    let name = if resource.tool == "mcp" {
        match (&resource.server, &resource.operation) {
            (Some(server), Some(operation)) => format!("mcp {server}/{operation}"),
            (Some(server), None) => format!("mcp {server}"),
            (None, Some(operation)) => format!("mcp {operation}"),
            (None, None) => resource.tool.clone(),
        }
    } else {
        permission_tool_label(&resource.tool)
    };

    if let Some(command) = resource
        .raw_command
        .as_deref()
        .filter(|command| !command.is_empty())
    {
        return format!("{name} {command}");
    }
    if !resource.command.is_empty() {
        return format!("{name} {}", resource.command.join(" "));
    }
    if let Some(path) = resource.path.as_deref().filter(|path| !path.is_empty()) {
        let path = Path::new(path)
            .strip_prefix(workspace)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map_or_else(
                || path.to_owned(),
                |relative| relative.to_string_lossy().replace('\\', "/"),
            );
        return format!("{name} {path}");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FilesystemPermissionDecision, PermissionAccess, PermissionDecision, PermissionEffect,
        PermissionLayerKind, PermissionResource, PermissionRule,
    };

    fn request(tool: &str) -> InteractionRequest {
        let operation = PermissionDecision {
            effect: PermissionEffect::Ask,
            layer: PermissionLayerKind::Default,
            rule_id: None,
            reason: "test".into(),
        };
        InteractionRequest {
            id: crate::InteractionRequestId::new(),
            origin: None,
            kind: InteractionRequestKind::PermissionApproval {
                resource: PermissionResource {
                    tool: tool.into(),
                    server: None,
                    operation: None,
                    path: Some("/tmp/project/file.txt".into()),
                    access: Some(PermissionAccess::Read),
                    mode: "ask".into(),
                    agent: "general".into(),
                    command: Vec::new(),
                    raw_command: None,
                    cwd: None,
                },
                decision: FilesystemPermissionDecision {
                    effect: PermissionEffect::Ask,
                    operation,
                    external: None,
                },
                message: "test".into(),
                queued_message_id: None,
                preview: None,
                arguments: None,
                auto_review: None,
                suggested_rule: Some(PermissionRule {
                    id: String::new(),
                    effect: PermissionEffect::Allow,
                    tool: Some(tool.into()),
                    server: None,
                    operation: None,
                    path: Some("/tmp/project/file.txt".into()),
                    command: None,
                    raw_command: None,
                    cwd: None,
                    access: Some("read".into()),
                    external: false,
                    mode: None,
                    agent: None,
                    source: None,
                    created_at: None,
                }),
            },
        }
    }

    fn path_string(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn file_tools_use_path_scoped_persistent_choices() {
        for tool in ["read", "grep"] {
            let choices = permission_approval_choices(&request(tool), PermissionScope::Project);
            assert_eq!(
                choices
                    .iter()
                    .map(|choice| choice.decision)
                    .collect::<Vec<_>>(),
                ["allow_once", "allow_project", "allow_project", "deny"]
            );
            assert_eq!(
                choices[1].prepared_rule.as_ref().unwrap().path.as_deref(),
                Some("/tmp/project/**")
            );
            assert_eq!(
                choices[2].prepared_rule.as_ref().unwrap().path.as_deref(),
                Some("/tmp/project/file.txt")
            );
        }
    }

    #[test]
    fn list_uses_exact_directory_persistence() {
        let choices = permission_approval_choices(&request("list"), PermissionScope::Project);
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.decision)
                .collect::<Vec<_>>(),
            ["allow_once", "allow_project", "deny"]
        );
        assert_eq!(
            choices[1].prepared_rule.as_ref().unwrap().path.as_deref(),
            Some("/tmp/project/file.txt")
        );
    }

    #[test]
    fn bash_and_web_default_to_conversation_and_global() {
        for tool in ["bash", "web_fetch", "web_search"] {
            let mut request = request(tool);
            if let InteractionRequestKind::PermissionApproval {
                resource,
                suggested_rule,
                ..
            } = &mut request.kind
            {
                resource.path = None;
                suggested_rule.as_mut().unwrap().path = None;
            }
            let scope = default_permission_approval_scope(&request);
            assert_eq!(scope, PermissionScope::ConversationGlobal);
            let choices = permission_approval_choices(&request, scope);
            assert_eq!(choices.len(), 3);
            assert_eq!(choices[0].decision, "allow_session");
            assert_eq!(choices[0].detail, "for this conversation");
            assert!(choices[0].supports_session());
            assert_eq!(choices[1].decision, "allow_global");
            assert_eq!(choices[1].detail, "globally");
            assert_eq!(choices[2].decision, "deny");

            let once = permission_approval_choices(&request, PermissionScope::Global);
            assert_eq!(once[0].decision, "allow_once");
            assert_eq!(once[0].detail, "once");
            assert_eq!(once[1], choices[1]);
            if tool == "bash" {
                assert_eq!(choices[0].session_rule.as_ref(), suggested_rule(&request));
            }
        }
        for tool in ["read", "grep", "list", "apply_patch"] {
            assert_eq!(
                default_permission_approval_scope(&request(tool)),
                PermissionScope::Project,
            );
        }
    }

    #[test]
    fn scope_changes_persistent_labels_and_removes_the_global_row() {
        let request = request("web_fetch");
        let project = permission_approval_choices(&request, PermissionScope::Project);
        let global = permission_approval_choices(&request, PermissionScope::Global);
        assert_eq!(project.len(), 3);
        assert_eq!(project[1].decision, "allow_project");
        assert_eq!(project[1].label, "Allow");
        assert_eq!(project[1].detail, "for this project");
        assert_eq!(global.len(), 3);
        assert_eq!(global[1].decision, "allow_global");
        assert_eq!(global[1].label, "Allow");
        assert_eq!(global[1].detail, "globally");
        assert!(project[1].is_persistent());
    }

    #[test]
    fn web_tools_offer_tool_wide_session_approval() {
        for tool in ["web_fetch", "web_search"] {
            let mut request = request(tool);
            let InteractionRequestKind::PermissionApproval {
                resource,
                suggested_rule,
                ..
            } = &mut request.kind
            else {
                unreachable!();
            };
            resource.server = (tool == "web_search").then(|| "exa".into());
            resource.operation = Some(
                if tool == "web_fetch" {
                    "fetch"
                } else {
                    "search"
                }
                .into(),
            );
            resource.command = vec![if tool == "web_fetch" {
                "https://example.com/page".into()
            } else {
                "rust agents".into()
            }];
            let suggested = suggested_rule.as_mut().unwrap();
            suggested.server = resource.server.clone();
            suggested.operation = resource.operation.clone();
            suggested.command = Some(resource.command.clone());
            let expected_operation = resource.operation.clone();

            let choices =
                permission_approval_choices(&request, default_permission_approval_scope(&request));
            assert_eq!(
                choices
                    .iter()
                    .map(|choice| choice.label.as_str())
                    .collect::<Vec<_>>(),
                ["Allow", "Allow", "Deny"]
            );
            let choice = &choices[0];
            assert_eq!(choice.decision, "allow_session");
            assert!(choice.supports_session());
            assert!(!choice.is_persistent());
            let rule = choice.session_rule.as_ref().unwrap();
            assert_eq!(rule.tool.as_deref(), Some(tool));
            assert_eq!(rule.operation, expected_operation);
            assert_eq!(rule.access.as_deref(), Some("read"));
            assert!(rule.server.is_none());
            assert!(rule.command.is_none());
            assert!(rule.path.is_none());

            let policy = crate::PermissionPolicy {
                session: vec![rule.clone()],
                ..crate::PermissionPolicy::default()
            };
            let mut subsequent = permission_resource(&request).unwrap().clone();
            subsequent.server = (tool == "web_search").then(|| "searxng".into());
            subsequent.command = vec!["a completely different request".into()];
            assert_eq!(
                policy.evaluate(&subsequent, PermissionEffect::Ask).effect,
                PermissionEffect::Allow
            );
            subsequent.tool = if tool == "web_fetch" {
                "web_search"
            } else {
                "web_fetch"
            }
            .into();
            assert_eq!(
                policy.evaluate(&subsequent, PermissionEffect::Ask).effect,
                PermissionEffect::Ask
            );
        }
    }

    #[test]
    fn bash_read_choices_prepare_exact_files_and_recursive_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("outside");
        std::fs::create_dir(&directory).unwrap();
        let mut request = request("bash");
        let directory = directory.canonicalize().unwrap();
        if let InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule,
            ..
        } = &mut request.kind
        {
            resource.path = Some(path_string(&directory));
            suggested_rule.as_mut().unwrap().path = resource.path.clone();
        }

        let directory_choices = permission_approval_choices(&request, PermissionScope::Project);
        assert_eq!(directory_choices.len(), 3);
        assert_eq!(directory_choices[0].label, "Allow reading");
        assert_eq!(directory_choices[1].decision, "allow_project");
        assert_eq!(
            directory_choices[1].label,
            format!("Always allow reading {}", path_string(&directory))
        );
        assert_eq!(
            directory_choices[1]
                .prepared_rule
                .as_ref()
                .unwrap()
                .path
                .as_deref(),
            Some(format!("{}/**", path_string(&directory)).as_str())
        );

        let file = directory.join("notes?.txt");
        if let InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule,
            ..
        } = &mut request.kind
        {
            resource.path = Some(path_string(&file));
            suggested_rule.as_mut().unwrap().path = resource.path.clone();
        }
        let file_choices = permission_approval_choices(&request, PermissionScope::Global);
        assert_eq!(file_choices[1].decision, "allow_global");
        assert_eq!(
            file_choices[1]
                .prepared_rule
                .as_ref()
                .unwrap()
                .path
                .as_deref(),
            Some(format!("{}/notes\\?.txt", path_string(&directory)).as_str())
        );
    }

    #[test]
    fn home_paths_are_presented_with_a_tilde() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };
        assert_eq!(
            display_path(&path_string(&home.join("notes.txt"))),
            "~/notes.txt"
        );
    }

    #[test]
    fn request_choice_can_be_promoted_to_an_editable_session_rule() {
        let request = request("apply_patch");
        let request_choices = permission_approval_choices(&request, PermissionScope::Project);
        assert_eq!(request_choices[0].decision, "allow_once");
        assert_eq!(request_choices[0].detail, "for this request");
        assert!(request_choices[0].supports_session());

        let session_choices = permission_approval_choices(&request, PermissionScope::Conversation);
        assert_eq!(session_choices[0].decision, "allow_session");
        assert_eq!(session_choices[0].label, "Allow");
        assert_eq!(session_choices[0].detail, "for this conversation");
        assert!(session_choices[0].session_rule.is_some());
        assert_eq!(session_choices[1].decision, "allow_project");
    }

    fn audit(resource: PermissionResource, outcome: PermissionEffect) -> PermissionAudit {
        PermissionAudit {
            resource,
            decision: FilesystemPermissionDecision {
                effect: outcome,
                operation: PermissionDecision {
                    effect: outcome,
                    layer: PermissionLayerKind::Default,
                    rule_id: None,
                    reason: "write default".into(),
                },
                external: None,
            },
            outcome,
            user_reason: None,
            scope: None,
            suggested_pattern: None,
            final_pattern: None,
            resulting_rule_id: None,
            classifier: None,
        }
    }

    #[test]
    fn denied_permission_summary_includes_path_and_rule_reason() {
        let summary = permission_denial_summary(
            &audit(
                PermissionResource {
                    tool: "apply_patch".into(),
                    server: None,
                    operation: None,
                    path: Some("/tmp/project/file.txt".into()),
                    access: Some(PermissionAccess::Write),
                    mode: "ask".into(),
                    agent: "general".into(),
                    command: Vec::new(),
                    raw_command: None,
                    cwd: None,
                },
                PermissionEffect::Deny,
            ),
            Path::new("/tmp/project"),
        );

        assert_eq!(
            summary.unwrap(),
            PermissionDenialSummary {
                resource: "edit file.txt".into(),
                reason: "write default".into(),
            }
        );
    }

    #[test]
    fn denied_permission_summary_prefers_the_user_reason() {
        let mut denied = audit(
            PermissionResource {
                tool: "apply_patch".into(),
                server: None,
                operation: None,
                path: Some("/tmp/project/secret.txt".into()),
                access: Some(crate::PermissionAccess::Write),
                mode: "ask".into(),
                agent: "general".into(),
                command: Vec::new(),
                raw_command: None,
                cwd: None,
            },
            PermissionEffect::Deny,
        );
        denied.user_reason = Some("do not change secrets".into());

        assert_eq!(
            permission_denial_summary(&denied, Path::new("/tmp/project"))
                .unwrap()
                .reason,
            "do not change secrets"
        );
    }

    #[test]
    fn decision_explanation_separates_operation_and_external_boundary() {
        let decision = crate::FilesystemPermissionDecision {
            effect: PermissionEffect::Deny,
            operation: PermissionDecision {
                effect: PermissionEffect::Allow,
                layer: PermissionLayerKind::Mode,
                rule_id: None,
                reason: "mode allows reads".into(),
            },
            external: Some(PermissionDecision {
                effect: PermissionEffect::Deny,
                layer: PermissionLayerKind::Project,
                rule_id: Some("deny-external".into()),
                reason: "external access denied".into(),
            }),
        };

        assert_eq!(
            permission_decision_explanation(&decision),
            PermissionDecisionExplanation {
                operation: "Operation  allow · mode allows reads".into(),
                external: Some("Boundary   deny · external access denied".into()),
                result: "Result     deny".into(),
            }
        );
    }

    #[test]
    fn denied_permission_summary_uses_command_and_user_fallback() {
        let mut denied = audit(
            PermissionResource {
                tool: "bash".into(),
                server: None,
                operation: None,
                path: None,
                access: Some(PermissionAccess::Execute),
                mode: "ask".into(),
                agent: "general".into(),
                command: vec!["echo".into(), "hello".into()],
                raw_command: Some("echo hello".into()),
                cwd: Some("/tmp".into()),
            },
            PermissionEffect::Ask,
        );
        denied.outcome = PermissionEffect::Deny;
        denied.decision.effect = PermissionEffect::Ask;
        denied.decision.operation.effect = PermissionEffect::Ask;

        assert_eq!(
            permission_denial_summary(&denied, Path::new("/tmp/project")).unwrap(),
            PermissionDenialSummary {
                resource: "bash echo hello".into(),
                reason: "denied by user".into(),
            }
        );
    }

    #[test]
    fn denied_permission_summary_identifies_mcp_external_denials() {
        let mut denied = audit(
            PermissionResource {
                tool: "mcp".into(),
                server: Some("docs".into()),
                operation: Some("search".into()),
                path: None,
                access: None,
                mode: "ask".into(),
                agent: "general".into(),
                command: Vec::new(),
                raw_command: None,
                cwd: None,
            },
            PermissionEffect::Deny,
        );
        denied.decision.operation.effect = PermissionEffect::Allow;
        denied.decision.operation.reason = "MCP allowed".into();
        denied.decision.external = Some(PermissionDecision {
            effect: PermissionEffect::Deny,
            layer: PermissionLayerKind::Project,
            rule_id: Some("deny-external".into()),
            reason: "external access denied".into(),
        });

        assert_eq!(
            permission_denial_summary(&denied, Path::new("/tmp/project")).unwrap(),
            PermissionDenialSummary {
                resource: "mcp docs/search".into(),
                reason: "external access denied".into(),
            }
        );
    }

    #[test]
    fn allowed_permission_does_not_produce_a_denial_summary() {
        let resource = PermissionResource {
            tool: "read".into(),
            server: None,
            operation: None,
            path: Some("/tmp/file.txt".into()),
            access: Some(PermissionAccess::Read),
            mode: "ask".into(),
            agent: "general".into(),
            command: Vec::new(),
            raw_command: None,
            cwd: None,
        };

        assert!(
            permission_denial_summary(&audit(resource, PermissionEffect::Allow), Path::new("/tmp"))
                .is_none()
        );
    }
}
