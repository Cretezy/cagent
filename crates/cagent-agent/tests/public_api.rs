use cagent_agent::WorkspaceEntry;
use cagent_agent::config::{
    AppPaths, BellConfig, BellMethod, ConfigSnapshot, ConfigStore, InstructionSnapshot,
    PathOverrides,
};
use cagent_agent::mcp::{
    McpConfigService, McpEffectiveServer, McpImportPreview, McpLocation, McpMutationPreview,
    McpRuntimeStatus, McpScope, McpServerDefinition, McpSupervisor, McpToolResult,
    McpTransportConfig, ProjectControlStatus, WorkspaceTrust,
};
use cagent_agent::permissions::{
    PermissionDecision, PermissionEffect, PermissionFile, PermissionRule, PermissionScope,
};
use cagent_agent::presentation::{
    DelegatedRunLive, HistoryRow, MarkdownDocument, ModelPickerRow, ProviderPickerRow,
    ToolActivityGroup,
};
use cagent_agent::protocol::{
    API_VERSION, AgentRunTimelineEntry, DurableEvent, RuntimeEvent, SessionCommand,
    SessionSnapshot, SessionUpdate, SessionUpdateNotice, TranscriptCursor, TranscriptPage,
    TranscriptWindow, TurnState, UpdatePlanArgs,
};
use cagent_agent::provider::{AuthFlow, ModelDescriptor, ProviderDescriptor};
use cagent_agent::runtime::{
    AgentRuntime, NewSession, RuntimeOptions, SessionAttachment, SessionHandle, SessionUpdateStream,
};
use cagent_agent::tools::{BashRequest, SemanticDiff};

fn public_type<T>() {}

#[test]
fn grouped_frontend_api_is_public() {
    public_type::<AppPaths>();
    public_type::<ConfigSnapshot>();
    public_type::<ConfigStore>();
    public_type::<BellMethod>();
    public_type::<BellConfig>();
    public_type::<InstructionSnapshot>();
    public_type::<PathOverrides>();
    public_type::<PermissionDecision>();
    public_type::<PermissionRule>();
    public_type::<McpConfigService>();
    public_type::<McpEffectiveServer>();
    public_type::<McpImportPreview>();
    public_type::<McpLocation>();
    public_type::<McpMutationPreview>();
    public_type::<McpRuntimeStatus>();
    public_type::<McpScope>();
    public_type::<McpServerDefinition>();
    public_type::<McpSupervisor>();
    public_type::<McpToolResult>();
    public_type::<McpTransportConfig>();
    public_type::<ProjectControlStatus>();
    public_type::<WorkspaceTrust>();
    public_type::<HistoryRow>();
    public_type::<MarkdownDocument>();
    public_type::<ModelPickerRow>();
    public_type::<ProviderPickerRow>();
    public_type::<ToolActivityGroup>();
    public_type::<DelegatedRunLive>();
    public_type::<DurableEvent>();
    public_type::<RuntimeEvent>();
    public_type::<SessionCommand>();
    public_type::<SessionSnapshot>();
    public_type::<SessionUpdate>();
    public_type::<SessionUpdateNotice>();
    public_type::<TranscriptWindow>();
    public_type::<TranscriptCursor>();
    public_type::<TranscriptPage>();
    public_type::<TurnState>();
    public_type::<UpdatePlanArgs>();
    public_type::<AgentRunTimelineEntry>();
    public_type::<AuthFlow>();
    public_type::<ModelDescriptor>();
    public_type::<ProviderDescriptor>();
    public_type::<AgentRuntime>();
    public_type::<NewSession>();
    public_type::<RuntimeOptions>();
    public_type::<SessionHandle>();
    public_type::<SessionAttachment>();
    public_type::<SessionUpdateStream>();
    public_type::<BashRequest>();
    public_type::<WorkspaceEntry>();
    public_type::<SemanticDiff>();
    assert_eq!(API_VERSION, 1);
}

#[test]
fn permission_file_edits_trust_and_rules_with_one_toml_document() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let permissions_path = temporary.path().join("permissions.toml");
    std::fs::write(&permissions_path, "# preserve me\nversion = 1\n").unwrap();
    let file = PermissionFile::new(permissions_path.clone(), &workspace).unwrap();

    file.set_trusted(true).unwrap();
    let stored = file
        .persist_rule(
            PermissionScope::Project,
            PermissionRule {
                id: "project-read".into(),
                effect: PermissionEffect::Allow,
                tool: Some("read".into()),
                server: None,
                operation: None,
                path: None,
                command: None,
                raw_command: None,
                cwd: None,
                access: Some("read".into()),
                external: false,
                mode: None,
                agent: None,
                source: Some("test".into()),
                created_at: None,
            },
        )
        .unwrap();

    let source = std::fs::read_to_string(permissions_path).unwrap();
    assert!(source.contains("# preserve me"));
    assert!(source.contains("trusted = true"));
    assert!(source.contains(&stored.id));
    assert!(file.is_trusted().unwrap());
    assert_eq!(file.load().unwrap().project[0].id, "project-read");
}

#[test]
fn config_store_hides_implicit_parent_tables() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("config.toml");
    std::fs::write(&path, "# preserve me\nversion = 1\n").unwrap();
    let store = ConfigStore::open(&path).unwrap();

    store.set_value("a.b.c.x", "true").unwrap();

    let source = store.source().unwrap();
    assert!(source.contains("[a.b.c]"));
    assert!(source.contains("x = true"));
    assert!(!source.contains("\n[a]\n"));
    assert!(!source.contains("\n[a.b]\n"));
    assert!(source.contains("# preserve me"));
}
