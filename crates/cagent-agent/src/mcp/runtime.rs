#![allow(deprecated)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, ClientInfo, Implementation, ListRootsResult,
    LoggingMessageNotificationParam, ProtocolVersion, Root,
};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt as _};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt as _;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use super::{
    McpConfigService, McpEffectiveServer, McpRunner, McpRuntimeStatus, McpSecretStore,
    McpServerDefinition, McpToolResult, McpToolSummary, McpTransportConfig,
};
use crate::{RuntimeError, ToolDefinition};

const DIAGNOSTIC_ITEMS: usize = 32;
const DIAGNOSTIC_BYTES: usize = 16 * 1024;
const SUMMARY_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ServerKey {
    workspace: PathBuf,
    name: String,
    fingerprint: String,
}

type Client = RunningService<RoleClient, CagentClientHandler>;

struct ServerRuntime {
    name: String,
    definition: McpServerDefinition,
    status: McpRuntimeStatus,
    generation: u64,
    restart_attempted: bool,
    client: Option<Arc<Client>>,
    tools: Vec<McpToolSummary>,
    handler: CagentClientHandler,
    startup_in_flight: bool,
    refresh_in_flight: bool,
    changed: Arc<Notify>,
}

#[derive(Clone, Debug)]
struct CagentClientHandler {
    workspace: PathBuf,
    diagnostics: Arc<std::sync::Mutex<DiagnosticBuffer>>,
    tools_changed: Arc<std::sync::atomic::AtomicBool>,
    redactor: Arc<std::sync::Mutex<Redactor>>,
}

