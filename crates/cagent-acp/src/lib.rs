//! Zed-compatible Agent Client Protocol frontend for Cagent.

use agent_client_protocol as acp_sdk;
use agent_client_protocol::schema::v1 as acp;
use base64::Engine as _;
use cagent_agent::config::{AppPaths, ConfigStore, PathOverrides};
use cagent_agent::protocol::{
    AttachmentSpec, DurableEventKind, ImageChipRange, InteractionRequestKind, NodeKind,
    QueueTarget, RuntimeEvent, SessionAction, SessionCommand, TransientEvent, UserDraft,
};
use cagent_agent::runtime::{AgentRuntime, NewSession, RuntimeOptions, SessionHandle};
use futures_util::StreamExt as _;
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct Options {
    pub config_file: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub temporary_workspace_trust: bool,
}

enum ClientCommand {
    Notify(acp::SessionNotification, oneshot::Sender<()>),
    Permission(
        acp::RequestPermissionRequest,
        oneshot::Sender<acp::Result<acp::RequestPermissionResponse>>,
    ),
    Elicit(
        acp::CreateElicitationRequest,
        oneshot::Sender<acp::Result<acp::CreateElicitationResponse>>,
    ),
    WriteTextFile(
        acp::WriteTextFileRequest,
        oneshot::Sender<acp::Result<acp::WriteTextFileResponse>>,
    ),
    CreateTerminal(
        acp::CreateTerminalRequest,
        oneshot::Sender<acp::Result<acp::CreateTerminalResponse>>,
    ),
    TerminalOutput(
        acp::TerminalOutputRequest,
        oneshot::Sender<acp::Result<acp::TerminalOutputResponse>>,
    ),
    WaitTerminal(
        acp::WaitForTerminalExitRequest,
        oneshot::Sender<acp::Result<acp::WaitForTerminalExitResponse>>,
    ),
    ReleaseTerminal(acp::ReleaseTerminalRequest),
    KillTerminal(
        acp::KillTerminalRequest,
        oneshot::Sender<acp::Result<acp::KillTerminalResponse>>,
    ),
}

struct AcpFileSystemBackend {
    session_id: acp::SessionId,
    client: mpsc::UnboundedSender<ClientCommand>,
}

impl cagent_agent::frontend::FrontendFileSystem for AcpFileSystemBackend {
    fn write_text_file(
        &self,
        request: cagent_agent::frontend::FrontendWriteTextFileRequest,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            let (sent, received) = oneshot::channel();
            self.client
                .send(ClientCommand::WriteTextFile(
                    acp::WriteTextFileRequest::new(
                        self.session_id.clone(),
                        request.path,
                        request.content,
                    ),
                    sent,
                ))
                .map_err(|_| "ACP filesystem connection closed".to_owned())?;
            tokio::select! {
                response = received => response
                    .map_err(|_| "ACP file write was cancelled".to_owned())?
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                () = cancellation.cancelled() => Err("file write cancelled".to_owned()),
            }
        })
    }
}

struct AcpTerminalBackend {
    session_id: acp::SessionId,
    client: mpsc::UnboundedSender<ClientCommand>,
    tool_terminals: Arc<Mutex<HashMap<(acp::SessionId, String), acp::TerminalId>>>,
}

impl cagent_agent::frontend::FrontendTerminal for AcpTerminalBackend {
    fn execute(
        &self,
        request: cagent_agent::frontend::FrontendTerminalRequest,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<cagent_agent::frontend::FrontendTerminalResult, String>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let (sent, received) = oneshot::channel();
            self.client
                .send(ClientCommand::CreateTerminal(
                    acp::CreateTerminalRequest::new(self.session_id.clone(), "sh")
                        .args(vec!["-lc".into(), request.command])
                        .cwd(request.cwd)
                        .env(
                            request
                                .env
                                .into_iter()
                                .map(|(name, value)| acp::EnvVariable::new(name, value))
                                .collect(),
                        )
                        .output_byte_limit(request.output_byte_limit),
                    sent,
                ))
                .map_err(|_| "ACP terminal connection closed".to_owned())?;
            let terminal = received
                .await
                .map_err(|_| "ACP terminal creation was cancelled".to_owned())?
                .map_err(|error| error.to_string())?
                .terminal_id;
            self.tool_terminals.lock().await.insert(
                (self.session_id.clone(), request.tool_call_id.clone()),
                terminal.clone(),
            );
            self.client
                .send(ClientCommand::Notify(
                    acp::SessionNotification::new(
                        self.session_id.clone(),
                        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                            request.tool_call_id,
                            acp::ToolCallUpdateFields::new().content(vec![
                                acp::ToolCallContent::Terminal(acp::Terminal::new(
                                    terminal.clone(),
                                )),
                            ]),
                        )),
                    ),
                    oneshot::channel().0,
                ))
                .map_err(|_| "ACP terminal connection closed".to_owned())?;
            async {
                let (sent, received) = oneshot::channel();
                self.client
                    .send(ClientCommand::WaitTerminal(
                        acp::WaitForTerminalExitRequest::new(
                            self.session_id.clone(),
                            terminal.clone(),
                        ),
                        sent,
                    ))
                    .map_err(|_| "ACP terminal connection closed".to_owned())?;
                let wait = tokio::select! {
                    result = received => result.map_err(|_| "ACP terminal wait was cancelled".to_owned())?.map_err(|error| error.to_string()),
                    () = cancellation.cancelled() => {
                        let (sent, received) = oneshot::channel();
                        self.client.send(ClientCommand::KillTerminal(
                            acp::KillTerminalRequest::new(self.session_id.clone(), terminal.clone()), sent
                        )).map_err(|_| "ACP terminal connection closed".to_owned())?;
                        received.await.map_err(|_| "ACP terminal kill was cancelled".to_owned())?.map_err(|error| error.to_string())?;
                        Err("terminal command cancelled".to_owned())
                    }
                }?;
                let (sent, received) = oneshot::channel();
                self.client
                    .send(ClientCommand::TerminalOutput(
                        acp::TerminalOutputRequest::new(
                            self.session_id.clone(),
                            terminal.clone(),
                        ),
                        sent,
                    ))
                    .map_err(|_| "ACP terminal connection closed".to_owned())?;
                let output = received
                    .await
                    .map_err(|_| "ACP terminal output was cancelled".to_owned())?
                    .map_err(|error| error.to_string())?;
                let status = output.exit_status.unwrap_or(wait.exit_status);
                Ok(cagent_agent::frontend::FrontendTerminalResult {
                    output: output.output,
                    exit_code: status.exit_code,
                    signal: status.signal,
                    truncated: output.truncated,
                })
            }
            .await
        })
    }
}

struct CagentAcp {
    runtime: AgentRuntime,
    sessions: Mutex<HashMap<acp::SessionId, SessionHandle>>,
    client: mpsc::UnboundedSender<ClientCommand>,
    permissions_file: PathBuf,
    temporary_workspace_trust: bool,
    client_capabilities: Mutex<acp::ClientCapabilities>,
    session_directories: Mutex<HashMap<acp::SessionId, Vec<PathBuf>>>,
    tool_terminals: Arc<Mutex<HashMap<(acp::SessionId, String), acp::TerminalId>>>,
}

impl CagentAcp {
    fn error(error: impl std::fmt::Display) -> acp::Error {
        tracing::error!(%error, "ACP request failed");
        acp::Error::internal_error()
    }

    async fn notify(
        &self,
        session_id: acp::SessionId,
        update: acp::SessionUpdate,
    ) -> acp::Result<()> {
        let (sent, received) = oneshot::channel();
        self.client
            .send(ClientCommand::Notify(
                acp::SessionNotification::new(session_id, update),
                sent,
            ))
            .map_err(|_| acp::Error::internal_error())?;
        received.await.map_err(|_| acp::Error::internal_error())
    }