impl CagentClientHandler {
    fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            diagnostics: Arc::new(std::sync::Mutex::new(DiagnosticBuffer::default())),
            tools_changed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            redactor: Arc::new(std::sync::Mutex::new(Redactor::default())),
        }
    }

    fn push(&self, value: impl Into<String>) {
        if let Ok(mut diagnostics) = self.diagnostics.lock() {
            let value = self.redact(&value.into());
            diagnostics.push(&value);
        }
    }

    fn set_redactor(&self, redactor: Redactor) {
        if let Ok(mut active) = self.redactor.lock() {
            *active = redactor;
        }
    }

    fn redactor(&self) -> Redactor {
        self.redactor
            .lock()
            .map(|redactor| redactor.clone())
            .unwrap_or_default()
    }

    fn redact(&self, value: &str) -> String {
        self.redactor().redact(value)
    }

    fn snapshot(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .map(|diagnostics| diagnostics.items.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl ClientHandler for CagentClientHandler {
    fn get_info(&self) -> ClientInfo {
        let capabilities =
            serde_json::from_value(serde_json::json!({ "roots": {} })).unwrap_or_default();
        ClientInfo::new(
            capabilities,
            Implementation::new("cagent", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_roots(
        &self,
        _context: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, rmcp::ErrorData> {
        let uri = url::Url::from_directory_path(&self.workspace)
            .map_err(|()| rmcp::ErrorData::internal_error("invalid workspace root", None))?;
        Ok(ListRootsResult::new(vec![
            Root::new(uri.to_string()).with_name("workspace"),
        ]))
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.push(format!("{:?}: {}", params.level, params.data));
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.tools_changed
            .store(true, std::sync::atomic::Ordering::Release);
        self.push("server reported that its tool catalog changed");
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.push("server resource capability is unsupported and was ignored");
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.push("server prompt capability is unsupported and was ignored");
    }
}

#[derive(Debug, Default)]
struct DiagnosticBuffer {
    items: VecDeque<String>,
    bytes: usize,
}

impl DiagnosticBuffer {
    fn push(&mut self, value: &str) {
        let value = terminal_safe(value, 2_048);
        self.bytes += value.len();
        self.items.push_back(value);
        while self.items.len() > DIAGNOSTIC_ITEMS || self.bytes > DIAGNOSTIC_BYTES {
            if let Some(removed) = self.items.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
            } else {
                break;
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct McpRegistrySnapshot {
    pub tools: Vec<ToolDefinition>,
    targets: HashMap<String, PinnedTool>,
}

#[derive(Clone)]
pub(crate) enum McpRegistryReadiness {
    ReadyOnly,
    WaitUntil {
        turn_started_at: Instant,
        cancellation: CancellationToken,
    },
}

#[derive(Clone)]
struct PinnedTool {
    key: ServerKey,
    runtime: Arc<Mutex<ServerRuntime>>,
    server: String,
    tool: String,
    provider_name: String,
    definition: McpServerDefinition,
    configured_read_only: bool,
}

impl std::fmt::Debug for PinnedTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PinnedTool")
            .field("key", &self.key)
            .field("server", &self.server)
            .field("tool", &self.tool)
            .field("provider_name", &self.provider_name)
            .finish_non_exhaustive()
    }
}

enum McpCallFailure {
    Restartable(RuntimeError),
    Final(RuntimeError),
}

impl McpCallFailure {
    fn into_runtime(self) -> RuntimeError {
        match self {
            Self::Restartable(error) | Self::Final(error) => error,
        }
    }
}

impl McpRegistrySnapshot {
    pub(crate) fn is_mcp_tool(&self, name: &str) -> bool {
        self.targets.contains_key(name)
    }

    pub(crate) fn is_read_only(&self, name: &str) -> bool {
        self.targets
            .get(name)
            .is_some_and(|target| target.configured_read_only)
    }

    pub(crate) fn identity(&self, name: &str) -> Option<(&str, &str)> {
        self.targets
            .get(name)
            .map(|target| (target.server.as_str(), target.tool.as_str()))
    }

    pub(crate) fn description(&self, name: &str) -> Option<&str> {
        self.tools
            .iter()
            .find(|tool| tool.name == name)
            .map(|tool| tool.description.as_str())
    }
}

#[derive(Clone, Default)]
pub struct McpSupervisor {
    servers: Arc<Mutex<HashMap<ServerKey, Arc<Mutex<ServerRuntime>>>>>,
    secrets: Option<McpSecretStore>,
}

impl std::fmt::Debug for McpSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpSupervisor")
            .finish_non_exhaustive()
    }
}

impl McpSupervisor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_secret_store(mut self, store: McpSecretStore) -> Self {
        self.secrets = Some(store);
        self
    }

    pub(crate) fn package_secret_status(
        &self,
        server: &str,
        secret: &str,
    ) -> Result<super::McpSecretStatus, RuntimeError> {
        let store = self.secrets.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("managed MCP credential storage is unavailable".into())
        })?;
        store.status(&format!("mcp/{server}/{secret}"))
    }

    pub(crate) fn update_package_secret(
        &self,
        server: &str,
        secret: &str,
        value: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let store = self.secrets.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("managed MCP credential storage is unavailable".into())
        })?;
        let reference = format!("mcp/{server}/{secret}");
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            store.set(&reference, value)
        } else {
            store.delete(&reference)
        }
    }

    pub(crate) async fn begin_oauth(
        &self,
        server: &McpEffectiveServer,
    ) -> Result<super::McpOAuthFlow, RuntimeError> {
        let super::McpTransportConfig::StreamableHttp { url, .. } = &server.definition.transport
        else {
            return Err(RuntimeError::InvalidOption(
                "MCP OAuth is only available for HTTP servers".into(),
            ));
        };
        let config = server.definition.oauth.clone().unwrap_or_default();
        let store = self.secrets.clone().ok_or_else(|| {
            RuntimeError::InvalidOption("managed MCP credential storage is unavailable".into())
        })?;
        super::begin_oauth(url, &config, store).await
    }

    pub(crate) fn oauth_status(
        &self,
        server: &McpEffectiveServer,
    ) -> Result<super::McpSecretStatus, RuntimeError> {
        let super::McpTransportConfig::StreamableHttp { url, .. } = &server.definition.transport
        else {
            return Err(RuntimeError::InvalidOption(
                "MCP OAuth is only available for HTTP servers".into(),
            ));
        };
        let store = self.secrets.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("managed MCP credential storage is unavailable".into())
        })?;
        super::oauth_status(url, store)
    }

    pub(crate) fn clear_oauth(&self, server: &McpEffectiveServer) -> Result<(), RuntimeError> {
        let super::McpTransportConfig::StreamableHttp { url, .. } = &server.definition.transport
        else {
            return Err(RuntimeError::InvalidOption(
                "MCP OAuth is only available for HTTP servers".into(),
            ));
        };
        let store = self.secrets.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("managed MCP credential storage is unavailable".into())
        })?;
        super::clear_oauth(url, store)
    }

    pub(crate) async fn forget_server(&self, workspace: &Path, name: &str) {
        let removed = {
            let mut servers = self.servers.lock().await;
            let keys = servers
                .keys()
                .filter(|key| key.workspace == workspace && key.name == name)
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| servers.remove(&key))
                .collect::<Vec<_>>()
        };
        for runtime in removed {
            stop_runtime(&runtime).await;
        }
    }

    /// Starts every enabled shared-catalog definition assigned to an active agent.
    ///
    /// # Errors
    ///
    /// Returns an error if effective configuration cannot be resolved.
    #[tracing::instrument(level = "trace", name = "mcp.activate_agent", skip_all)]
    pub async fn activate_agent(
        &self,
        config: &McpConfigService,
        agent: &str,
    ) -> Result<(), RuntimeError> {
        self.reconcile(config).await?;
        for server in config.list_effective(agent)? {
            if server.allowed_for_agent && server.definition.enabled {
                self.ensure_started(config.workspace(), &server).await;
            }
        }
        Ok(())
    }

    /// Backwards-compatible name for embedders; eager configuration itself is no
    /// longer supported and activation is agent-scoped.
    pub async fn start_eager(
        &self,
        config: &McpConfigService,
        agent: &str,
    ) -> Result<(), RuntimeError> {
        self.activate_agent(config, agent).await
    }

    /// Discovers a server catalog for UI inspection. Discovery uses the same
    /// redaction, timeouts, and schema conversion as provider activation.
    pub async fn inspect(
        &self,
        config: &McpConfigService,
        server: &McpEffectiveServer,
    ) -> Result<Vec<McpToolSummary>, RuntimeError> {
        self.reconcile(config).await?;
        let state = self.ensure_started(config.workspace(), server).await;
        let deadline = Instant::now()
            + Duration::from_secs(
                server
                    .definition
                    .startup_timeout_seconds
                    .saturating_add(server.definition.request_timeout_seconds),
            );
        self.wait_until_ready(&state, deadline, &CancellationToken::new())
            .await
            .map(|(_, tools)| tools)
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!(
                    "MCP server {} did not become ready",
                    server.name
                ))
            })
    }

    pub(crate) async fn pin_registry(
        &self,
        config: &McpConfigService,
        agent: &str,
        readiness: McpRegistryReadiness,
    ) -> McpRegistrySnapshot {
        let mut snapshot = McpRegistrySnapshot::default();
        if self.reconcile(config).await.is_err() {
            return snapshot;
        }
        let mut used = HashSet::new();
        let Ok(servers) = config.list_effective(agent) else {
            return snapshot;
        };
        let workspace = config.workspace().to_path_buf();
        let candidates = servers
            .into_iter()
            .filter(|server| server.allowed_for_agent && server.definition.enabled)
            .collect::<Vec<_>>();
        let resolved = futures_util::future::join_all(candidates.into_iter().map(|server| {
            let supervisor = self.clone();
            let workspace = workspace.clone();
            let readiness = readiness.clone();
            async move {
                let state = supervisor.ensure_started(&workspace, &server).await;
                let ready = match readiness {
                    McpRegistryReadiness::ReadyOnly => supervisor.ready_snapshot(&state).await,
                    McpRegistryReadiness::WaitUntil {
                        turn_started_at,
                        cancellation,
                    } => {
                        let deadline = turn_started_at
                            + Duration::from_secs(server.definition.startup_timeout_seconds);
                        supervisor
                            .wait_until_ready(&state, deadline, &cancellation)
                            .await
                    }
                };
                (server, ready)
            }
        }))
        .await;
        for (server, ready) in resolved {
            let Some((key, tools)) = ready else {
                continue;
            };
            let Some(state) = self.servers.lock().await.get(&key).cloned() else {
                continue;
            };
            for tool in tools {
                let provider_name = unique_provider_name(&server.name, &tool.name, &mut used);
                snapshot.tools.push(ToolDefinition {
                    name: provider_name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                    asynchronous: false,
                });
                snapshot.targets.insert(
                    provider_name.clone(),
                    PinnedTool {
                        key: key.clone(),
                        runtime: state.clone(),
                        server: server.name.clone(),
                        tool: tool.name.clone(),
                        provider_name,
                        definition: server.definition.clone(),
                        configured_read_only: server
                            .definition
                            .read_only_tools
                            .iter()
                            .any(|name| name == &tool.name),
                    },
                );
            }
        }
        snapshot
    }

    async fn reconcile(&self, config: &McpConfigService) -> Result<(), RuntimeError> {
        let workspace = config.workspace();
        let active = config
            .list_all()?
            .into_iter()
            .filter(|(_, _, definition)| definition.enabled)
            .map(|(_, name, definition)| ServerKey {
                workspace: workspace.to_path_buf(),
                name,
                fingerprint: fingerprint(&definition),
            })
            .collect::<HashSet<_>>();
        let removed = {
            let mut servers = self.servers.lock().await;
            let stale = servers
                .keys()
                .filter(|key| key.workspace == workspace && !active.contains(*key))
                .cloned()
                .collect::<Vec<_>>();
            stale
                .into_iter()
                .filter_map(|key| servers.remove(&key))
                .collect::<Vec<_>>()
        };
        for runtime in removed {
            // A pinned turn owns the runtime until its request boundary. With
            // no pinned owner, stop the transport immediately.
            if Arc::strong_count(&runtime) == 1 {
                stop_runtime(&runtime).await;
            }
        }
        Ok(())
    }

    #[tracing::instrument(
        level = "trace",
        name = "mcp.call",
        skip_all,
        fields(tool = %provider_name)
    )]
    pub(crate) async fn call_pinned(
        &self,
        snapshot: &McpRegistrySnapshot,
        provider_name: &str,
        arguments: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<McpToolResult, RuntimeError> {
        let target = snapshot.targets.get(provider_name).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("stale or disabled MCP tool: {provider_name}"))
        })?;
        let started = Instant::now();
        let first = self
            .call_target(target, arguments.clone(), cancellation)
            .await;
        let result = match first {
            Ok(result) => result,
            Err(McpCallFailure::Final(error)) => return Err(error),
            Err(first_error @ McpCallFailure::Restartable(_)) => {
                let runtime = target.runtime.clone();
                let should_restart = {
                    let mut runtime = runtime.lock().await;
                    if runtime.restart_attempted {
                        false
                    } else {
                        runtime.restart_attempted = true;
                        runtime.status = McpRuntimeStatus::Restarting;
                        runtime.client.take();
                        true
                    }
                };
                if !should_restart {
                    return Err(first_error.into_runtime());
                }
                let server = McpEffectiveServer {
                    name: target.server.clone(),
                    location: super::McpLocation::global(),
                    definition: target.definition.clone(),
                    status: McpRuntimeStatus::Restarting,
                    agents: target.definition.agents.clone(),
                    allowed_for_agent: true,
                    overridden: Vec::new(),
                    tools: Vec::new(),
                    diagnostics: Vec::new(),
                    generation: 0,
                };
                let state = self.ensure_started(&target.key.workspace, &server).await;
                let deadline = Instant::now()
                    + Duration::from_secs(
                        target
                            .definition
                            .startup_timeout_seconds
                            .saturating_add(target.definition.request_timeout_seconds),
                    );
                self.wait_until_ready(&state, deadline, cancellation)
                    .await
                    .ok_or_else(|| {
                        RuntimeError::InvalidOption(format!(
                            "MCP server {} did not restart in time",
                            target.server
                        ))
                    })?;
                self.call_target(target, arguments, cancellation)
                    .await
                    .map_err(McpCallFailure::into_runtime)?
            }
        };
        let duration_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let redactor = target.runtime.lock().await.handler.redactor();
        let content = redactor.redact_value(serde_json::to_value(&result.content)?);
        let summary_source = result
            .content
            .iter()
            .filter_map(|content| content.as_text().map(|text| text.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(McpToolResult {
            server: target.server.clone(),
            tool: target.tool.clone(),
            provider_name: target.provider_name.clone(),
            content,
            structured_content: result
                .structured_content
                .map(|value| redactor.redact_value(value)),
            is_error: result.is_error.unwrap_or(false),
            duration_millis,
            summary: terminal_safe(&redactor.redact(&summary_source), SUMMARY_BYTES),
        })
    }

    #[tracing::instrument(
        level = "trace",
        name = "mcp.call_target",
        skip_all,
        fields(server = %target.server, tool = %target.tool)
    )]
    async fn call_target(
        &self,
        target: &PinnedTool,
        arguments: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<rmcp::model::CallToolResult, McpCallFailure> {
        let runtime = target.runtime.clone();
        let (client, handler) = {
            let runtime = runtime.lock().await;
            let client = runtime.client.clone().ok_or_else(|| {
                McpCallFailure::Restartable(RuntimeError::InvalidOption(format!(
                    "MCP server {} is not connected",
                    target.server
                )))
            })?;
            (client, runtime.handler.clone())
        };
        let arguments = arguments.as_object().cloned().ok_or_else(|| {
            McpCallFailure::Final(RuntimeError::InvalidOption(
                "MCP tool arguments must be an object".into(),
            ))
        })?;
        let request = client
            .call_tool(CallToolRequestParams::new(target.tool.clone()).with_arguments(arguments));
        let timeout = Duration::from_secs(target.definition.request_timeout_seconds);
        tokio::select! {
            () = cancellation.cancelled() => Err(McpCallFailure::Final(RuntimeError::InvalidOption("MCP tool call cancelled".into()))),
            result = tokio::time::timeout(timeout, request) => match result {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(error)) => Err(McpCallFailure::Restartable(RuntimeError::InvalidOption(handler.redact(&format!("MCP tool call failed: {error}"))))),
                Err(_) => Err(McpCallFailure::Final(RuntimeError::InvalidOption(format!("MCP tool call timed out after {} seconds", target.definition.request_timeout_seconds)))),
            }
        }
    }

    /// Returns effective definitions enriched with live supervisor state.
    ///
    /// # Errors
    ///
    /// Returns an error if effective configuration cannot be resolved.
    pub async fn describe(
        &self,
        config: &McpConfigService,
        agent: &str,
    ) -> Result<Vec<McpEffectiveServer>, RuntimeError> {
        let mut servers = config.list_effective(agent)?;
        for server in &mut servers {
            let key = ServerKey {
                workspace: config.workspace().to_path_buf(),
                name: server.name.clone(),
                fingerprint: fingerprint(&server.definition),
            };
            let runtime = self.servers.lock().await.get(&key).cloned();
            if let Some(runtime) = runtime {
                let runtime = runtime.lock().await;
                server.status.clone_from(&runtime.status);
                server.tools.clone_from(&runtime.tools);
                server.diagnostics = runtime.handler.snapshot();
                server.generation = runtime.generation;
            }
        }
        Ok(servers)
    }

    async fn ensure_started(
        &self,
        workspace: &Path,
        server: &McpEffectiveServer,
    ) -> Arc<Mutex<ServerRuntime>> {
        let key = ServerKey {
            workspace: workspace.to_path_buf(),
            name: server.name.clone(),
            fingerprint: fingerprint(&server.definition),
        };
        let state = {
            let mut servers = self.servers.lock().await;
            servers
                .entry(key.clone())
                .or_insert_with(|| {
                    let handler = CagentClientHandler::new(workspace.to_path_buf());
                    Arc::new(Mutex::new(ServerRuntime {
                        name: server.name.clone(),
                        definition: server.definition.clone(),
                        status: McpRuntimeStatus::NotStarted,
                        generation: 1,
                        restart_attempted: false,
                        client: None,
                        tools: Vec::new(),
                        handler,
                        startup_in_flight: false,
                        refresh_in_flight: false,
                        changed: Arc::new(Notify::new()),
                    }))
                })
                .clone()
        };
        let mut start = None;
        let mut refresh = None;
        {
            let mut runtime = state.lock().await;
            let connected = runtime
                .client
                .as_ref()
                .is_some_and(|client| !client.is_closed());
            if connected {
                if runtime
                    .handler
                    .tools_changed
                    .swap(false, std::sync::atomic::Ordering::AcqRel)
                    && !runtime.refresh_in_flight
                {
                    runtime.refresh_in_flight = true;
                    refresh.clone_from(&runtime.client);
                }
            } else if !runtime.startup_in_flight {
                runtime.startup_in_flight = true;
                if !matches!(runtime.status, McpRuntimeStatus::Restarting) {
                    runtime.status = McpRuntimeStatus::Starting;
                }
                start = Some((runtime.handler.clone(), runtime.definition.clone()));
            }
        }
        if let Some((handler, definition)) = start {
            let supervisor = self.clone();
            let state = Arc::clone(&state);
            let workspace = workspace.to_path_buf();
            let name = server.name.clone();
            tokio::spawn(async move {
                let result = connect_and_discover(
                    &workspace,
                    &name,
                    &definition,
                    handler,
                    supervisor.secrets.as_ref(),
                )
                .await;
                supervisor.publish_startup_result(&state, result).await;
            });
        } else if let Some(client) = refresh {
            let supervisor = self.clone();
            let state = Arc::clone(&state);
            let name = server.name.clone();
            let definition = server.definition.clone();
            tokio::spawn(async move {
                let result = discover_tools(&name, &definition, &client).await;
                supervisor.publish_refresh_result(&state, result).await;
            });
        }
        state
    }

    async fn ready_snapshot(
        &self,
        state: &Arc<Mutex<ServerRuntime>>,
    ) -> Option<(ServerKey, Vec<McpToolSummary>)> {
        let runtime = state.lock().await;
        let client = runtime.client.as_ref()?;
        if client.is_closed() || runtime.status != McpRuntimeStatus::Connected {
            return None;
        }
        Some((
            ServerKey {
                workspace: runtime.handler.workspace.clone(),
                name: runtime.name.clone(),
                fingerprint: fingerprint(&runtime.definition),
            },
            runtime.tools.clone(),
        ))
    }

    async fn wait_until_ready(
        &self,
        state: &Arc<Mutex<ServerRuntime>>,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Option<(ServerKey, Vec<McpToolSummary>)> {
        let changed = state.lock().await.changed.clone();
        loop {
            let notified = changed.notified();
            if let Some(snapshot) = self.ready_snapshot(state).await {
                return Some(snapshot);
            }
            if matches!(
                state.lock().await.status,
                McpRuntimeStatus::Failed { .. }
                    | McpRuntimeStatus::AuthenticationRequired
                    | McpRuntimeStatus::Stopped
                    | McpRuntimeStatus::Disabled
            ) {
                return None;
            }
            tokio::select! {
                () = cancellation.cancelled() => return None,
                () = tokio::time::sleep_until(deadline.into()) => return None,
                () = notified => {}
            }
        }
    }

    async fn publish_startup_result(
        &self,
        state: &Arc<Mutex<ServerRuntime>>,
        result: Result<(Arc<Client>, Vec<McpToolSummary>), RuntimeError>,
    ) {
        let mut runtime = state.lock().await;
        runtime.startup_in_flight = false;
        match result {
            Ok((client, tools)) => {
                runtime.client = Some(client);
                runtime.tools = tools;
                runtime.status = McpRuntimeStatus::Connected;
            }
            Err(error) => set_runtime_failed(&mut runtime, &error),
        }
        runtime.changed.notify_waiters();
    }

    async fn publish_refresh_result(
        &self,
        state: &Arc<Mutex<ServerRuntime>>,
        result: Result<Vec<McpToolSummary>, RuntimeError>,
    ) {
        let mut runtime = state.lock().await;
        runtime.refresh_in_flight = false;
        match result {
            Ok(tools) => runtime.tools = tools,
            Err(error) => runtime.handler.push(error.to_string()),
        }
        runtime.changed.notify_waiters();
    }
}

fn set_runtime_failed(runtime: &mut ServerRuntime, error: &RuntimeError) {
    let message = runtime
        .handler
        .redact(&terminal_safe(&error.to_string(), 2_048));
    runtime.handler.push(message.clone());
    runtime.status = if matches!(error, RuntimeError::McpAuthenticationRequired) {
        McpRuntimeStatus::AuthenticationRequired
    } else {
        McpRuntimeStatus::Failed { message }
    };
}

async fn stop_runtime(runtime: &Arc<Mutex<ServerRuntime>>) {
    let client = {
        let mut runtime = runtime.lock().await;
        runtime.status = McpRuntimeStatus::Stopped;
        runtime.tools.clear();
        runtime.changed.notify_waiters();
        runtime.client.take()
    };
    if let Some(client) = client {
        client.cancellation_token().cancel();
    }
}

async fn discover_tools(
    server_name: &str,
    definition: &McpServerDefinition,
    client: &Client,
) -> Result<Vec<McpToolSummary>, RuntimeError> {
    let response = tokio::time::timeout(
        Duration::from_secs(definition.request_timeout_seconds),
        client.list_all_tools(),
    )
    .await
    .map_err(|_| RuntimeError::InvalidOption("MCP tools/list timed out".into()))?
    .map_err(|error| RuntimeError::InvalidOption(format!("MCP tools/list failed: {error}")))?;
    Ok(response
        .into_iter()
        .map(|tool| McpToolSummary {
            server: server_name.to_owned(),
            name: tool.name.to_string(),
            provider_name: provider_safe_tool_name(server_name, &tool.name),
            description: tool
                .description
                .map(|description| description.to_string())
                .unwrap_or_default(),
            input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
            configured_read_only: definition
                .read_only_tools
                .iter()
                .any(|name| name == tool.name.as_ref()),
        })
        .collect())
}