    async fn request_permission(
        &self,
        request: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let (sent, received) = oneshot::channel();
        self.client
            .send(ClientCommand::Permission(request, sent))
            .map_err(|_| acp::Error::internal_error())?;
        received.await.map_err(|_| acp::Error::internal_error())?
    }

    async fn elicit(
        &self,
        request: acp::CreateElicitationRequest,
    ) -> acp::Result<acp::CreateElicitationResponse> {
        let (sent, received) = oneshot::channel();
        self.client
            .send(ClientCommand::Elicit(request, sent))
            .map_err(|_| acp::Error::internal_error())?;
        received.await.map_err(|_| acp::Error::internal_error())?
    }

    async fn session(&self, id: &acp::SessionId) -> acp::Result<SessionHandle> {
        self.sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(acp::Error::internal_error)
    }

    async fn install_frontend_services(
        &self,
        session_id: &acp::SessionId,
        session: &SessionHandle,
    ) -> acp::Result<()> {
        let capabilities = self.client_capabilities.lock().await.clone();
        if capabilities.fs.write_text_file {
            session
                .install_frontend_file_system(Arc::new(AcpFileSystemBackend {
                    session_id: session_id.clone(),
                    client: self.client.clone(),
                }))
                .map_err(Self::error)?;
        }
        if capabilities.terminal {
            session
                .install_frontend_terminal(Arc::new(AcpTerminalBackend {
                    session_id: session_id.clone(),
                    client: self.client.clone(),
                    tool_terminals: Arc::clone(&self.tool_terminals),
                }))
                .map_err(Self::error)?;
        }
        Ok(())
    }

    async fn config_options(
        &self,
        session: &SessionHandle,
    ) -> acp::Result<Vec<acp::SessionConfigOption>> {
        let (active_agent, active_mode) = session.active_profiles().await.map_err(Self::error)?;
        let agents = session
            .agent_profiles()
            .map_err(Self::error)?
            .into_iter()
            .map(|profile| {
                acp::SessionConfigSelectOption::new(profile.name.clone(), profile.name)
                    .description(profile.description)
            })
            .collect::<Vec<_>>();
        let modes = session
            .mode_profiles()
            .map_err(Self::error)?
            .into_iter()
            .map(|mode| {
                acp::SessionConfigSelectOption::new(mode.name.clone(), mode.name)
                    .description(mode.description)
            })
            .collect::<Vec<_>>();

        let current_model = session.model_selection().await.map_err(Self::error)?;
        let current_model_id = current_model
            .as_ref()
            .map(|(provider, model, _)| format!("{provider}/{model}"));
        let mut model_groups = Vec::new();
        for provider in session
            .providers()
            .await
            .into_iter()
            .filter(|provider| provider.enabled)
        {
            let id = provider.descriptor.id;
            let Ok(catalog) = session.models(&id).await else {
                continue;
            };
            let models = catalog
                .catalog
                .models
                .into_iter()
                .map(|model| {
                    acp::SessionConfigSelectOption::new(
                        format!("{id}/{}", model.id),
                        model.display_name,
                    )
                })
                .collect::<Vec<_>>();
            if !models.is_empty() {
                model_groups.push(acp::SessionConfigSelectGroup::new(
                    id,
                    provider.descriptor.display_name,
                    models,
                ));
            }
        }

        let mut options = vec![
            acp::SessionConfigOption::select("agent", "Agent", active_agent, agents)
                .category(acp::SessionConfigOptionCategory::Other("_agent".into())),
            acp::SessionConfigOption::select("mode", "Mode", active_mode, modes)
                .category(acp::SessionConfigOptionCategory::Mode),
        ];
        if let Some(current_model_id) = current_model_id {
            options.push(
                acp::SessionConfigOption::select("model", "Model", current_model_id, model_groups)
                    .category(acp::SessionConfigOptionCategory::Model),
            );
        }
        if let Some((provider, model, effort)) = current_model
            && let Some(efforts) = session.models(&provider).await.ok().and_then(|catalog| {
                catalog
                    .catalog
                    .models
                    .into_iter()
                    .find(|candidate| candidate.id == model)
                    .and_then(|model| model.capabilities.reasoning_efforts)
            })
            && !efforts.is_empty()
        {
            let current = effort
                .filter(|effort| efforts.contains(effort))
                .unwrap_or_else(|| efforts[0].clone());
            options.push(
                acp::SessionConfigOption::select(
                    "effort",
                    "Reasoning effort",
                    current,
                    efforts
                        .into_iter()
                        .map(|effort| acp::SessionConfigSelectOption::new(effort.clone(), effort))
                        .collect::<Vec<_>>(),
                )
                .category(acp::SessionConfigOptionCategory::ThoughtLevel),
            );
        }
        Ok(options)
    }

    fn mode_state(session: &SessionHandle, current: String) -> acp::Result<acp::SessionModeState> {
        let modes = session
            .mode_profiles()
            .map_err(Self::error)?
            .into_iter()
            .map(|mode| {
                acp::SessionMode::new(mode.name.clone(), mode.name).description(mode.description)
            })
            .collect();
        Ok(acp::SessionModeState::new(current, modes))
    }
}

#[allow(clippy::too_many_lines)]
impl CagentAcp {
    async fn initialize(
        &self,
        request: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        *self.client_capabilities.lock().await = request.client_capabilities;
        Ok(
            acp::InitializeResponse::new(negotiated_protocol_version(request.protocol_version))
                .agent_capabilities(
                    acp::AgentCapabilities::new()
                        .load_session(true)
                        .session_capabilities(
                            acp::SessionCapabilities::new()
                                .list(acp::SessionListCapabilities::new())
                                .delete(acp::SessionDeleteCapabilities::new())
                                .resume(acp::SessionResumeCapabilities::new())
                                .close(acp::SessionCloseCapabilities::new())
                                .additional_directories(
                                    acp::SessionAdditionalDirectoriesCapabilities::new(),
                                ),
                        )
                        .mcp_capabilities(acp::McpCapabilities::new().http(true))
                        .prompt_capabilities(
                            acp::PromptCapabilities::new()
                                .image(true)
                                .embedded_context(true),
                        ),
                )
                .agent_info(
                    acp::Implementation::new("cagent", env!("CARGO_PKG_VERSION")).title("Cagent"),
                ),
        )
    }

    async fn authenticate(
        &self,
        _request: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        Err(acp::Error::invalid_params())
    }

    async fn logout(&self, _request: acp::LogoutRequest) -> acp::Result<acp::LogoutResponse> {
        Ok(acp::LogoutResponse::new())
    }