async fn connect_and_discover(
    workspace: &Path,
    server_name: &str,
    definition: &McpServerDefinition,
    handler: CagentClientHandler,
    secrets: Option<&McpSecretStore>,
) -> Result<(Arc<Client>, Vec<McpToolSummary>), RuntimeError> {
    let client = Arc::new(connect(workspace, definition, handler.clone(), secrets).await?);
    let tools = discover_tools(server_name, definition, &client)
        .await
        .map_err(|error| RuntimeError::InvalidOption(handler.redact(&error.to_string())))?;
    Ok((client, tools))
}

#[tracing::instrument(level = "trace", name = "mcp.connect", skip_all)]
async fn connect(
    workspace: &Path,
    definition: &McpServerDefinition,
    handler: CagentClientHandler,
    secrets: Option<&McpSecretStore>,
) -> Result<Client, RuntimeError> {
    let (resolved, mut redactor) = ResolvedDefinition::resolve(workspace, definition, secrets)?;
    handler.set_redactor(redactor.clone());
    let timeout = Duration::from_secs(definition.startup_timeout_seconds);
    let lifecycle = ClientLifecycleMode::Auto {
        preferred_versions: modern_protocol_versions(),
        // Legacy stdio servers such as ha-mcp commonly implement the stable
        // 2024 handshake and reject later initialize parameter shapes.
        legacy_version: Some(ProtocolVersion::V_2024_11_05),
    };
    let connected = match resolved {
        ResolvedDefinition::Stdio {
            command,
            args,
            cwd,
            env,
            env_remove,
            inherit_env,
        } => {
            let (executable, process_args) = if definition.runner == McpRunner::Oci {
                oci_command(definition, &command, &args, &env, &env_remove)?
            } else {
                (command, args)
            };
            let mut process = tokio::process::Command::new(executable);
            process.args(process_args);
            if let Some(cwd) = cwd {
                process.current_dir(cwd);
            }
            if definition.runner == McpRunner::Host && !inherit_env {
                process.env_clear();
            }
            for name in env_remove {
                process.env_remove(name);
            }
            if definition.runner == McpRunner::Host {
                process.envs(env);
            } else {
                // Docker and Podman copy these named variables into the container.
                // Keeping values in the runtime process environment prevents managed
                // secrets from appearing in command-line inspection or diagnostics.
                process.envs(env);
            }
            let (transport, stderr) = TokioChildProcess::builder(process)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| {
                    RuntimeError::InvalidOption(redactor.redact(&error.to_string()))
                })?;
            if let Some(stderr) = stderr {
                capture_stderr(stderr, handler.clone());
            }
            tokio::time::timeout(timeout, handler.serve_with_lifecycle(transport, lifecycle)).await
        }
        ResolvedDefinition::Http {
            url,
            headers,
            oauth,
            bearer_token,
        } => {
            let oauth_configured = oauth.is_some();
            let oauth = oauth.unwrap_or_default();
            let custom_headers = headers
                .into_iter()
                .map(|(name, value)| {
                    let name = name.parse::<http::HeaderName>().map_err(|error| {
                        RuntimeError::InvalidOption(format!("invalid MCP header name: {error}"))
                    })?;
                    let value = value.parse::<http::HeaderValue>().map_err(|error| {
                        RuntimeError::InvalidOption(
                            redactor.redact(&format!("invalid MCP header value: {error}")),
                        )
                    })?;
                    Ok((name, value))
                })
                .collect::<Result<HashMap<_, _>, RuntimeError>>()?;
            let mut config =
                StreamableHttpClientTransportConfig::with_uri(url).custom_headers(custom_headers);
            let oauth_client = if let Some(store) = secrets {
                super::oauth::authorized_client(&config.uri, &oauth, store.clone()).await?
            } else {
                None
            };
            if let Some(oauth_client) = oauth_client {
                let transport = StreamableHttpClientTransport::with_client(oauth_client, config);
                tokio::time::timeout(timeout, handler.serve_with_lifecycle(transport, lifecycle))
                    .await
            } else if let Some(token) = bearer_token {
                redactor.secrets.push(token.clone());
                handler.set_redactor(redactor.clone());
                config = config.auth_header(token);
                let transport = StreamableHttpClientTransport::from_config(config);
                tokio::time::timeout(timeout, handler.serve_with_lifecycle(transport, lifecycle))
                    .await
            } else if oauth_configured {
                return Err(RuntimeError::McpAuthenticationRequired);
            } else {
                let transport = StreamableHttpClientTransport::from_config(config);
                tokio::time::timeout(timeout, handler.serve_with_lifecycle(transport, lifecycle))
                    .await
            }
        }
    };
    match connected {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(error)) => {
            let message = redactor.redact(&format!("MCP initialization failed: {error}"));
            let normalized = message.to_ascii_lowercase();
            if normalized.contains("authrequired")
                || normalized.contains("auth required")
                || normalized.contains("authentication required")
                || normalized.contains("401 unauthorized")
            {
                Err(RuntimeError::McpAuthenticationRequired)
            } else {
                Err(RuntimeError::InvalidOption(message))
            }
        }
        Err(_) => Err(RuntimeError::InvalidOption(format!(
            "MCP initialization timed out after {} seconds",
            definition.startup_timeout_seconds
        ))),
    }
}

fn modern_protocol_versions() -> Vec<ProtocolVersion> {
    vec![
        ProtocolVersion::V_2026_07_28,
        ProtocolVersion::V_2025_11_25,
        ProtocolVersion::V_2025_06_18,
        ProtocolVersion::V_2025_03_26,
        ProtocolVersion::V_2024_11_05,
    ]
}

fn capture_stderr(mut stderr: tokio::process::ChildStderr, handler: CagentClientHandler) {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 1_024];
        while let Ok(read) = stderr.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            handler.push(String::from_utf8_lossy(&buffer[..read]));
        }
    });
}

#[allow(clippy::large_enum_variant)] // Transport definitions are consumed immediately after resolution.
enum ResolvedDefinition {
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: BTreeMap<String, String>,
        env_remove: Vec<String>,
        inherit_env: bool,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        oauth: Option<super::McpOAuthConfig>,
        bearer_token: Option<String>,
    },
}

impl ResolvedDefinition {
    fn resolve(
        workspace: &Path,
        definition: &McpServerDefinition,
        store: Option<&McpSecretStore>,
    ) -> Result<(Self, Redactor), RuntimeError> {
        let mut secrets = Vec::new();
        let resolved = match &definition.transport {
            McpTransportConfig::Stdio {
                command,
                args,
                cwd,
                env,
                env_remove,
                inherit_env,
            } => {
                let command = expand(command, workspace, &mut secrets, store)?;
                let args = args
                    .iter()
                    .map(|value| expand(value, workspace, &mut secrets, store))
                    .collect::<Result<Vec<_>, _>>()?;
                let cwd = cwd
                    .as_deref()
                    .map(|value| expand(value, workspace, &mut secrets, store))
                    .transpose()?;
                let env = env
                    .iter()
                    .map(|(name, value)| {
                        let value = if definition.package.is_some() {
                            expand_optional(value.value(), workspace, &mut secrets, store)?
                        } else {
                            Some(expand(value.value(), workspace, &mut secrets, store)?)
                        };
                        Ok(value.map(|value| {
                            if !value.is_empty() {
                                secrets.push(value.clone());
                            }
                            (name.clone(), value)
                        }))
                    })
                    .collect::<Result<Vec<_>, RuntimeError>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                Self::Stdio {
                    command,
                    args,
                    cwd,
                    env,
                    env_remove: env_remove.clone(),
                    inherit_env: *inherit_env,
                }
            }
            McpTransportConfig::StreamableHttp {
                url,
                headers,
                allow_insecure,
            } => {
                let url = expand(url, workspace, &mut secrets, store)?;
                let url_redactor = Redactor {
                    secrets: secrets.clone(),
                };
                super::validate_http_url(&url, *allow_insecure)
                    .map_err(|error| RuntimeError::InvalidOption(url_redactor.redact(&error)))?;
                let headers = headers
                    .iter()
                    .map(|(name, value)| {
                        let value = if definition.package.is_some() {
                            expand_optional(value.value(), workspace, &mut secrets, store)?
                        } else {
                            Some(expand(value.value(), workspace, &mut secrets, store)?)
                        };
                        Ok(value.map(|value| {
                            if !value.is_empty() {
                                secrets.push(value.clone());
                            }
                            (name.clone(), value)
                        }))
                    })
                    .collect::<Result<Vec<_>, RuntimeError>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                let bearer_token = definition
                    .oauth
                    .as_ref()
                    .and_then(|oauth| oauth.bearer_token.as_ref())
                    .map(|value| expand_optional(value.value(), workspace, &mut secrets, store))
                    .transpose()?
                    .flatten();
                Self::Http {
                    url,
                    headers,
                    oauth: definition.oauth.clone(),
                    bearer_token,
                }
            }
        };
        Ok((resolved, Redactor { secrets }))
    }
}

fn expand(
    source: &str,
    workspace: &Path,
    secrets: &mut Vec<String>,
    store: Option<&McpSecretStore>,
) -> Result<String, RuntimeError> {
    let mut output = source.replace("${workspace}", &workspace.to_string_lossy());
    while let Some(start) = output.find("${env:") {
        let rest = &output[start + 6..];
        let Some(end) = rest.find('}') else {
            return Err(RuntimeError::InvalidOption(
                "unterminated ${env:NAME} MCP reference".into(),
            ));
        };
        let name = &rest[..end];
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(RuntimeError::InvalidOption(format!(
                "invalid MCP environment reference: {name}"
            )));
        }
        let value = std::env::var(name).map_err(|_| {
            RuntimeError::InvalidOption(format!("missing MCP environment variable: {name}"))
        })?;
        if !value.is_empty() {
            secrets.push(value.clone());
        }
        output.replace_range(start..=(start + 6 + end), &value);
    }
    while let Some(start) = output.find("${secret:") {
        let rest = &output[start + 9..];
        let Some(end) = rest.find('}') else {
            return Err(RuntimeError::InvalidOption(
                "unterminated ${secret:mcp/...} MCP reference".into(),
            ));
        };
        let reference = &rest[..end];
        super::secrets::referenced_secrets(&output[start..=(start + 9 + end)])?;
        let value = store
            .ok_or_else(|| RuntimeError::InvalidOption("MCP secret storage is unavailable".into()))?
            .get(reference)?
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!("missing managed MCP secret: {reference}"))
            })?;
        secrets.push(value.clone());
        output.replace_range(start..=(start + 9 + end), &value);
    }
    Ok(output)
}

fn expand_optional(
    source: &str,
    workspace: &Path,
    secrets: &mut Vec<String>,
    store: Option<&McpSecretStore>,
) -> Result<Option<String>, RuntimeError> {
    if let Some(reference) = source
        .strip_prefix("${secret:")
        .and_then(|value| value.strip_suffix('}'))
    {
        super::secrets::referenced_secrets(source)?;
        let Some(value) = store
            .map(|store| store.get(reference))
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        secrets.push(value.clone());
        return Ok(Some(value));
    }
    expand(source, workspace, secrets, store).map(Some)
}

fn oci_command(
    definition: &McpServerDefinition,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    env_remove: &[String],
) -> Result<(String, Vec<String>), RuntimeError> {
    let runtime = definition.oci.runtime.clone().unwrap_or_else(|| {
        if std::process::Command::new("docker")
            .arg("version")
            .arg("--format")
            .arg("{{.Client.Version}}")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            "docker".into()
        } else {
            "podman".into()
        }
    });
    if runtime != "docker" && runtime != "podman" {
        return Err(RuntimeError::InvalidOption(
            "oci.runtime must be docker or podman".into(),
        ));
    }
    let image = definition.image.clone().ok_or_else(|| {
        RuntimeError::InvalidOption("OCI MCP definition requires an image".into())
    })?;
    let mut launch = vec![
        "run".into(),
        "--rm".into(),
        "--interactive".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        format!("--memory={}", definition.oci.memory),
        format!("--pids-limit={}", definition.oci.pids),
        format!(
            "--network={}",
            if definition.oci.network {
                "bridge"
            } else {
                "none"
            }
        ),
    ];
    if definition.oci.read_only_root {
        launch.push("--read-only".into());
        launch.extend(["--tmpfs".into(), "/tmp:rw,noexec,nosuid,size=64m".into()]);
    }
    for name in env_remove {
        if env.contains_key(name) {
            return Err(RuntimeError::InvalidOption(format!(
                "OCI MCP environment both sets and removes {name}"
            )));
        }
    }
    for name in env.keys() {
        launch.extend(["--env".into(), name.clone()]);
    }
    launch.push(image);
    if !command.is_empty() {
        launch.push(command.into());
    }
    launch.extend(args.iter().cloned());
    Ok((runtime, launch))
}

#[derive(Clone, Debug, Default)]
struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    fn redact(&self, source: &str) -> String {
        self.secrets
            .iter()
            .fold(source.to_owned(), |value, secret| {
                if secret.is_empty() {
                    value
                } else {
                    value.replace(secret, "[REDACTED]")
                }
            })
    }

    fn redact_value(&self, mut value: serde_json::Value) -> serde_json::Value {
        match &mut value {
            serde_json::Value::String(text) => *text = self.redact(text),
            serde_json::Value::Array(values) => {
                for value in values {
                    *value = self.redact_value(std::mem::take(value));
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values_mut() {
                    *value = self.redact_value(std::mem::take(value));
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
        value
    }
}

fn fingerprint(definition: &McpServerDefinition) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(definition).unwrap_or_default())
    )
}

#[must_use]
pub fn provider_safe_tool_name(server: &str, tool: &str) -> String {
    let sanitize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let candidate = format!("mcp__{}__{}", sanitize(server), sanitize(tool));
    if candidate.len() <= 64 {
        return candidate;
    }
    let hash = format!("{:x}", Sha256::digest(candidate.as_bytes()));
    let keep = 64 - 9;
    format!(
        "{}_{}",
        &candidate[..floor_char_boundary(&candidate, keep)],
        &hash[..8]
    )
}