    async fn list_sessions(
        &self,
        request: acp::ListSessionsRequest,
    ) -> acp::Result<acp::ListSessionsResponse> {
        const PAGE_SIZE: usize = 100;
        let offset = request
            .cursor
            .as_deref()
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| acp::Error::invalid_params())?
            .unwrap_or_default();
        let directories = self.session_directories.lock().await;
        let conversations = self
            .runtime
            .conversations(request.cwd.as_deref())
            .await
            .map_err(Self::error)?;
        if offset > conversations.len() {
            return Err(acp::Error::invalid_params());
        }
        let total = conversations.len();
        let end = (offset + PAGE_SIZE).min(total);
        let sessions = conversations
            .into_iter()
            .skip(offset)
            .take(PAGE_SIZE)
            .map(|session| {
                let id = acp::SessionId::new(session.id.to_string());
                acp::SessionInfo::new(id.clone(), session.workspace)
                    .additional_directories(directories.get(&id).cloned().unwrap_or_default())
                    .title(session.title)
                    .updated_at(session.updated_at)
            })
            .collect();
        Ok(acp::ListSessionsResponse::new(sessions)
            .next_cursor((end < total).then(|| end.to_string())))
    }

    async fn delete_session(
        &self,
        request: acp::DeleteSessionRequest,
    ) -> acp::Result<acp::DeleteSessionResponse> {
        let conversation_id = request
            .session_id
            .to_string()
            .parse()
            .map_err(Self::error)?;
        self.sessions.lock().await.remove(&request.session_id);
        self.session_directories
            .lock()
            .await
            .remove(&request.session_id);
        self.runtime
            .delete_conversation(conversation_id)
            .await
            .map_err(Self::error)?;
        Ok(acp::DeleteSessionResponse::new())
    }

    async fn resume_session(
        &self,
        request: acp::ResumeSessionRequest,
    ) -> acp::Result<acp::ResumeSessionResponse> {
        let additional_directories = canonical_directories(request.additional_directories)?;
        let mcp_servers = mcp_servers(request.mcp_servers)?;
        let conversation_id = request
            .session_id
            .to_string()
            .parse()
            .map_err(Self::error)?;
        let matches_workspace = self
            .runtime
            .conversations(Some(&request.cwd))
            .await
            .map_err(Self::error)?
            .iter()
            .any(|summary| summary.id == conversation_id);
        if !matches_workspace {
            return Err(acp::Error::invalid_params());
        }
        let session = self
            .runtime
            .resume_session(conversation_id)
            .await
            .map_err(Self::error)?;
        session
            .install_additional_directories(&additional_directories)
            .map_err(Self::error)?;
        session
            .install_session_mcp_servers(mcp_servers)
            .await
            .map_err(Self::error)?;
        let (_, mode) = session.active_profiles().await.map_err(Self::error)?;
        let modes = Self::mode_state(&session, mode)?;
        let config_options = self.config_options(&session).await?;
        self.install_frontend_services(&request.session_id, &session)
            .await?;
        self.sessions
            .lock()
            .await
            .insert(request.session_id.clone(), session);
        self.session_directories
            .lock()
            .await
            .insert(request.session_id.clone(), additional_directories);
        Ok(acp::ResumeSessionResponse::new()
            .modes(modes)
            .config_options(config_options))
    }

    async fn close_session(
        &self,
        request: acp::CloseSessionRequest,
    ) -> acp::Result<acp::CloseSessionResponse> {
        let session = self
            .sessions
            .lock()
            .await
            .remove(&request.session_id)
            .ok_or_else(acp::Error::invalid_params)?;
        self.session_directories
            .lock()
            .await
            .remove(&request.session_id);
        session.end().await.map_err(Self::error)?;
        Ok(acp::CloseSessionResponse::new())
    }

    async fn new_session(
        &self,
        request: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let workspace = request.cwd.canonicalize().map_err(Self::error)?;
        let additional_directories = canonical_directories(request.additional_directories)?;
        let mcp_servers = mcp_servers(request.mcp_servers)?;
        let permissions = cagent_agent::permissions::PermissionFile::new(
            self.permissions_file.clone(),
            &workspace,
        )
        .map_err(Self::error)?;
        if !self.temporary_workspace_trust && !permissions.is_trusted().map_err(Self::error)? {
            tracing::warn!(workspace = %workspace.display(), "Zed requested an untrusted workspace");
            return Err(acp::Error::internal_error());
        }
        let session = self
            .runtime
            .create_frontend_session(
                NewSession { workspace },
                additional_directories.clone(),
                mcp_servers,
            )
            .await
            .map_err(Self::error)?;
        session.wait_for_startup_resources().await;
        let id = acp::SessionId::new(session.id().to_string());
        let (_, mode) = session.active_profiles().await.map_err(Self::error)?;
        let modes = Self::mode_state(&session, mode)?;
        let config_options = self.config_options(&session).await?;
        self.install_frontend_services(&id, &session).await?;
        self.sessions.lock().await.insert(id.clone(), session);
        self.session_directories
            .lock()
            .await
            .insert(id.clone(), additional_directories);
        Ok(acp::NewSessionResponse::new(id)
            .modes(modes)
            .config_options(config_options))
    }

    async fn load_session(
        &self,
        request: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        let additional_directories = canonical_directories(request.additional_directories)?;
        let mcp_servers = mcp_servers(request.mcp_servers)?;
        let conversation_id = request
            .session_id
            .to_string()
            .parse()
            .map_err(Self::error)?;
        if !self
            .runtime
            .conversations(Some(&request.cwd))
            .await
            .map_err(Self::error)?
            .iter()
            .any(|summary| summary.id == conversation_id)
        {
            return Err(acp::Error::invalid_params());
        }
        let session = self
            .runtime
            .resume_session(conversation_id)
            .await
            .map_err(Self::error)?;
        session
            .install_additional_directories(&additional_directories)
            .map_err(Self::error)?;
        session
            .install_session_mcp_servers(mcp_servers)
            .await
            .map_err(Self::error)?;
        let (_, mode) = session.active_profiles().await.map_err(Self::error)?;
        let modes = Self::mode_state(&session, mode)?;
        let config_options = self.config_options(&session).await?;
        self.install_frontend_services(&request.session_id, &session)
            .await?;
        self.sessions
            .lock()
            .await
            .insert(request.session_id.clone(), session.clone());
        self.session_directories
            .lock()
            .await
            .insert(request.session_id.clone(), additional_directories);
        self.replay_history(&request.session_id, &session).await?;
        Ok(acp::LoadSessionResponse::new()
            .modes(modes)
            .config_options(config_options))
    }

    async fn replay_history(
        &self,
        session_id: &acp::SessionId,
        session: &SessionHandle,
    ) -> acp::Result<()> {
        let mut tools = HashMap::new();
        for node in session.history().await.map_err(Self::error)? {
            let message_id = node.id.to_string();
            let update = match node.kind {
                NodeKind::UserMessage => node
                    .composer_text
                    .or_else(|| text_field(&node.content, "display_text"))
                    .or_else(|| text_field(&node.content, "text"))
                    .map(|text| text_chunk(text, &message_id))
                    .map(acp::SessionUpdate::UserMessageChunk),
                NodeKind::AssistantMessage => text_field(&node.content, "text")
                    .map(|text| text_chunk(text, &message_id))
                    .map(acp::SessionUpdate::AgentMessageChunk),
                NodeKind::ToolCall => {
                    let id =
                        text_field(&node.content, "call_id").unwrap_or_else(|| node.id.to_string());
                    let name = text_field(&node.content, "name").unwrap_or_else(|| "tool".into());
                    let arguments = node.content.get("arguments").cloned();
                    tools.insert(node.id, id.clone());
                    Some(acp::SessionUpdate::ToolCall(
                        acp::ToolCall::new(id, tool_title(&name, arguments.as_ref()))
                            .kind(tool_kind(&name, arguments.as_ref()))
                            .status(acp::ToolCallStatus::Completed)
                            .locations(tool_locations(arguments.as_ref()))
                            .raw_input(arguments),
                    ))
                }
                NodeKind::ToolResult => {
                    node.owner_id.and_then(|owner| tools.get(&owner)).map(|id| {
                        let failed = node
                            .content
                            .get("is_error")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let output = node
                            .content
                            .get("output")
                            .or_else(|| node.content.get("model_output"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                            id.clone(),
                            acp::ToolCallUpdateFields::new()
                                .status(if failed {
                                    acp::ToolCallStatus::Failed
                                } else {
                                    acp::ToolCallStatus::Completed
                                })
                                .content(tool_output_content(&output))
                                .raw_output(output),
                        ))
                    })
                }
                NodeKind::AcceptedPlan => text_field(&node.content, "plan_markdown").map(|plan| {
                    acp::SessionUpdate::Plan(acp::Plan::new(vec![acp::PlanEntry::new(
                        plan,
                        acp::PlanEntryPriority::High,
                        acp::PlanEntryStatus::Completed,
                    )]))
                }),
                _ => None,
            };
            if let Some(update) = update {
                self.notify(session_id.clone(), update).await?;
            }
        }
        Ok(())
    }

    async fn send_available_commands(&self, session_id: &acp::SessionId) -> acp::Result<()> {
        let session = self.session(session_id).await?;
        self.notify(
            session_id.clone(),
            available_commands_update(&session.skills()),
        )
        .await
    }

    async fn prompt(&self, request: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let session = self.session(&request.session_id).await?;
        let mut draft = prompt_draft(&session, &request.prompt).await?;
        let mut events = session.subscribe(None);
        let mut cancelled = false;
        let mut tool_calls = HashMap::new();
        let mut plan = String::new();
        if let Some(command) = parse_command(&draft.text) {
            match command.name {
                "compact" => {
                    session
                        .submit(SessionCommand::new(SessionAction::Compact {
                            target: QueueTarget::NextBoundary,
                            instructions: (!command.arguments.is_empty())
                                .then(|| command.arguments.to_owned()),
                        }))
                        .await
                        .map_err(Self::error)?;
                }
                "recap" if command.arguments.is_empty() => {
                    session
                        .submit(SessionCommand::new(SessionAction::Recap))
                        .await
                        .map_err(Self::error)?;
                    return Ok(acp::PromptResponse::new(acp::StopReason::EndTurn));
                }
                "retry" if command.arguments.is_empty() => {
                    session
                        .submit(SessionCommand::new(SessionAction::Retry))
                        .await
                        .map_err(Self::error)?;
                }
                "spawn" | "web-search" if !command.arguments.is_empty() => {
                    let text = command.arguments.to_owned();
                    let attachments = std::mem::take(&mut draft.attachment_specs);
                    let action = match (command.name, attachments.is_empty()) {
                        ("spawn", true) => SessionAction::SubmitSpawnInput { text },
                        ("spawn", false) => {
                            SessionAction::SubmitSpawnWithAttachments { text, attachments }
                        }
                        ("web-search", true) => SessionAction::SubmitWebSearchInput { text },
                        ("web-search", false) => {
                            SessionAction::SubmitWebSearchWithAttachments { text, attachments }
                        }
                        _ => unreachable!(),
                    };
                    session
                        .submit(SessionCommand::new(action))
                        .await
                        .map_err(Self::error)?;
                }
                "read" | "edit" | "auto" | "plan" if !command.arguments.is_empty() => {
                    session
                        .set_session_mode(command.name)
                        .await
                        .map_err(Self::error)?;
                    draft.text = command.arguments.to_owned();
                    session
                        .submit(SessionCommand::new(SessionAction::SubmitDraft { draft }))
                        .await
                        .map_err(Self::error)?;
                }
                _ => {
                    if let Some(prompt) = session
                        .resolve_skill_command(command.name, command.arguments)
                        .map_err(Self::error)?
                    {
                        draft.text = prompt;
                    }
                    session
                        .submit(SessionCommand::new(SessionAction::SubmitDraft { draft }))
                        .await
                        .map_err(Self::error)?;
                }
            }
        } else {
            session
                .submit(SessionCommand::new(SessionAction::SubmitDraft { draft }))
                .await
                .map_err(Self::error)?;
        }
        while let Some(event) = events.next().await {
            match event.map_err(Self::error)? {
                RuntimeEvent::Durable(event) => match event.kind {
                    DurableEventKind::AssistantDelta { node_id, delta, .. } => {
                        self.notify(
                            request.session_id.clone(),
                            acp::SessionUpdate::AgentMessageChunk(text_chunk(
                                delta,
                                &node_id.to_string(),
                            )),
                        )
                        .await?;
                    }
                    DurableEventKind::AssistantFailed { message, .. } => {
                        tracing::error!(%message, "Cagent turn failed");
                        return Err(acp::Error::internal_error());
                    }
                    DurableEventKind::NodeAppended {
                        node_id,
                        owner_id,
                        node_kind: NodeKind::ToolCall,
                        content,
                        ..
                    } => {
                        let id = content
                            .get("call_id")
                            .and_then(serde_json::Value::as_str)
                            .map_or_else(|| node_id.to_string(), str::to_owned);
                        let name = content
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("tool")
                            .to_owned();
                        let arguments = content.get("arguments").cloned();
                        tool_calls.insert(node_id, id.clone());
                        self.notify(
                            request.session_id.clone(),
                            acp::SessionUpdate::ToolCall(
                                acp::ToolCall::new(id, tool_title(&name, arguments.as_ref()))
                                    .kind(tool_kind(&name, arguments.as_ref()))
                                    .status(acp::ToolCallStatus::InProgress)
                                    .locations(tool_locations(arguments.as_ref()))
                                    .raw_input(arguments),
                            ),
                        )
                        .await?;
                        let _ = owner_id;
                    }
                    DurableEventKind::NodeAppended {
                        owner_id,
                        node_kind: NodeKind::ToolResult,
                        content,
                        ..
                    } => {
                        let id = owner_id
                            .and_then(|owner| tool_calls.get(&owner).cloned())
                            .or_else(|| {
                                content
                                    .get("call_id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_owned)
                            });
                        if let Some(id) = id {
                            let failed = content
                                .get("is_error")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false);
                            let output = content
                                .get("output")
                                .or_else(|| content.get("model_output"))
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let rendered_content = self
                                .tool_terminals
                                .lock()
                                .await
                                .get(&(request.session_id.clone(), id.clone()))
                                .cloned()
                                .map(|terminal| {
                                    vec![acp::ToolCallContent::Terminal(acp::Terminal::new(
                                        terminal,
                                    ))]
                                })
                                .unwrap_or_else(|| tool_output_content(&output));
                            let notification = self
                                .notify(
                                    request.session_id.clone(),
                                    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                                        id.clone(),
                                        acp::ToolCallUpdateFields::new()
                                            .status(if failed {
                                                acp::ToolCallStatus::Failed
                                            } else {
                                                acp::ToolCallStatus::Completed
                                            })
                                            .content(rendered_content)
                                            .raw_output(output),
                                    )),
                                )
                                .await;
                            if let Some(terminal) = self
                                .tool_terminals
                                .lock()
                                .await
                                .remove(&(request.session_id.clone(), id))
                            {
                                // Terminal cleanup must not delay later turn events.
                                let _ = self.client.send(ClientCommand::ReleaseTerminal(
                                    acp::ReleaseTerminalRequest::new(
                                        request.session_id.clone(),
                                        terminal,
                                    ),
                                ));
                            }
                            notification?;
                        }
                    }
                    DurableEventKind::PlanStarted { .. } => plan.clear(),
                    DurableEventKind::PlanDelta { delta, .. } => {
                        plan.push_str(&delta);
                        self.notify(
                            request.session_id.clone(),
                            acp::SessionUpdate::Plan(acp::Plan::new(vec![acp::PlanEntry::new(
                                plan.clone(),
                                acp::PlanEntryPriority::High,
                                acp::PlanEntryStatus::InProgress,
                            )])),
                        )
                        .await?;
                    }
                    DurableEventKind::ModeChanged {
                        mode,
                        pending: false,
                    } => {
                        self.notify(
                            request.session_id.clone(),
                            acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate::new(
                                mode,
                            )),
                        )
                        .await?;
                    }
                    DurableEventKind::ModelSelectionChanged { pending: false, .. }
                    | DurableEventKind::AgentChanged { pending: false, .. }
                    | DurableEventKind::ConfigurationChanged { .. } => {
                        self.notify(
                            request.session_id.clone(),
                            acp::SessionUpdate::ConfigOptionUpdate(acp::ConfigOptionUpdate::new(
                                self.config_options(&session).await?,
                            )),
                        )
                        .await?;
                    }
                    DurableEventKind::ConversationTitleChanged { title, .. } => {
                        self.notify(
                            request.session_id.clone(),
                            acp::SessionUpdate::SessionInfoUpdate(
                                acp::SessionInfoUpdate::new().title(title),
                            ),
                        )
                        .await?;
                    }
                    DurableEventKind::ModelUsageRecorded { .. } => {
                        if let Some(context) = session.context_usage() {
                            let usage = session.session_usage().await.map_err(Self::error)?;
                            let cost = usage.cost.and_then(|cost| {
                                cost.total_cost
                                    .and_then(|amount| amount.parse::<f64>().ok())
                                    .map(|amount| acp::Cost::new(amount, cost.currency))
                            });
                            self.notify(
                                request.session_id.clone(),
                                acp::SessionUpdate::UsageUpdate(
                                    acp::UsageUpdate::new(
                                        context.used_tokens,
                                        context.context_window,
                                    )
                                    .cost(cost),
                                ),
                            )
                            .await?;
                        }
                    }
                    _ => {}
                },
                RuntimeEvent::Transient {
                    event: TransientEvent::TurnCompleted,
                    ..
                } => {
                    return Ok(acp::PromptResponse::new(if cancelled {
                        acp::StopReason::Cancelled
                    } else {
                        acp::StopReason::EndTurn
                    }));
                }
                RuntimeEvent::Transient {
                    event: TransientEvent::TurnCancellationRequested,
                    ..
                } => cancelled = true,
                RuntimeEvent::Transient {
                    event: TransientEvent::TurnFailed { message },
                    ..
                } => {
                    tracing::error!(%message, "Cagent turn failed");
                    return Err(acp::Error::internal_error());
                }
                RuntimeEvent::Interaction {
                    request: interaction,
                    ..
                } => {
                    let interaction = *interaction;
                    match interaction.kind {
                        InteractionRequestKind::PermissionApproval {
                            message, arguments, ..
                        } => {
                            let tool_call = acp::ToolCallUpdate::new(
                                interaction.id.to_string(),
                                acp::ToolCallUpdateFields::new()
                                    .title(message)
                                    .raw_input(arguments.unwrap_or(serde_json::Value::Null)),
                            );
                            let response = self
                                .request_permission(acp::RequestPermissionRequest::new(
                                    request.session_id.clone(),
                                    tool_call,
                                    vec![
                                        acp::PermissionOption::new(
                                            "allow_once",
                                            "Allow once",
                                            acp::PermissionOptionKind::AllowOnce,
                                        ),
                                        acp::PermissionOption::new(
                                            "allow_session",
                                            "Allow for this session",
                                            acp::PermissionOptionKind::AllowAlways,
                                        ),
                                        acp::PermissionOption::new(
                                            "deny",
                                            "Deny",
                                            acp::PermissionOptionKind::RejectOnce,
                                        ),
                                    ],
                                ))
                                .await?;
                            let decision = match response.outcome {
                                acp::RequestPermissionOutcome::Selected(selected) => {
                                    selected.option_id.to_string()
                                }
                                _ => "deny".into(),
                            };
                            session
                                .submit(SessionCommand::new(SessionAction::RespondToInteraction {
                                    request_id: interaction.id,
                                    response: serde_json::json!({"decision": decision}),
                                }))
                                .await
                                .map_err(Self::error)?;
                        }
                        InteractionRequestKind::Question { questions } => {
                            let mut answers = serde_json::Map::new();
                            let mut cancelled = false;
                            for question in questions {
                                if self
                                    .client_capabilities
                                    .lock()
                                    .await
                                    .elicitation
                                    .as_ref()
                                    .is_some_and(|capabilities| capabilities.form.is_some())
                                {
                                    let selection = acp::StringPropertySchema::new()
                                        .title(question.header.clone())
                                        .one_of(Some(
                                            question
                                                .options
                                                .iter()
                                                .map(|option| {
                                                    acp::EnumOption::new(
                                                        option.label.clone(),
                                                        option.label.clone(),
                                                    )
                                                    .description(option.description.clone())
                                                })
                                                .collect(),
                                        ));
                                    let schema = acp::ElicitationSchema::new()
                                        .property("selection", selection, false)
                                        .property(
                                            "note",
                                            acp::StringPropertySchema::new()
                                                .title("Other answer or additional context"),
                                            false,
                                        );
                                    let response = self
                                        .elicit(acp::CreateElicitationRequest::new(
                                            acp::ElicitationFormMode::new(
                                                acp::ElicitationSessionScope::new(
                                                    request.session_id.clone(),
                                                ),
                                                schema,
                                            ),
                                            question.question.clone(),
                                        ))
                                        .await?;
                                    match response.action {
                                        acp::ElicitationAction::Accept(accepted) => {
                                            let content = accepted.content.unwrap_or_default();
                                            let selection =
                                                content.get("selection").and_then(|value| {
                                                    match value {
                                                        acp::ElicitationContentValue::String(
                                                            value,
                                                        ) => Some(value.clone()),
                                                        _ => None,
                                                    }
                                                });
                                            let note =
                                                content.get("note").and_then(|value| match value {
                                                    acp::ElicitationContentValue::String(value)
                                                        if !value.trim().is_empty() =>
                                                    {
                                                        Some(value.clone())
                                                    }
                                                    _ => None,
                                                });
                                            answers.insert(
                                                question.id,
                                                serde_json::json!({
                                                    "selection": selection,
                                                    "note": note,
                                                }),
                                            );
                                        }
                                        _ => {
                                            cancelled = true;
                                            break;
                                        }
                                    }
                                    continue;
                                }
                                let options = question
                                    .options
                                    .iter()
                                    .map(|option| {
                                        acp::PermissionOption::new(
                                            option.label.clone(),
                                            format!("{} — {}", option.label, option.description),
                                            acp::PermissionOptionKind::AllowOnce,
                                        )
                                    })
                                    .chain(std::iter::once(acp::PermissionOption::new(
                                        "__none__",
                                        "None of the above",
                                        acp::PermissionOptionKind::RejectOnce,
                                    )))
                                    .collect();
                                let outcome = self
                                    .request_permission(acp::RequestPermissionRequest::new(
                                        request.session_id.clone(),
                                        acp::ToolCallUpdate::new(
                                            interaction.id.to_string(),
                                            acp::ToolCallUpdateFields::new().title(format!(
                                                "{}: {}",
                                                question.header, question.question
                                            )),
                                        ),
                                        options,
                                    ))
                                    .await?
                                    .outcome;
                                match outcome {
                                    acp::RequestPermissionOutcome::Selected(selected) => {
                                        answers.insert(
                                            question.id,
                                            if selected.option_id.to_string() == "__none__" {
                                                serde_json::json!({})
                                            } else {
                                                serde_json::json!({"selection": selected.option_id.to_string()})
                                            },
                                        );
                                    }
                                    _ => {
                                        cancelled = true;
                                        break;
                                    }
                                }
                            }
                            session
                                .submit(SessionCommand::new(SessionAction::RespondToInteraction {
                                    request_id: interaction.id,
                                    response: serde_json::json!({
                                        "answers": answers,
                                        "cancelled": cancelled,
                                    }),
                                }))
                                .await
                                .map_err(Self::error)?;
                        }
                        InteractionRequestKind::PlanCompletion {
                            plan,
                            implementation_modes,
                            default_mode: _,
                        } => {
                            let mut options = implementation_modes
                                .iter()
                                .map(|mode| {
                                    acp::PermissionOption::new(
                                        format!("implement:{mode}"),
                                        format!("Implement in {mode} mode"),
                                        acp::PermissionOptionKind::AllowOnce,
                                    )
                                })
                                .collect::<Vec<_>>();
                            options.push(acp::PermissionOption::new(
                                "keep_planning",
                                "Keep planning",
                                acp::PermissionOptionKind::RejectOnce,
                            ));
                            let outcome = self
                                .request_permission(acp::RequestPermissionRequest::new(
                                    request.session_id.clone(),
                                    acp::ToolCallUpdate::new(
                                        interaction.id.to_string(),
                                        acp::ToolCallUpdateFields::new()
                                            .title("Plan complete — choose the next action")
                                            .raw_input(serde_json::json!({"plan": plan})),
                                    ),
                                    options,
                                ))
                                .await?
                                .outcome;
                            let response = match outcome {
                                acp::RequestPermissionOutcome::Selected(selected) => {
                                    let selected = selected.option_id.to_string();
                                    selected.strip_prefix("implement:").map_or_else(
                                        || serde_json::json!({"decision": "keep_planning"}),
                                        |mode| {
                                            serde_json::json!({
                                                "decision": "implement",
                                                "mode": mode,
                                                "context_action": "keep",
                                            })
                                        },
                                    )
                                }
                                _ => serde_json::json!({"decision": "keep_planning"}),
                            };
                            session
                                .submit(SessionCommand::new(SessionAction::RespondToInteraction {
                                    request_id: interaction.id,
                                    response,
                                }))
                                .await
                                .map_err(Self::error)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Err(acp::Error::internal_error())
    }

    async fn cancel(&self, request: acp::CancelNotification) -> acp::Result<()> {
        self.session(&request.session_id).await?.cancel();
        Ok(())
    }

    async fn set_session_mode(
        &self,
        request: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        self.session(&request.session_id)
            .await?
            .set_session_mode(request.mode_id.to_string())
            .await
            .map_err(Self::error)?;
        Ok(acp::SetSessionModeResponse::new())
    }

    async fn set_session_config_option(
        &self,
        request: acp::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::SetSessionConfigOptionResponse> {
        let session = self.session(&request.session_id).await?;
        let value = request
            .value
            .as_value_id()
            .ok_or_else(acp::Error::invalid_params)?
            .to_string();
        match request.config_id.to_string().as_str() {
            "agent" => session
                .set_session_agent(value)
                .await
                .map_err(Self::error)?,
            "mode" => session.set_session_mode(value).await.map_err(Self::error)?,
            "model" => {
                let (provider, model) = value
                    .split_once('/')
                    .ok_or_else(acp::Error::invalid_params)?;
                session
                    .set_session_model(provider, model, None)
                    .await
                    .map_err(Self::error)?;
            }
            "effort" => {
                let (provider, model, _) = session
                    .model_selection()
                    .await
                    .map_err(Self::error)?
                    .ok_or_else(acp::Error::invalid_params)?;
                session
                    .set_session_model(provider, model, Some(value))
                    .await
                    .map_err(Self::error)?;
            }
            _ => return Err(acp::Error::invalid_params()),
        }
        Ok(acp::SetSessionConfigOptionResponse::new(
            self.config_options(&session).await?,
        ))
    }
}

fn tool_kind(name: &str, arguments: Option<&serde_json::Value>) -> acp::ToolKind {
    match name {
        "bash"
            if arguments.is_some_and(|arguments| {
                cagent_agent::presentation::exploration_activity_kind_for("bash", arguments)
                    .is_some()
            }) =>
        {
            acp::ToolKind::Read
        }
        "read" | "list" => acp::ToolKind::Read,
        "bash" | "terminal_write" | "terminal_kill" | "terminal_output" => acp::ToolKind::Execute,
        "apply_patch" => acp::ToolKind::Edit,
        "web_fetch" => acp::ToolKind::Fetch,
        "web_search" | "grep" | "rg" => acp::ToolKind::Search,
        _ => acp::ToolKind::Other,
    }
}

const fn negotiated_protocol_version(
    _requested: acp_sdk::schema::ProtocolVersion,
) -> acp_sdk::schema::ProtocolVersion {
    acp_sdk::schema::ProtocolVersion::V1
}

fn tool_title(name: &str, arguments: Option<&serde_json::Value>) -> String {
    if name != "bash" {
        return name.to_owned();
    }
    let Some(command) = arguments
        .and_then(|arguments| arguments.get("command"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
    else {
        return name.to_owned();
    };
    let command = command.lines().collect::<Vec<_>>().join(" ↵ ");
    const MAX_TITLE_CHARS: usize = 200;
    if command.chars().count() <= MAX_TITLE_CHARS {
        command
    } else {
        format!(
            "{}…",
            command.chars().take(MAX_TITLE_CHARS).collect::<String>()
        )
    }
}

fn available_commands_update(skills: &[cagent_agent::config::SkillMetadata]) -> acp::SessionUpdate {
    let prompt =
        acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new("Prompt"));
    let mut commands = vec![
        acp::AvailableCommand::new("read", "Run a prompt in read mode").input(prompt.clone()),
        acp::AvailableCommand::new("edit", "Run a prompt in edit mode").input(prompt.clone()),
        acp::AvailableCommand::new("auto", "Run a prompt in auto mode").input(prompt.clone()),
        acp::AvailableCommand::new("plan", "Create a plan without modifying the workspace")
            .input(prompt),
        acp::AvailableCommand::new("compact", "Compact conversation context").input(
            acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                "Optional compaction instructions",
            )),
        ),
        acp::AvailableCommand::new("recap", "Generate a conversation recap"),
        acp::AvailableCommand::new("retry", "Retry the previous model response"),
        acp::AvailableCommand::new("spawn", "Run a prompt that must delegate a subtask").input(
            acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new("Prompt")),
        ),
        acp::AvailableCommand::new("web-search", "Run a prompt that must search the web").input(
            acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new("Prompt")),
        ),
    ];
    let mut names = commands
        .iter()
        .map(|command| command.name.to_string())
        .collect::<BTreeSet<_>>();
    for skill in skills {
        if !skill.enabled || !names.insert(skill.name.clone()) {
            continue;
        }
        commands.push(
            acp::AvailableCommand::new(skill.name.clone(), skill.description.clone()).input(
                acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                    "Optional skill arguments",
                )),
            ),
        );
    }
    acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(commands))
}