fn unique_provider_name(server: &str, tool: &str, used: &mut HashSet<String>) -> String {
    let candidate = provider_safe_tool_name(server, tool);
    if used.insert(candidate.clone()) {
        return candidate;
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(format!("{server}\0{tool}").as_bytes())
    );
    let keep = 64 - 9;
    let name = format!(
        "{}_{}",
        &candidate[..floor_char_boundary(&candidate, keep.min(candidate.len()))],
        &hash[..8]
    );
    used.insert(name.clone());
    name
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn terminal_safe(source: &str, limit: usize) -> String {
    let safe = source
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>();
    if safe.len() <= limit {
        safe
    } else {
        let boundary = floor_char_boundary(&safe, limit);
        format!("{}… [truncated]", &safe[..boundary])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_startup() -> McpRegistryReadiness {
        McpRegistryReadiness::WaitUntil {
            turn_started_at: Instant::now(),
            cancellation: CancellationToken::new(),
        }
    }

    #[test]
    fn modern_protocol_versions_are_newest_first_and_complete() {
        assert_eq!(
            modern_protocol_versions(),
            vec![
                ProtocolVersion::V_2026_07_28,
                ProtocolVersion::V_2025_11_25,
                ProtocolVersion::V_2025_06_18,
                ProtocolVersion::V_2025_03_26,
                ProtocolVersion::V_2024_11_05,
            ]
        );
    }

    #[test]
    fn oci_arguments_name_environment_without_exposing_values() {
        let definition = McpServerDefinition {
            runner: McpRunner::Oci,
            image: Some("example.invalid/mcp@sha256:deadbeef".into()),
            ..McpServerDefinition::default()
        };
        let env = BTreeMap::from([("TOKEN".into(), "highly-secret".into())]);
        let (runtime, arguments) = oci_command(&definition, "server", &[], &env, &[]).unwrap();
        assert!(matches!(runtime.as_str(), "docker" | "podman"));
        assert!(
            arguments
                .windows(2)
                .any(|values| values == ["--env", "TOKEN"])
        );
        assert!(
            arguments
                .iter()
                .all(|value| !value.contains("highly-secret"))
        );
    }

    #[test]
    fn authentication_required_has_a_frontend_safe_status() {
        assert_eq!(
            McpRuntimeStatus::AuthenticationRequired.to_string(),
            "Login required"
        );
        assert_eq!(
            RuntimeError::McpAuthenticationRequired.to_string(),
            "MCP login required"
        );
    }

    const LEGACY_STDIO_FIXTURE: &str = r#"
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "server/discover":
        response = {"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Method not found","data":None}}
    elif method == "initialize":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"legacy-fixture","version":"1"}}}
    elif method == "tools/list":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"tools":[{"name":"echo","description":"Echo a value","inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}}]}}
    elif method == "tools/call":
        value = message.get("params", {}).get("arguments", {}).get("value", "")
        response = {"jsonrpc":"2.0","id":request_id,"result":{"content":[{"type":"text","text":value}],"structuredContent":{"echo":value},"isError":False}}
    else:
        continue
    print(json.dumps(response), flush=True)
"#;

    const ENV_STDIO_FIXTURE: &str = r#"
import json, os, sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "server/discover":
        response = {"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Method not found","data":None}}
    elif method == "initialize":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"env-fixture","version":"1"}}}
    elif method == "tools/list":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"tools":[{"name":"check_env","description":"Check environment ordering","inputSchema":{"type":"object"}}]}}
    elif method == "tools/call":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"content":[],"structuredContent":{"expanded":bool(os.environ.get("SOURCE_PATH")),"removed":"PATH" not in os.environ},"isError":False}}
    else:
        continue
    print(json.dumps(response), flush=True)
"#;

    const CRASH_STDIO_FIXTURE: &str = r#"
import json, os, pathlib, sys
flag = pathlib.Path(os.environ["CRASH_FLAG"])
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "server/discover":
        response = {"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Method not found","data":None}}
    elif method == "initialize":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"crash-fixture","version":"1"}}}
    elif method == "tools/list":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"tools":[{"name":"crash_once","description":"Crash once, then return","inputSchema":{"type":"object"}}]}}
    elif method == "tools/call" and not flag.exists():
        flag.write_text("crashed")
        sys.stderr.write(os.environ["FIXTURE_SECRET"] + ("x" * 20000))
        sys.stderr.flush()
        os._exit(17)
    elif method == "tools/call":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"content":[],"structuredContent":{"restarted":True},"isError":False}}
    else:
        continue
    print(json.dumps(response), flush=True)
"#;

    const SLOW_CATALOG_STDIO_FIXTURE: &str = r#"
import json, os, pathlib, sys, time
flag = pathlib.Path(os.environ["START_FLAG"])
with flag.open("a") as stream:
    stream.write("started\n")
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "server/discover":
        response = {"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Method not found","data":None}}
    elif method == "initialize":
        response = {"jsonrpc":"2.0","id":request_id,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"slow-catalog","version":"1"}}}
    elif method == "tools/list":
        time.sleep(float(os.environ["TOOLS_DELAY_SECONDS"]))
        response = {"jsonrpc":"2.0","id":request_id,"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}}
    else:
        continue
    print(json.dumps(response), flush=True)
"#;

    fn slow_catalog_definition(flag: &Path, delay_seconds: &str) -> McpServerDefinition {
        McpServerDefinition {
            transport: McpTransportConfig::Stdio {
                command: "python3".into(),
                args: vec!["-u".into(), "-c".into(), SLOW_CATALOG_STDIO_FIXTURE.into()],
                cwd: None,
                env: BTreeMap::from([
                    (
                        "START_FLAG".into(),
                        crate::McpConfiguredValue::from(flag.to_string_lossy().into_owned()),
                    ),
                    ("TOOLS_DELAY_SECONDS".into(), delay_seconds.into()),
                ]),
                env_remove: Vec::new(),
                inherit_env: true,
            },
            enabled: true,
            agents: Vec::new(),
            eager: false,
            startup_timeout_seconds: 1,
            request_timeout_seconds: 5,
            read_only_tools: vec!["echo".into()],
            ..McpServerDefinition::default()
        }
    }

    #[derive(Clone)]
    struct HttpFixture {
        expanded: Arc<std::sync::atomic::AtomicBool>,
    }

    impl rmcp::ServerHandler for HttpFixture {
        fn get_info(&self) -> rmcp::model::ServerInfo {
            rmcp::model::ServerInfo::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .enable_tool_list_changed()
                    .build(),
            )
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
            let mut tools = vec![rmcp::model::Tool::new(
                "structured",
                "Return structured content",
                serde_json::Map::from_iter([(
                    "type".into(),
                    serde_json::Value::String("object".into()),
                )]),
            )];
            if self.expanded.load(std::sync::atomic::Ordering::Acquire) {
                tools.push(rmcp::model::Tool::new(
                    "added",
                    "Added after list-changed",
                    serde_json::Map::from_iter([(
                        "type".into(),
                        serde_json::Value::String("object".into()),
                    )]),
                ));
            }
            Ok(rmcp::model::ListToolsResult {
                tools,
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: rmcp::model::CallToolRequestParams,
            context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            self.expanded
                .store(true, std::sync::atomic::Ordering::Release);
            context
                .peer
                .notify_tool_list_changed()
                .await
                .map_err(|error| rmcp::ErrorData::internal_error(error.to_string(), None))?;
            Ok(rmcp::model::CallToolResult::structured(serde_json::json!({
                "transport": "http"
            }))
            .into())
        }
    }

    #[derive(Clone)]
    struct SlowHttpFixture;

    impl rmcp::ServerHandler for SlowHttpFixture {
        fn get_info(&self) -> rmcp::model::ServerInfo {
            rmcp::model::ServerInfo::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
            Ok(rmcp::model::ListToolsResult {
                tools: vec![rmcp::model::Tool::new(
                    "slow",
                    "Wait before returning",
                    serde_json::Map::from_iter([(
                        "type".into(),
                        serde_json::Value::String("object".into()),
                    )]),
                )],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: rmcp::model::CallToolRequestParams,
            _context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Ok(rmcp::model::CallToolResult::success(Vec::new()).into())
        }
    }

    #[test]
    fn provider_names_are_sanitized_bounded_and_stable() {
        assert_eq!(
            provider_safe_tool_name("git hub", "issues/list"),
            "mcp__git_hub__issues_list"
        );
        let long = provider_safe_tool_name(&"server".repeat(20), &"tool".repeat(20));
        assert_eq!(long.len(), 64);
        assert_eq!(
            long,
            provider_safe_tool_name(&"server".repeat(20), &"tool".repeat(20))
        );
    }

    #[test]
    fn url_security_rejects_remote_http_but_allows_loopback_or_attestation() {
        assert!(super::super::validate_http_url("http://localhost:3000/mcp", false).is_ok());
        assert!(super::super::validate_http_url("http://127.0.0.1/mcp", false).is_ok());
        assert!(super::super::validate_http_url("http://example.test/mcp", false).is_err());
        assert!(super::super::validate_http_url("http://example.test/mcp", true).is_ok());
    }

    #[test]
    fn expansion_redacts_resolved_environment_values() {
        let mut secrets = Vec::new();
        let expanded = expand("${env:PATH}", Path::new("/workspace"), &mut secrets, None).unwrap();
        assert_eq!(Redactor { secrets }.redact(&expanded), "[REDACTED]");
    }

    #[test]
    fn package_runtime_omits_unconfigured_whole_secret_fields() {
        let definition = McpServerDefinition {
            transport: McpTransportConfig::Stdio {
                command: "server".into(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::from([
                    ("OPTIONAL_PAT".into(), "${secret:mcp/gitlab/pat}".into()),
                    ("STATIC".into(), "present".into()),
                ]),
                env_remove: Vec::new(),
                inherit_env: true,
            },
            package: Some("builtin:gitlab".into()),
            ..McpServerDefinition::default()
        };

        let (resolved, _) =
            ResolvedDefinition::resolve(Path::new("/workspace"), &definition, None).unwrap();
        let ResolvedDefinition::Stdio { env, .. } = resolved else {
            unreachable!()
        };
        assert_eq!(env.get("STATIC").map(String::as_str), Some("present"));
        assert!(!env.contains_key("OPTIONAL_PAT"));

        let mut custom = definition;
        custom.package = None;
        let error = ResolvedDefinition::resolve(Path::new("/workspace"), &custom, None)
            .err()
            .unwrap();
        assert!(error.to_string().contains("secret storage is unavailable"));
    }

    #[test]
    fn redaction_covers_nested_structured_results_and_diagnostics() {
        let redactor = Redactor {
            secrets: vec!["super-secret".into()],
        };
        assert_eq!(
            redactor.redact_value(serde_json::json!({
                "content": ["super-secret", {"nested": "Bearer super-secret"}]
            })),
            serde_json::json!({
                "content": ["[REDACTED]", {"nested": "Bearer [REDACTED]"}]
            })
        );
        let handler = CagentClientHandler::new(PathBuf::from("/workspace"));
        handler.set_redactor(redactor);
        handler.push("server logged super-secret");
        assert_eq!(handler.snapshot(), ["server logged [REDACTED]"]);
    }

    #[tokio::test]
    async fn rmcp_legacy_stdio_fixture_negotiates_discovers_and_invokes() {
        let definition = McpServerDefinition {
            transport: McpTransportConfig::Stdio {
                command: "python3".into(),
                args: vec!["-u".into(), "-c".into(), LEGACY_STDIO_FIXTURE.into()],
                cwd: None,
                env: BTreeMap::new(),
                env_remove: Vec::new(),
                inherit_env: true,
            },
            enabled: true,
            agents: Vec::new(),
            eager: false,
            startup_timeout_seconds: 5,
            request_timeout_seconds: 5,
            read_only_tools: vec!["echo".into()],
            ..McpServerDefinition::default()
        };
        let handler = CagentClientHandler::new(std::env::current_dir().unwrap());
        let client = connect(Path::new("."), &definition, handler, None)
            .await
            .unwrap();
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools[0].name, "echo");
        let result = client
            .call_tool(CallToolRequestParams::new("echo").with_arguments(
                serde_json::Map::from_iter([(
                    "value".into(),
                    serde_json::Value::String("stdio-ok".into()),
                )]),
            ))
            .await
            .unwrap();
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"echo":"stdio-ok"}))
        );
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn stdio_fixture_receives_connection_time_environment_and_remove_order() {
        let definition = McpServerDefinition {
            transport: McpTransportConfig::Stdio {
                command: "python3".into(),
                args: vec!["-u".into(), "-c".into(), ENV_STDIO_FIXTURE.into()],
                cwd: None,
                env: BTreeMap::from([("SOURCE_PATH".into(), "${env:PATH}".into())]),
                env_remove: vec!["PATH".into()],
                inherit_env: false,
            },
            enabled: true,
            agents: Vec::new(),
            eager: false,
            startup_timeout_seconds: 5,
            request_timeout_seconds: 5,
            read_only_tools: vec!["check_env".into()],
            ..McpServerDefinition::default()
        };
        let handler = CagentClientHandler::new(std::env::current_dir().unwrap());
        let client = connect(Path::new("."), &definition, handler, None)
            .await
            .unwrap();
        let result = client
            .call_tool(CallToolRequestParams::new("check_env"))
            .await
            .unwrap();
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"expanded":true,"removed":true}))
        );
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn rmcp_current_streamable_http_fixture_negotiates_discovers_and_invokes() {
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        };

        let cancellation = CancellationToken::new();
        let fixture = HttpFixture {
            expanded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let service_fixture = fixture.clone();
        let service: StreamableHttpService<HttpFixture, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(service_fixture.clone()),
                Arc::default(),
                StreamableHttpServerConfig::default()
                    .with_json_response(true)
                    .with_cancellation_token(cancellation.clone()),
            );
        let header_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let middleware_seen = header_seen.clone();
        let router =
            axum::Router::new()
                .nest_service("/mcp", service)
                .layer(axum::middleware::from_fn(
                    move |request: axum::extract::Request, next: axum::middleware::Next| {
                        let middleware_seen = middleware_seen.clone();
                        async move {
                            if request
                                .headers()
                                .get("x-cagent-fixture")
                                .and_then(|value| value.to_str().ok())
                                == Some("fixture-secret")
                            {
                                middleware_seen.store(true, std::sync::atomic::Ordering::Release);
                            }
                            next.run(request).await
                        }
                    },
                ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let definition = McpServerDefinition {
            transport: McpTransportConfig::StreamableHttp {
                url: format!("http://{address}/mcp"),
                headers: BTreeMap::from([("x-cagent-fixture".into(), "fixture-secret".into())]),
                allow_insecure: false,
            },
            enabled: true,
            agents: Vec::new(),
            eager: false,
            startup_timeout_seconds: 5,
            request_timeout_seconds: 5,
            read_only_tools: Vec::new(),
            ..McpServerDefinition::default()
        };
        let handler = CagentClientHandler::new(std::env::current_dir().unwrap());
        let observed_handler = handler.clone();
        let client = connect(Path::new("."), &definition, handler, None)
            .await
            .unwrap();
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools[0].name, "structured");
        let result = client
            .call_tool(CallToolRequestParams::new("structured"))
            .await
            .unwrap();
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"transport":"http"}))
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while !observed_handler
                .tools_changed
                .load(std::sync::atomic::Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            client
                .list_all_tools()
                .await
                .unwrap()
                .iter()
                .any(|tool| tool.name == "added")
        );
        assert!(header_seen.load(std::sync::atomic::Ordering::Acquire));
        client.cancel().await.unwrap();
        cancellation.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn supervisor_propagates_cancellation_and_timeout_without_restarting() {
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        };

        let cancellation = CancellationToken::new();
        let service: StreamableHttpService<SlowHttpFixture, LocalSessionManager> =
            StreamableHttpService::new(
                || Ok(SlowHttpFixture),
                Arc::default(),
                StreamableHttpServerConfig::default()
                    .with_json_response(true)
                    .with_cancellation_token(cancellation.clone()),
            );
        let router = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(
            &global,
            format!(
                "version = 1\n[mcp.servers.slow]\ntransport = \"http\"\nurl = \"http://{address}/mcp\"\nrequest_timeout_seconds = 1\n"
            ),
        )
        .unwrap();
        let config = McpConfigService::load(global, &workspace, true).unwrap();
        let supervisor = McpSupervisor::new();
        let registry = supervisor
            .pin_registry(&config, "general", wait_for_startup())
            .await;
        let provider_name = registry.tools[0].name.clone();

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = supervisor
            .call_pinned(&registry, &provider_name, serde_json::json!({}), &cancelled)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));

        let error = supervisor
            .call_pinned(
                &registry,
                &provider_name,
                serde_json::json!({}),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out after 1 seconds"));
        let target = registry.targets.get(&provider_name).unwrap();
        let runtime = supervisor
            .servers
            .lock()
            .await
            .get(&target.key)
            .cloned()
            .unwrap();
        assert!(!runtime.lock().await.restart_attempted);

        cancellation.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn registry_wait_is_concurrent_and_slow_startup_continues_in_background() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(&global, "version = 1\n").unwrap();
        let config = McpConfigService::load(global, &workspace, true).unwrap();
        let first_flag = temp.path().join("first-started");
        let second_flag = temp.path().join("second-started");
        let preview = config
            .preview_mutations(
                "general",
                vec![
                    super::super::McpMutation::Put {
                        location: super::super::McpLocation::global(),
                        name: "first".into(),
                        definition: slow_catalog_definition(&first_flag, "1.4"),
                    },
                    super::super::McpMutation::Put {
                        location: super::super::McpLocation::global(),
                        name: "second".into(),
                        definition: slow_catalog_definition(&second_flag, "1.4"),
                    },
                ],
            )
            .unwrap();
        config.apply_mutations("general", preview, false).unwrap();

        let supervisor = McpSupervisor::new();
        let started = Instant::now();
        let registry = supervisor
            .pin_registry(&config, "general", wait_for_startup())
            .await;
        assert!(registry.tools.is_empty());
        assert!(
            started.elapsed() < Duration::from_millis(1_800),
            "server readiness waits ran sequentially"
        );

        tokio::time::sleep(Duration::from_millis(700)).await;
        let started = Instant::now();
        let registry = supervisor
            .pin_registry(&config, "general", McpRegistryReadiness::ReadyOnly)
            .await;
        assert_eq!(registry.tools.len(), 2);
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(
            std::fs::read_to_string(first_flag).unwrap().lines().count(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(second_flag)
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn ready_only_pin_starts_server_without_waiting_or_cancelling_it() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(&global, "version = 1\n").unwrap();
        let config = McpConfigService::load(global, &workspace, true).unwrap();
        let flag = temp.path().join("started");
        let preview = config
            .preview_mutations(
                "general",
                vec![super::super::McpMutation::Put {
                    location: super::super::McpLocation::global(),
                    name: "slow".into(),
                    definition: slow_catalog_definition(&flag, "0.4"),
                }],
            )
            .unwrap();
        config.apply_mutations("general", preview, false).unwrap();

        let supervisor = McpSupervisor::new();
        let started = Instant::now();
        let registry = supervisor
            .pin_registry(&config, "general", McpRegistryReadiness::ReadyOnly)
            .await;
        assert!(registry.tools.is_empty());
        assert!(started.elapsed() < Duration::from_millis(250));

        tokio::time::sleep(Duration::from_millis(600)).await;
        let registry = supervisor
            .pin_registry(&config, "general", McpRegistryReadiness::ReadyOnly)
            .await;
        assert_eq!(registry.tools.len(), 1);
        assert_eq!(std::fs::read_to_string(flag).unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn stdio_crash_restarts_once_and_keeps_stderr_bounded_and_redacted() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(&global, "version = 1\n").unwrap();
        let config = McpConfigService::load(global, &workspace, true).unwrap();
        let definition = McpServerDefinition {
            transport: McpTransportConfig::Stdio {
                command: "python3".into(),
                args: vec!["-u".into(), "-c".into(), CRASH_STDIO_FIXTURE.into()],
                cwd: None,
                env: BTreeMap::from([
                    (
                        "CRASH_FLAG".into(),
                        crate::McpConfiguredValue::from(
                            temp.path()
                                .join("crashed.flag")
                                .to_string_lossy()
                                .into_owned(),
                        ),
                    ),
                    ("FIXTURE_SECRET".into(), "crash-secret".into()),
                ]),
                env_remove: Vec::new(),
                inherit_env: true,
            },
            enabled: true,
            agents: Vec::new(),
            eager: false,
            startup_timeout_seconds: 5,
            request_timeout_seconds: 5,
            read_only_tools: Vec::new(),
            ..McpServerDefinition::default()
        };
        let preview = config
            .preview_mutations(
                "general",
                vec![super::super::McpMutation::Put {
                    location: super::super::McpLocation::global(),
                    name: "crasher".into(),
                    definition,
                }],
            )
            .unwrap();
        config.apply_mutations("general", preview, false).unwrap();

        let supervisor = McpSupervisor::new();
        let registry = supervisor
            .pin_registry(&config, "general", wait_for_startup())
            .await;
        let provider_name = registry.tools[0].name.clone();
        let result = supervisor
            .call_pinned(
                &registry,
                &provider_name,
                serde_json::json!({}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"restarted":true}))
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let server = supervisor
            .describe(&config, "general")
            .await
            .unwrap()
            .remove(0);
        assert_eq!(server.status, McpRuntimeStatus::Connected);
        assert!(server.generation > 0);
        assert!(server.diagnostics.len() <= DIAGNOSTIC_ITEMS);
        assert!(server.diagnostics.iter().map(String::len).sum::<usize>() <= DIAGNOSTIC_BYTES);
        assert!(
            server
                .diagnostics
                .iter()
                .all(|item| !item.contains("crash-secret"))
        );
        let target = registry.targets.get(&provider_name).unwrap();
        let runtime = supervisor
            .servers
            .lock()
            .await
            .get(&target.key)
            .cloned()
            .unwrap();
        assert!(runtime.lock().await.restart_attempted);
    }

    #[tokio::test]
    async fn pinned_registry_survives_mid_request_removal_and_next_boundary_drops_tool() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(&global, "version = 1\n").unwrap();
        let config = McpConfigService::load(global, &workspace, true).unwrap();
        let definition = McpServerDefinition {
            transport: McpTransportConfig::Stdio {
                command: "python3".into(),
                args: vec!["-u".into(), "-c".into(), LEGACY_STDIO_FIXTURE.into()],
                cwd: None,
                env: BTreeMap::new(),
                env_remove: Vec::new(),
                inherit_env: true,
            },
            enabled: true,
            agents: vec!["general".into()],
            eager: false,
            startup_timeout_seconds: 5,
            request_timeout_seconds: 5,
            read_only_tools: vec!["echo".into()],
            ..McpServerDefinition::default()
        };
        let preview = config
            .preview_mutations(
                "general",
                vec![super::super::McpMutation::Put {
                    location: super::super::McpLocation::global(),
                    name: "fixture".into(),
                    definition,
                }],
            )
            .unwrap();
        config.apply_mutations("general", preview, false).unwrap();
        let supervisor = McpSupervisor::new();
        let pinned = supervisor
            .pin_registry(&config, "general", wait_for_startup())
            .await;
        let provider_name = pinned.tools[0].name.clone();
        assert!(
            supervisor
                .pin_registry(&config, "explore", wait_for_startup())
                .await
                .tools
                .is_empty()
        );
        assert_eq!(supervisor.servers.lock().await.len(), 1);

        let removal = config
            .preview_mutations(
                "general",
                vec![super::super::McpMutation::Remove {
                    location: super::super::McpLocation::global(),
                    name: "fixture".into(),
                }],
            )
            .unwrap();
        config.apply_mutations("general", removal, false).unwrap();
        let result = supervisor
            .call_pinned(
                &pinned,
                &provider_name,
                serde_json::json!({"value":"still-pinned"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"echo":"still-pinned"}))
        );
        assert!(
            supervisor
                .pin_registry(&config, "general", wait_for_startup())
                .await
                .tools
                .is_empty()
        );
        assert!(supervisor.servers.lock().await.is_empty());
        // The pinned turn owns its runtime even after the shared registry is
        // reconciled, so configuration reloads cannot invalidate a live turn.
        supervisor
            .call_pinned(
                &pinned,
                &provider_name,
                serde_json::json!({"value":"still-owned"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
    }
}