struct ParsedCommand<'a> {
    name: &'a str,
    arguments: &'a str,
}

fn parse_command(text: &str) -> Option<ParsedCommand<'_>> {
    let text = text.trim();
    let command = text.strip_prefix('/')?;
    let split = command.find(char::is_whitespace).unwrap_or(command.len());
    Some(ParsedCommand {
        name: &command[..split],
        arguments: command[split..].trim(),
    })
}

fn canonical_directories(directories: Vec<PathBuf>) -> acp::Result<Vec<PathBuf>> {
    let mut seen = BTreeSet::new();
    directories
        .into_iter()
        .map(|directory| {
            if !directory.is_absolute() {
                return Err(acp::Error::invalid_params());
            }
            let directory = directory.canonicalize().map_err(CagentAcp::error)?;
            if !directory.is_dir() || !seen.insert(directory.clone()) {
                return Err(acp::Error::invalid_params());
            }
            Ok(directory)
        })
        .collect()
}

fn mcp_servers(
    servers: Vec<acp::McpServer>,
) -> acp::Result<BTreeMap<String, cagent_agent::mcp::McpServerDefinition>> {
    let mut result = BTreeMap::new();
    for server in servers {
        let (name, transport) = match server {
            acp::McpServer::Stdio(server) => (
                server.name,
                cagent_agent::mcp::McpTransportConfig::Stdio {
                    command: server.command.to_string_lossy().into_owned(),
                    args: server.args,
                    cwd: None,
                    env: server
                        .env
                        .into_iter()
                        .map(|entry| (entry.name, entry.value.into()))
                        .collect(),
                    env_remove: Vec::new(),
                    inherit_env: true,
                },
            ),
            acp::McpServer::Http(server) => (
                server.name,
                cagent_agent::mcp::McpTransportConfig::StreamableHttp {
                    url: server.url,
                    headers: server
                        .headers
                        .into_iter()
                        .map(|header| (header.name, header.value.into()))
                        .collect(),
                    allow_insecure: false,
                },
            ),
            _ => return Err(acp::Error::invalid_params()),
        };
        let definition = cagent_agent::mcp::McpServerDefinition {
            transport,
            enabled: true,
            agents: Vec::new(),
            eager: true,
            startup_timeout_seconds: 10,
            request_timeout_seconds: 60,
            read_only_tools: Vec::new(),
            ..cagent_agent::mcp::McpServerDefinition::default()
        };
        if result.insert(name, definition).is_some() {
            return Err(acp::Error::invalid_params());
        }
    }
    Ok(result)
}

fn text_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn text_chunk(text: String, message_id: &str) -> acp::ContentChunk {
    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text)))
        .message_id(message_id)
}

fn tool_output_content(output: &serde_json::Value) -> Vec<acp::ToolCallContent> {
    let text = match output {
        serde_json::Value::Null => return Vec::new(),
        serde_json::Value::String(text) => text.clone(),
        value => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    };
    vec![acp::ToolCallContent::from(text)]
}

fn tool_locations(arguments: Option<&serde_json::Value>) -> Vec<acp::ToolCallLocation> {
    let Some(arguments) = arguments.and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    ["path", "file_path", "directory"]
        .into_iter()
        .filter_map(|key| arguments.get(key).and_then(serde_json::Value::as_str))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(acp::ToolCallLocation::new)
        .collect()
}

async fn prompt_draft(
    session: &SessionHandle,
    content: &[acp::ContentBlock],
) -> acp::Result<UserDraft> {
    let mut text = String::new();
    let mut attachment_specs = Vec::new();
    let mut images = Vec::new();
    let mut image_chips = Vec::new();
    for block in content {
        match block {
            acp::ContentBlock::Text(value) => append_text(&mut text, &value.text),
            acp::ContentBlock::ResourceLink(resource) => {
                if let Some(path) = file_uri_path(&resource.uri) {
                    attachment_specs.push(AttachmentSpec {
                        path,
                        start_line: None,
                        end_line: None,
                    });
                } else {
                    append_text(&mut text, &format!("Resource: {}", resource.uri));
                }
            }
            acp::ContentBlock::Resource(resource) => match &resource.resource {
                acp::EmbeddedResourceResource::TextResourceContents(resource) => {
                    append_text(
                        &mut text,
                        &format!(
                            "<resource uri=\"{}\">\n{}\n</resource>",
                            resource.uri, resource.text
                        ),
                    );
                }
                acp::EmbeddedResourceResource::BlobResourceContents(resource) => {
                    if resource
                        .mime_type
                        .as_deref()
                        .is_some_and(|mime| mime.starts_with("image/"))
                    {
                        add_image(
                            session,
                            &mut text,
                            &mut images,
                            &mut image_chips,
                            &resource.blob,
                        )
                        .await?;
                    } else {
                        append_text(&mut text, &format!("Binary resource: {}", resource.uri));
                    }
                }
                _ => return Err(acp::Error::invalid_params()),
            },
            acp::ContentBlock::Image(image) => {
                add_image(
                    session,
                    &mut text,
                    &mut images,
                    &mut image_chips,
                    &image.data,
                )
                .await?;
            }
            acp::ContentBlock::Audio(_) => return Err(acp::Error::invalid_params()),
            _ => return Err(acp::Error::invalid_params()),
        }
    }
    if text.is_empty() && attachment_specs.is_empty() && images.is_empty() {
        Err(acp::Error::internal_error())
    } else {
        Ok(UserDraft {
            text,
            attachment_specs,
            images,
            image_chips,
        })
    }
}

fn append_text(target: &mut String, value: &str) {
    if !target.is_empty() && !target.ends_with('\n') {
        target.push('\n');
    }
    target.push_str(value);
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

async fn add_image(
    session: &SessionHandle,
    text: &mut String,
    images: &mut Vec<cagent_agent::protocol::ImageAttachment>,
    chips: &mut Vec<ImageChipRange>,
    encoded: &str,
) -> acp::Result<()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(CagentAcp::error)?;
    let number = u64::try_from(images.len() + 1).map_err(CagentAcp::error)?;
    let image = session
        .store_clipboard_image(&bytes, number)
        .await
        .map_err(CagentAcp::error)?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let start = text.len();
    text.push_str(&format!("[Image #{number}]"));
    let end = text.len();
    chips.push(ImageChipRange {
        image_id: image.id,
        start,
        end,
    });
    images.push(image);
    Ok(())
}

/// Runs the Agent Client Protocol server over stdin/stdout until Zed closes it.
///
/// # Errors
///
/// Returns an error when configuration, runtime startup, or the ACP transport fails.
pub async fn serve(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::resolve(PathOverrides {
        config_file: options.config_file,
        data_dir: options.data_dir,
    })?;
    paths.create_directories()?;
    let config = ConfigStore::open(&paths.config_file)?;
    let permissions_file = paths.permissions_file.clone();
    let launch_workspace = std::env::current_dir()?.canonicalize()?;
    let mut runtime_options = RuntimeOptions::new(paths.data_dir.clone())
        .with_credential_dir(paths.data_dir.clone())
        .with_models_dev_cache_path(paths.data_dir.join("models.json"))
        .with_config(config)
        .with_instruction_paths(paths.global_instruction_dir, launch_workspace)
        .with_permissions_file(paths.permissions_file);
    if options.temporary_workspace_trust {
        runtime_options = runtime_options.with_temporary_workspace_trust();
    }
    let runtime = AgentRuntime::open(runtime_options).await?;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let agent = Arc::new(CagentAcp {
        runtime,
        sessions: Mutex::new(HashMap::new()),
        client: sender,
        permissions_file,
        temporary_workspace_trust: options.temporary_workspace_trust,
        client_capabilities: Mutex::new(acp::ClientCapabilities::default()),
        session_directories: Mutex::new(HashMap::new()),
        tool_terminals: Arc::new(Mutex::new(HashMap::new())),
    });
    let initialize = Arc::clone(&agent);
    let authenticate = Arc::clone(&agent);
    let logout = Arc::clone(&agent);
    let list_sessions = Arc::clone(&agent);
    let delete_session = Arc::clone(&agent);
    let new_session = Arc::clone(&agent);
    let load_session = Arc::clone(&agent);
    let resume_session = Arc::clone(&agent);
    let close_session = Arc::clone(&agent);
    let prompt = Arc::clone(&agent);
    let cancel = Arc::clone(&agent);
    let set_mode = Arc::clone(&agent);
    let set_config = Arc::clone(&agent);
    acp_sdk::Agent
        .builder()
        .name("cagent")
        .on_receive_request(
            async move |request: acp::InitializeRequest, responder, _connection| {
                responder.respond_with_result(initialize.initialize(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::AuthenticateRequest, responder, _connection| {
                responder.respond_with_result(authenticate.authenticate(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::LogoutRequest, responder, _connection| {
                responder.respond_with_result(logout.logout(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::ListSessionsRequest, responder, _connection| {
                responder.respond_with_result(list_sessions.list_sessions(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::DeleteSessionRequest, responder, _connection| {
                responder.respond_with_result(delete_session.delete_session(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::NewSessionRequest, responder, _connection| {
                let result = new_session.new_session(request).await;
                let session_id = result
                    .as_ref()
                    .ok()
                    .map(|response| response.session_id.clone());
                responder.respond_with_result(result)?;
                if let Some(session_id) = session_id {
                    new_session.send_available_commands(&session_id).await?;
                }
                Ok(())
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::LoadSessionRequest, responder, _connection| {
                let session_id = request.session_id.clone();
                let result = load_session.load_session(request).await;
                let succeeded = result.is_ok();
                responder.respond_with_result(result)?;
                if succeeded {
                    load_session.send_available_commands(&session_id).await?;
                }
                Ok(())
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::ResumeSessionRequest, responder, _connection| {
                let session_id = request.session_id.clone();
                let result = resume_session.resume_session(request).await;
                let succeeded = result.is_ok();
                responder.respond_with_result(result)?;
                if succeeded {
                    resume_session.send_available_commands(&session_id).await?;
                }
                Ok(())
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::CloseSessionRequest, responder, _connection| {
                responder.respond_with_result(close_session.close_session(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::PromptRequest, responder, connection| {
                let prompt = Arc::clone(&prompt);
                // A prompt can send requests back to the client. Run it outside the
                // SDK dispatch loop so responses and cancellation can be received.
                connection.spawn(async move {
                    responder.respond_with_result(prompt.prompt(request).await)
                })?;
                Ok(())
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_notification(
            async move |request: acp::CancelNotification, _connection| cancel.cancel(request).await,
            acp_sdk::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: acp::SetSessionModeRequest, responder, _connection| {
                responder.respond_with_result(set_mode.set_session_mode(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::SetSessionConfigOptionRequest, responder, _connection| {
                responder.respond_with_result(set_config.set_session_config_option(request).await)
            },
            acp_sdk::on_receive_request!(),
        )
        .connect_with(acp_sdk::Stdio::new(), async move |connection| {
            let closed = connection.clone();
            tokio::select! {
                () = closed.incoming_closed() => {},
                () = async move {
                while let Some(command) = receiver.recv().await {
                    match command {
                        ClientCommand::Notify(notification, delivered) => {
                            if connection.send_notification(notification).is_err() {
                                break;
                            }
                            let _ = delivered.send(());
                        }
                        ClientCommand::Permission(request, response) => {
                            let result = connection.send_request(request).block_task().await;
                            let _ = response.send(result);
                        }
                        ClientCommand::Elicit(request, response) => {
                            let result = connection.send_request(request).block_task().await;
                            let _ = response.send(result);
                        }
                        ClientCommand::WriteTextFile(request, response) => {
                            let connection = connection.clone();
                            tokio::spawn(async move {
                                let result = connection.send_request(request).block_task().await;
                                let _ = response.send(result);
                            });
                        }
                        ClientCommand::CreateTerminal(request, response) => {
                            let result = connection.send_request(request).block_task().await;
                            let _ = response.send(result);
                        }
                        ClientCommand::TerminalOutput(request, response) => {
                            let result = connection.send_request(request).block_task().await;
                            let _ = response.send(result);
                        }
                        ClientCommand::WaitTerminal(request, response) => {
                            let connection = connection.clone();
                            tokio::spawn(async move {
                                let result = connection.send_request(request).block_task().await;
                                let _ = response.send(result);
                            });
                        }
                        ClientCommand::ReleaseTerminal(request) => {
                            let connection = connection.clone();
                            tokio::spawn(async move {
                                let _ = connection.send_request(request).block_task().await;
                            });
                        }
                        ClientCommand::KillTerminal(request, response) => {
                            let result = connection.send_request(request).block_task().await;
                            let _ = response.send(result);
                        }
                    }
                }
                } => {},
            }
            Ok(())
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn filesystem_backend_forwards_text_writes_to_the_acp_client() {
        let (client, mut commands) = mpsc::unbounded_channel();
        let backend = Arc::new(AcpFileSystemBackend {
            session_id: acp::SessionId::new("session-1"),
            client,
        });
        let write = tokio::spawn({
            let backend = Arc::clone(&backend);
            async move {
                cagent_agent::frontend::FrontendFileSystem::write_text_file(
                    backend.as_ref(),
                    cagent_agent::frontend::FrontendWriteTextFileRequest {
                        path: PathBuf::from("/workspace/file.txt"),
                        content: "new contents".into(),
                    },
                    CancellationToken::new(),
                )
                .await
            }
        });

        let ClientCommand::WriteTextFile(request, response) = commands.recv().await.unwrap() else {
            panic!("expected an ACP file write");
        };
        assert_eq!(request.session_id, acp::SessionId::new("session-1"));
        assert_eq!(request.path, PathBuf::from("/workspace/file.txt"));
        assert_eq!(request.content, "new contents");
        response
            .send(Ok(acp::WriteTextFileResponse::new()))
            .unwrap();
        write.await.unwrap().unwrap();
    }

    #[test]
    fn extracts_text_prompt_blocks() {
        let mut text = String::new();
        append_text(&mut text, "hello");
        append_text(&mut text, "world");
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn extracts_file_resource_paths() {
        assert_eq!(
            file_uri_path("file:///workspace/a.rs"),
            Some(PathBuf::from("/workspace/a.rs"))
        );
        assert_eq!(file_uri_path("https://example.com/a.rs"), None);
    }

    #[test]
    fn parses_advertised_slash_commands() {
        let command = parse_command("  /plan investigate this  ").unwrap();
        assert_eq!(command.name, "plan");
        assert_eq!(command.arguments, "investigate this");
        assert!(parse_command("ordinary prompt").is_none());
    }

    #[test]
    fn advertises_commands_with_input_metadata() {
        let value = serde_json::to_value(available_commands_update(&[])).unwrap();
        assert_eq!(value["sessionUpdate"], "available_commands_update");
        let commands = value["availableCommands"].as_array().unwrap();
        assert!(commands.iter().any(|command| command["name"] == "plan"));
        assert!(commands.iter().any(|command| command["name"] == "compact"));
        assert!(commands.iter().any(|command| command["name"] == "recap"));
        for primitive in ["bash", "read-file", "write-file"] {
            assert!(commands.iter().all(|command| command["name"] != primitive));
        }
    }

    #[test]
    fn advertises_enabled_skills_without_overriding_builtins() {
        let skill = |name: &str, enabled| cagent_agent::config::SkillMetadata {
            path: PathBuf::from(format!("{name}/SKILL.md")),
            name: name.into(),
            description: format!("Use {name}"),
            source: cagent_agent::config::SkillSource::Project,
            compatibility: None,
            compatibility_project: false,
            enabled,
            content_hash: String::new(),
        };
        let value = serde_json::to_value(available_commands_update(&[
            skill("review", true),
            skill("disabled", false),
            skill("plan", true),
        ]))
        .unwrap();
        let commands = value["availableCommands"].as_array().unwrap();
        assert!(commands.iter().any(|command| {
            command["name"] == "review" && command["description"] == "Use review"
        }));
        assert!(commands.iter().all(|command| command["name"] != "disabled"));
        assert_eq!(
            commands
                .iter()
                .filter(|command| command["name"] == "plan")
                .count(),
            1
        );
    }

    #[test]
    fn message_chunks_include_the_persisted_node_id() {
        let value = serde_json::to_value(text_chunk("hello".into(), "node-123")).unwrap();
        assert_eq!(value["messageId"], "node-123");
        assert_eq!(value["content"]["text"], "hello");
    }

    #[test]
    fn negotiates_only_stable_v1() {
        let requested = serde_json::from_value(serde_json::json!(2)).unwrap();
        assert_eq!(
            negotiated_protocol_version(requested),
            acp_sdk::schema::ProtocolVersion::V1
        );
    }

    #[test]
    fn presents_read_only_bash_with_its_command() {
        let arguments = serde_json::json!({
            "command": "ls -la",
            "wait": true,
        });
        assert_eq!(tool_title("bash", Some(&arguments)), "ls -la");
        assert_eq!(tool_kind("bash", Some(&arguments)), acp::ToolKind::Read);

        let mutation = serde_json::json!({
            "command": "touch created.txt",
            "wait": true,
        });
        assert_eq!(tool_kind("bash", Some(&mutation)), acp::ToolKind::Execute);
    }

    #[test]
    fn prepares_tool_output_and_absolute_locations() {
        let content = tool_output_content(&serde_json::json!({"ok": true}));
        assert_eq!(content.len(), 1);
        let locations = tool_locations(Some(&serde_json::json!({
            "path": "/workspace/file.rs",
            "directory": "relative"
        })));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, PathBuf::from("/workspace/file.rs"));
    }
}
