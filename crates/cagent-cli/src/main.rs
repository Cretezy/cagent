use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{IsTerminal, Read as _, Write as _};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::time::Instant;

use cagent_agent::config::{AppPaths, ConfigStore, PathOverrides};
use cagent_agent::protocol::{
    ConversationId, DurableEventKind, InteractionRequestKind, RuntimeEvent, SessionAction,
    SessionCommand, SessionUsage, ToolPolicy, TransientEvent,
};
use cagent_agent::runtime::{AgentRuntime, NewSession, RuntimeOptions};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::style::Stylize as _;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use futures_util::StreamExt as _;
use serde::Serialize;
use serde_json::Value;
use tracing::Instrument as _;

#[allow(
    dead_code,
    unused_imports,
    clippy::if_same_then_else,
    clippy::double_ended_iterator_last,
    clippy::collapsible_if,
    clippy::let_and_return,
    clippy::collapsible_match,
    clippy::question_mark,
    clippy::type_complexity,
    clippy::useless_format,
    clippy::single_match,
    clippy::too_many_arguments,
    clippy::obfuscated_if_else,
    clippy::large_enum_variant,
    clippy::derivable_impls,
    clippy::match_like_matches_macro,
    clippy::borrow_deref_ref,
    clippy::redundant_closure,
    clippy::unnecessary_to_owned,
    clippy::map_flatten,
    clippy::bool_assert_comparison,
    clippy::field_reassign_with_default,
    clippy::unnecessary_find_map,
    clippy::useless_vec
)] // The TUI keeps rendering and test fixtures near their input handlers.
mod app;
mod config;
mod diagnostic;
mod exec;
mod markdown;
mod mcp;
#[allow(
    unused_imports,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::derivable_impls,
    clippy::collapsible_if,
    clippy::match_like_matches_macro,
    clippy::borrow_deref_ref,
    clippy::redundant_closure,
    clippy::unnecessary_to_owned,
    clippy::let_and_return,
    clippy::useless_vec
)] // Rendering retains explicit layout state and test fixtures.
mod render;
mod theme;
mod trust;

#[derive(Debug, Parser)]
#[command(
    name = "cagent",
    version,
    about = "Cagent Ratatui frontend",
    subcommand_precedence_over_arg = true
)]
struct Cli {
    /// Change to DIR before starting Cagent.
    #[arg(long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,

    /// Create/enter a worktree for a new session, or select it for `continue`.
    #[arg(short = 'w', long, global = true, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    worktree: Option<String>,

    /// Override the configuration file.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Override the data directory containing global and conversation databases.
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Words forming the new session name; existing sessions are unchanged.
    #[arg(value_name = "NAME")]
    name: Vec<String>,

    /// Prompt to submit to the new session.
    #[arg(short = 'p', long = "prompt", global = true, value_name = "PROMPT")]
    initial_prompt: Option<String>,

    /// Select the provider for this session's default model.
    #[arg(long, global = true, value_name = "PROVIDER")]
    provider: Option<String>,

    /// Select the model ID for this session's default model.
    #[arg(long, global = true, value_name = "MODEL")]
    model: Option<String>,

    /// Select the agent profile for this session.
    #[arg(long, global = true, value_name = "AGENT")]
    agent: Option<String>,

    /// Select the mode for this session.
    #[arg(long, global = true, value_name = "MODE")]
    mode: Option<String>,

    /// Select the reasoning effort for this session's model.
    #[arg(long, global = true, value_name = "EFFORT")]
    effort: Option<String>,

    /// Temporarily trust this workspace for a session-launch command.
    #[arg(long, global = true)]
    trust: bool,

    /// Archive a newly created conversation immediately.
    #[arg(long, global = true)]
    archive: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Run Cagent as an Agent Client Protocol server for editors such as Zed.
    Acp,
    /// Run one prompt without opening the terminal UI.
    Exec {
        /// Select text, versioned JSON Lines, or structured output.
        #[arg(long, value_enum, default_value_t = ExecOutputMode::Text)]
        output: ExecOutputMode,
        /// Stream assistant delta records in JSON mode.
        #[arg(long)]
        delta: bool,
        /// Inline JSON Schema for the final assistant result.
        #[arg(long, value_name = "JSON", conflicts_with = "output_schema_file")]
        output_schema: Option<String>,
        /// Read the JSON Schema for the final assistant result from FILE.
        #[arg(long, value_name = "FILE", conflicts_with = "output_schema")]
        output_schema_file: Option<PathBuf>,
        /// Restrict this exec to the named tools. Repeat for multiple tools.
        #[arg(
            long = "allow-tool",
            visible_alias = "allow-tools",
            value_name = "TOOL",
            value_delimiter = ','
        )]
        allow_tools: Vec<String>,
        /// Prevent this exec from using the named tools. Repeat for multiple tools.
        #[arg(
            long = "deny-tool",
            visible_alias = "deny-tools",
            value_name = "TOOL",
            value_delimiter = ','
        )]
        deny_tools: Vec<String>,
        /// Do not retain the conversation after this command exits.
        #[arg(long, conflicts_with = "archive")]
        no_session: bool,
        /// Request Fast service-tier routing for this exec without changing configuration.
        #[arg(long)]
        fast: bool,
        /// Session name; existing sessions are unchanged.
        #[arg(short = 'n', long, value_name = "NAME")]
        name: Option<String>,
        /// Prompt to submit. If omitted, read UTF-8 input from stdin.
        #[arg(value_name = "PROMPT")]
        prompt: Option<String>,
    },
    /// Resume a conversation by ID or title, or open the workspace picker.
    Resume {
        #[arg(value_name = "ID_OR_TITLE")]
        id: Option<String>,
    },
    /// Resume the most recently updated conversation in this workspace.
    Continue,
    /// Manage configured external MCP servers.
    Mcp {
        #[command(subcommand)]
        command: mcp::McpCommand,
    },
    /// Inspect and edit the main TOML configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum ExecOutputMode {
    #[default]
    Text,
    Json,
    Structured,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the complete configuration with TOML syntax highlighting.
    View {
        /// Redact values whose keys look secret-bearing.
        #[arg(long)]
        safe: bool,
    },
    /// Print one configured value by dotted key.
    Get { key: String },
    /// Set a TOML value by dotted key. Bare values are treated as strings when not valid TOML.
    Set { key: String, value: String },
    /// Open the configuration in $VISUAL or $EDITOR.
    Edit,
    /// List dotted keys explicitly set in the configuration file.
    List,
}

fn main() {
    if let Err(error) = run() {
        let exec_error = error.downcast_ref::<ExecExitError>();
        if exec_error.is_none_or(|error| !error.message.is_empty()) {
            eprintln!("cagent: {error}");
        }
        let code = exec_error.map_or(1, |error| error.code);
        std::process::exit(code);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut cli = Cli::parse();
    if let Some(directory) = cli.dir.take() {
        std::env::set_current_dir(&directory).map_err(|error| {
            format!(
                "failed to change directory to {}: {error}",
                directory.display()
            )
        })?;
    }
    if let Some(command) = take_mcp_command(&mut cli.command) {
        return mcp::run(cli.config, cli.data_dir, command);
    }
    if let Some(command) = take_config_command(&mut cli.command) {
        return config::run(cli.config, cli.data_dir, command);
    }
    if take_acp_command(&mut cli.command) {
        let runtime = tokio::runtime::Runtime::new()?;
        return runtime.block_on(cagent_acp::serve(cagent_acp::Options {
            config_file: cli.config,
            data_dir: cli.data_dir,
            temporary_workspace_trust: cli.trust,
        }));
    }
    if let Some(command) = take_exec_command(&mut cli.command) {
        return exec::run(cli, command);
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("cagent requires an interactive terminal".into());
    }
    app::set_terminal_title("Cagent")?;
    // This frontend intentionally uses background panels and semantic colors.
    // Crossterm honors NO_COLOR globally, but doing so would make the Ratatui
    // layout materially different from the existing TUI.
    crossterm::style::force_color_output(true);
    let prompt = cli.initial_prompt.clone();
    let workspace = std::env::current_dir()?.canonicalize()?;
    let paths = AppPaths::resolve(PathOverrides {
        config_file: cli.config,
        data_dir: cli.data_dir,
    })?;
    let show_onboarding = should_show_onboarding(
        &paths.config_file,
        cli.command.as_ref(),
        cli.provider.as_deref(),
        cli.model.as_deref(),
        cli.effort.as_deref(),
    );
    let provider = cli.provider;
    let model = cli.model;
    let name = (!cli.name.is_empty()).then(|| cli.name.join(" "));
    let agent = cli.agent;
    let mode = cli.mode;
    let effort = cli.effort;
    let temporary_workspace_trust = cli.trust;
    let startup_worktree = cli.worktree.clone();
    let archive = cli.archive;
    paths.create_directories()?;
    let _diagnostic_guard = diagnostic::init_logging(&paths.data_dir)?;
    let startup_started_at = Instant::now();
    tracing::trace!(phase = "startup_started", "startup timing began");

    let result = ratatui::run(|terminal| -> Result<_, Box<dyn std::error::Error>> {
        if !workspace_trust_preflight(
            &paths.permissions_file,
            &workspace,
            temporary_workspace_trust,
            terminal,
            startup_started_at,
        )? {
            return Ok(None);
        }
        tracing::trace!(
            phase = "trust_preflight_complete",
            elapsed = ?startup_started_at.elapsed(),
            "startup milestone reached"
        );

        let config = {
            let _span =
                tracing::trace_span!("frontend.startup.phase", phase = "config_loaded").entered();
            ConfigStore::open(&paths.config_file)?
        };

        let async_runtime = {
            let _span =
                tracing::trace_span!("frontend.startup.phase", phase = "tokio_runtime_created")
                    .entered();
            tokio::runtime::Runtime::new()?
        };
        let (agent_runtime, session, startup_surface) = async_runtime.block_on(async {
            let mut options = RuntimeOptions::new(paths.data_dir.clone())
                .with_config(config)
                .with_instruction_paths(paths.global_instruction_dir.clone(), workspace.clone())
                .with_permissions_file(paths.permissions_file.clone());
            if let Some(CliCommand::Resume { id: Some(id) }) = cli.command.as_ref()
                && let Ok(id) = id.parse()
            {
                options = options.with_cleanup_protected_conversation(id);
            }
            if temporary_workspace_trust {
                options = options.with_temporary_workspace_trust();
            }
            let runtime = AgentRuntime::open(options)
                .instrument(tracing::trace_span!(
                    "frontend.startup.phase",
                    phase = "agent_runtime_opened"
                ))
                .await?;

            let resuming = matches!(cli.command.as_ref(), Some(CliCommand::Resume { .. } | CliCommand::Continue));
            if matches!(cli.command.as_ref(), Some(CliCommand::Resume { .. }))
                && startup_worktree.is_some()
            {
                eprintln!("Ignoring --worktree because resumed sessions restore their recorded working directory.");
            }
            let (session, startup_surface) = async {
                match cli.command {
                    Some(CliCommand::Resume { id: Some(selector) }) => {
                        let id = runtime.resolve_conversation(&selector).await?;
                        Ok::<_, Box<dyn std::error::Error>>((
                            runtime.resume_session(id).await?,
                            None,
                        ))
                    }
                    Some(CliCommand::Resume { id: None }) => {
                        let conversations = runtime.conversations(Some(&workspace)).await?;
                        Ok((
                            runtime
                                .create_session_named(
                                    NewSession {
                                        workspace: workspace.clone(),
                                    },
                                    name.clone(),
                                )
                                .await?,
                            Some(app::StartupSurface::ResumePicker(conversations)),
                        ))
                    }
                    Some(CliCommand::Continue) => {
                        let session = match startup_worktree.as_deref() {
                            Some("") => {
                                return Err("--worktree requires NAME with continue".into());
                            }
                            Some(name) => {
                                runtime
                                    .continue_session_in_worktree(&workspace, name)
                                    .await?
                            }
                            None => runtime.continue_session(&workspace).await?,
                        };
                        Ok((session, None))
                    }
                    None => {
                        let session = runtime
                            .create_session_named(
                                NewSession {
                                    workspace: workspace.clone(),
                                },
                                name.clone(),
                            )
                            .await?;
                        if archive {
                            runtime.set_conversation_archived(session.id(), true).await?;
                        }
                        Ok((
                            session,
                            show_onboarding.then_some(app::StartupSurface::Onboarding),
                        ))
                    }
                    Some(CliCommand::Mcp { .. }) => {
                        unreachable!("MCP commands return before TUI setup")
                    }
                    Some(CliCommand::Config { .. }) => {
                        unreachable!("config commands return before TUI setup")
                    }
                    Some(CliCommand::Exec { .. }) => {
                        unreachable!("exec commands return before TUI setup")
                    }
                    Some(CliCommand::Acp) => {
                        unreachable!("ACP commands return before TUI setup")
                    }
                }
            }
            .instrument(tracing::trace_span!(
                "frontend.startup.phase",
                phase = "session_opened"
            ))
            .await?;

            if !resuming && let Some(name) = startup_worktree.as_deref() {
                session
                    .enter_worktree(cagent_agent::runtime::worktrees::EnterWorktreeRequest {
                        name: (!name.is_empty()).then(|| name.to_owned()),
                        path: None,
                        base: None,
                    })
                    .await?;
            }

            async {
                let requested_model = session_model_override(
                    &session,
                    provider.as_deref(),
                    model.as_deref(),
                    effort.as_deref(),
                    &runtime.config_snapshot(),
                    false,
                )
                .await?;
                if let Some(agent) = agent.as_deref() {
                    session.set_session_agent(agent.to_owned()).await?;
                }
                if let Some(mode) = mode.as_deref() {
                    session.set_session_mode(mode.to_owned()).await?;
                }
                if let Some(selection) = requested_model {
                    session
                        .set_session_model(selection.provider, selection.model, selection.effort)
                        .await?;
                }
                Ok::<_, Box<dyn std::error::Error>>(())
            }
            .instrument(tracing::trace_span!(
                "frontend.startup.phase",
                phase = "session_overrides_applied"
            ))
            .await?;
            Ok::<_, Box<dyn std::error::Error>>((runtime, session, startup_surface))
        })?;

        // Ratatui already owns the alternate screen for the trust preflight
        // and keeps it through the main app. Mouse reporting and bracketed
        // paste remain explicit Crossterm opt-ins for the main interface.
        {
            let _span = tracing::trace_span!(
                "frontend.startup.phase",
                phase = "terminal_input_configured"
            )
            .entered();
            execute!(
                std::io::stdout(),
                EnableBracketedPaste,
                EnableMouseCapture,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            )?;
        }
        let app_result = async_runtime.block_on(app::run(
            &agent_runtime,
            session,
            workspace.clone(),
            prompt,
            startup_surface,
            terminal,
            startup_started_at,
        ));
        let disable_result = execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
        );
        let stats = app_result?;
        disable_result?;
        Ok(Some(stats))
    });
    let Some((stats, session_workspace)) = result? else {
        println!("Cagent exited because the workspace was not trusted.");
        return Ok(());
    };
    if should_print_session_exit_message(&stats) {
        print_session_summary(&stats, &workspace, &session_workspace);
        println!(
            "Resume with {}",
            format!("cagent resume {}", stats.id).cyan().bold()
        );
    }
    Ok(())
}

fn should_show_onboarding(
    config_file: &Path,
    command: Option<&CliCommand>,
    provider: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
) -> bool {
    command.is_none()
        && provider.is_none()
        && model.is_none()
        && effort.is_none()
        && config_file_is_empty(config_file)
}

fn config_file_is_empty(config_file: &Path) -> bool {
    match std::fs::read_to_string(config_file) {
        Ok(source) => source.trim().is_empty(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn take_mcp_command(command: &mut Option<CliCommand>) -> Option<mcp::McpCommand> {
    match command.take()? {
        CliCommand::Mcp { command } => Some(command),
        other => {
            *command = Some(other);
            None
        }
    }
}

fn take_config_command(command: &mut Option<CliCommand>) -> Option<ConfigCommand> {
    match command.take()? {
        CliCommand::Config { command } => Some(command),
        other => {
            *command = Some(other);
            None
        }
    }
}

fn take_exec_command(command: &mut Option<CliCommand>) -> Option<ExecCommand> {
    match command.take()? {
        CliCommand::Exec {
            output,
            delta,
            output_schema,
            output_schema_file,
            allow_tools,
            deny_tools,
            no_session,
            fast,
            prompt,
            name,
        } => Some(ExecCommand {
            output,
            delta,
            output_schema,
            output_schema_file,
            allow_tools,
            deny_tools,
            no_session,
            fast,
            prompt,
            name,
        }),
        other => {
            *command = Some(other);
            None
        }
    }
}

fn take_acp_command(command: &mut Option<CliCommand>) -> bool {
    if matches!(command, Some(CliCommand::Acp)) {
        command.take();
        true
    } else {
        false
    }
}

#[derive(Debug)]
struct ExecCommand {
    output: ExecOutputMode,
    delta: bool,
    output_schema: Option<String>,
    output_schema_file: Option<PathBuf>,
    allow_tools: Vec<String>,
    deny_tools: Vec<String>,
    no_session: bool,
    fast: bool,
    prompt: Option<String>,
    name: Option<String>,
}

#[derive(Debug)]
struct ExecExitError {
    code: i32,
    message: String,
}

impl std::fmt::Display for ExecExitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for ExecExitError {}

#[derive(Serialize)]
struct ExecRecord<'a, T: Serialize> {
    version: u16,
    event: &'a str,
    #[serde(flatten)]
    data: T,
}

struct ExecOutput {
    mode: ExecOutputMode,
    assistant_started: bool,
    bash_commands: std::collections::HashMap<String, String>,
}

impl ExecOutput {
    fn new(mode: ExecOutputMode) -> Self {
        Self {
            mode,
            assistant_started: false,
            bash_commands: std::collections::HashMap::new(),
        }
    }

    fn emits_json_lines(&self) -> bool {
        self.mode == ExecOutputMode::Json
    }

    fn silent(&self) -> bool {
        self.mode == ExecOutputMode::Structured
    }
}

fn exec_error(code: i32, message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(ExecExitError {
        code,
        message: message.into(),
    })
}

fn emit_exec<T: Serialize>(
    output: &mut ExecOutput,
    event: &str,
    data: T,
) -> Result<(), Box<dyn std::error::Error>> {
    // Every headless event first becomes the stable JSON record. Text output is
    // only a presentation of that record, so both formats share one schema.
    let record = serde_json::to_value(ExecRecord {
        version: 1,
        event,
        data,
    })?;
    if event == "tool_started"
        && record
            .get("tool")
            .and_then(|tool| tool.get("name"))
            .and_then(serde_json::Value::as_str)
            == Some("bash")
        && let (Some(node_id), Some(command)) = (
            record.get("node_id").and_then(serde_json::Value::as_str),
            record
                .get("tool")
                .and_then(|tool| tool.get("arguments"))
                .and_then(|arguments| arguments.get("command"))
                .and_then(serde_json::Value::as_str),
        )
    {
        output
            .bash_commands
            .insert(node_id.to_owned(), command.to_owned());
    }
    if output.silent() {
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    if output.emits_json_lines() {
        serde_json::to_writer(&mut stdout, &record)?;
        writeln!(stdout)?;
    } else if event == "assistant_delta" {
        if !output.assistant_started {
            write!(stdout, "{}", "assistant: ".green())?;
            output.assistant_started = true;
        }
        if let Some(delta) = record.get("delta").and_then(serde_json::Value::as_str) {
            write!(stdout, "{delta}")?;
            stdout.flush()?;
        }
    } else {
        if output.assistant_started {
            writeln!(stdout)?;
            output.assistant_started = false;
        }
        if event == "tool_completed"
            && record
                .get("result")
                .and_then(|result| result.get("name"))
                .and_then(serde_json::Value::as_str)
                != Some("bash")
        {
            return Ok(());
        }
        let (detail, color) = if event == "tool_completed" {
            let call_node_id = record
                .get("call_node_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let command = output
                .bash_commands
                .get(call_node_id)
                .map(String::as_str)
                .unwrap_or("bash");
            (
                format_exec_tool_completion(&record, command),
                crossterm::style::Color::DarkGrey,
            )
        } else if event == "tool_output" {
            let node_id = record
                .get("node_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let command = output
                .bash_commands
                .get(node_id)
                .map(String::as_str)
                .unwrap_or("bash");
            let text = record
                .get("output")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim_end()
                .replace(['\n', '\r'], " ");
            (
                format!("[bash: {command}]: {text}"),
                crossterm::style::Color::Yellow,
            )
        } else {
            format_exec_text_record(&record)
        };
        let raw_activity = exec_event_uses_raw_activity(event);
        if text_output_uses_color(std::io::stdout().is_terminal()) {
            if event == "user_message" {
                let text = record
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                writeln!(stdout, "{}{}", "user: ".cyan(), text)?;
            } else if raw_activity {
                writeln!(stdout, "{}", detail.with(color))?;
            } else {
                writeln!(stdout, "{}", format!("[{detail}]").with(color))?;
            }
        } else {
            if event == "user_message" || raw_activity {
                writeln!(stdout, "{detail}")?;
            } else {
                writeln!(stdout, "[{detail}]")?;
            }
        }
    }
    Ok(())
}

fn exec_event_uses_raw_activity(event: &str) -> bool {
    matches!(
        event,
        "tool_started" | "tool_output" | "tool_completed" | "permission_denied"
    )
}

fn emit_exec_header(
    output: &ExecOutput,
    workspace: &Path,
    session_id: ConversationId,
    model_selection: Option<&(String, String, Option<String>)>,
) -> Result<(), Box<dyn std::error::Error>> {
    if output.mode != ExecOutputMode::Text {
        return Ok(());
    }

    let model = model_selection.map_or_else(
        || "unknown".to_owned(),
        |(provider, model, effort)| {
            format!(
                "{provider}/{model} {}",
                effort.as_deref().unwrap_or("default")
            )
        },
    );
    let mut stdout = std::io::stdout().lock();
    if text_output_uses_color(std::io::stdout().is_terminal()) {
        writeln!(
            stdout,
            "{} {} {}",
            ">_".cyan().bold(),
            "Cagent".bold(),
            format!("(v{})", env!("CARGO_PKG_VERSION")).dim()
        )?;
        writeln!(stdout, "{} {}", "directory:".dim(), workspace.display())?;
        writeln!(stdout, "{} {}", "session:".dim(), session_id)?;
        writeln!(stdout, "{} {}", "model:".dim(), model)?;
    } else {
        writeln!(stdout, ">_ Cagent (v{})", env!("CARGO_PKG_VERSION"))?;
        writeln!(stdout, "directory: {}", workspace.display())?;
        writeln!(stdout, "session: {session_id}")?;
        writeln!(stdout, "model: {model}")?;
    }
    writeln!(stdout)?;
    Ok(())
}

fn format_exec_text_record(record: &serde_json::Value) -> (String, crossterm::style::Color) {
    let event = record
        .get("event")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("event");
    match event {
        "started" => ("Started".to_owned(), crossterm::style::Color::Green),
        "user_message" => (
            format!(
                "user: {}",
                record
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            ),
            crossterm::style::Color::Cyan,
        ),
        "tool_started" => (format_exec_tool(record), crossterm::style::Color::Cyan),
        "tool_output" => {
            let output = record
                .get("output")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim_end()
                .replace(['\n', '\r'], " ");
            (
                format!("Tool output: {output}"),
                crossterm::style::Color::Yellow,
            )
        }
        "tool_completed" => (
            "Tool completed".to_owned(),
            crossterm::style::Color::DarkGrey,
        ),
        "permission_denied" => (
            format!(
                "[Permission denied]: {}",
                format_exec_denied_resource(record)
            ),
            crossterm::style::Color::Red,
        ),
        "usage" => (format_exec_usage(record), crossterm::style::Color::DarkGrey),
        "error" => (
            format!(
                "Error: {}",
                record
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("execution failed")
            ),
            crossterm::style::Color::Red,
        ),
        "completed" => (
            format_exec_completion(record),
            crossterm::style::Color::Green,
        ),
        _ => (event.replace('_', " "), crossterm::style::Color::DarkGrey),
    }
}

fn format_exec_denied_resource(record: &serde_json::Value) -> String {
    let Some(resource) = record
        .get("resource")
        .and_then(serde_json::Value::as_object)
    else {
        return record
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("permission request")
            .to_owned();
    };

    if let Some(command) = resource
        .get("raw_command")
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.is_empty())
    {
        return normalize_bash_display_redirections(command);
    }
    if let Some(command) = resource
        .get("command")
        .and_then(serde_json::Value::as_array)
        .filter(|command| !command.is_empty())
        .and_then(|command| {
            command
                .iter()
                .map(serde_json::Value::as_str)
                .collect::<Option<Vec<_>>>()
        })
    {
        return command.join(" ");
    }

    let tool = resource
        .get("tool")
        .and_then(serde_json::Value::as_str)
        .filter(|tool| !tool.is_empty());
    if let Some(path) = resource
        .get("path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
    {
        return tool.map_or_else(|| path.to_owned(), |tool| format!("{tool} {path}"));
    }
    if let Some(operation) = resource
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .filter(|operation| !operation.is_empty())
    {
        return resource
            .get("server")
            .and_then(serde_json::Value::as_str)
            .filter(|server| !server.is_empty())
            .map_or_else(
                || operation.to_owned(),
                |server| format!("{server}/{operation}"),
            );
    }
    if let Some(tool) = tool {
        return tool.to_owned();
    }
    record
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("permission request")
        .to_owned()
}

fn normalize_bash_display_redirections(command: &str) -> String {
    // brush-parser's Display implementation inserts a space before the
    // duplicated file descriptor. Keep the one safe form's conventional
    // spelling in headless diagnostics.
    command.replace("2>& 1", "2>&1")
}

fn format_exec_tool(record: &serde_json::Value) -> String {
    let tool = record.get("tool").unwrap_or(&serde_json::Value::Null);
    let name = tool
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool");
    let arguments = tool.get("arguments").unwrap_or(&serde_json::Value::Null);
    let subject = ["path", "pattern", "command", "task", "prompt"]
        .into_iter()
        .find_map(|key| arguments.get(key).and_then(serde_json::Value::as_str));
    if name == "bash" {
        subject.map_or_else(
            || "[bash start]".to_owned(),
            |subject| format!("[bash start: {subject}]"),
        )
    } else {
        subject.map_or_else(
            || format!("[{name}]"),
            |subject| format!("[{name}]: {subject}"),
        )
    }
}

fn format_exec_tool_completion(record: &serde_json::Value, command: &str) -> String {
    let result = record.get("result").unwrap_or(&serde_json::Value::Null);
    let output = result.get("output").unwrap_or(&serde_json::Value::Null);
    let status = output
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .map_or_else(|| "unknown".to_owned(), |status| status.to_string());
    format!("[bash end: {command}] status {status}")
}

fn format_exec_usage(record: &serde_json::Value) -> String {
    let usage = record.get("usage").unwrap_or(&serde_json::Value::Null);
    let input = usage
        .get("non_cached_input_tokens")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .zip(
                    usage
                        .get("cache_read_input_tokens")
                        .and_then(serde_json::Value::as_u64),
                )
                .map(|(input, cached)| input.saturating_sub(cached))
        })
        .or_else(|| {
            usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
        });
    let tokens = input
        .zip(
            usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64),
        )
        .map_or_else(
            || "unknown".to_owned(),
            |(input, output)| (input + output).to_string(),
        );
    format!("Usage updated: {tokens} tokens{}", format_exec_cost(usage))
}

fn format_exec_completion(record: &serde_json::Value) -> String {
    let stats = record.get("stats").unwrap_or(&serde_json::Value::Null);
    let messages = stats
        .get("message_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let plural = if messages == 1 { "" } else { "s" };
    let usage = stats
        .get("usage")
        .cloned()
        .and_then(|value| serde_json::from_value::<SessionUsage>(value).ok())
        .unwrap_or_default();
    format!(
        "Completed: {messages} message{plural}, {}{}",
        format_session_tokens(&usage),
        format_exec_cost(stats)
    )
}

fn format_exec_cost(value: &serde_json::Value) -> String {
    let cost = value.get("cost").unwrap_or(value);
    let amount = cost.get("total_cost").and_then(serde_json::Value::as_str);
    let Some(amount) = amount else {
        return String::new();
    };
    let amount = cagent_agent::presentation::format_currency(
        amount,
        cagent_agent::presentation::CurrencyFormat::Long,
    );
    let currency = cost
        .get("currency")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let estimated = value
        .get("cost_source")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|source| source != "provider_reported");
    let prefix = if estimated { "~" } else { "" };
    if currency == "USD" {
        format!(", {prefix}${amount}")
    } else if currency.is_empty() {
        format!(", {prefix}{amount}")
    } else {
        format!(", {prefix}{amount} {currency}")
    }
}

fn load_exec_schema(command: &ExecCommand) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    if command.output == ExecOutputMode::Structured
        && command.output_schema.is_none()
        && command.output_schema_file.is_none()
    {
        return Err(exec_error(
            2,
            "--output structured requires --output-schema or --output-schema-file",
        ));
    }
    if command.delta && command.output != ExecOutputMode::Json {
        return Err(exec_error(2, "--delta requires --output json"));
    }
    let raw = if let Some(schema) = command.output_schema.as_deref() {
        schema.to_owned()
    } else if let Some(path) = command.output_schema_file.as_ref() {
        std::fs::read_to_string(path).map_err(|error| {
            exec_error(
                2,
                format!("could not read output schema {}: {error}", path.display()),
            )
        })?
    } else {
        return Ok(None);
    };
    let schema = serde_json::from_str::<Value>(&raw)
        .map_err(|error| exec_error(2, format!("invalid output schema JSON: {error}")))?;
    cagent_agent::provider::StructuredOutputRequest::validate_schema(&schema)
        .map_err(|error| exec_error(2, error))?;
    Ok(Some(schema))
}

fn load_exec_tool_policy(
    command: &ExecCommand,
) -> Result<Option<ToolPolicy>, Box<dyn std::error::Error>> {
    fn normalize(
        values: &[String],
        option: &str,
    ) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
        let mut normalized = BTreeSet::new();
        for value in values {
            let value = value.trim();
            if value.is_empty() {
                return Err(exec_error(
                    2,
                    format!("{option} tool name must not be empty"),
                ));
            }
            normalized.insert(value.to_owned());
        }
        Ok(normalized)
    }

    let allow = normalize(&command.allow_tools, "--allow-tool")?;
    let deny = normalize(&command.deny_tools, "--deny-tool")?;
    let allow = (!allow.is_empty()).then_some(allow);
    if allow.is_none() && deny.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ToolPolicy { allow, deny }))
    }
}

fn apply_exec_fast_override(
    config: ConfigStore,
    fast: bool,
) -> Result<ConfigStore, cagent_agent::runtime::RuntimeError> {
    if !fast || config.snapshot().fast() {
        return Ok(config);
    }
    let config = ConfigStore::in_memory(config.snapshot());
    config.persist_fast(true)?;
    Ok(config)
}

fn run_exec_impl(mut cli: Cli, command: ExecCommand) -> Result<(), Box<dyn std::error::Error>> {
    let prompt = if let Some(prompt) = command.prompt.clone().or_else(|| cli.initial_prompt.take())
    {
        prompt
    } else {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        if input.is_empty() {
            return Err(exec_error(
                2,
                "exec requires a prompt argument or UTF-8 stdin input",
            ));
        }
        input
    };
    if prompt.trim().is_empty() {
        return Err(exec_error(2, "exec prompt must not be empty"));
    }
    let output_schema = load_exec_schema(&command)?;
    let tool_policy = load_exec_tool_policy(&command)?;

    let workspace = std::env::current_dir()?.canonicalize()?;
    let paths = AppPaths::resolve(PathOverrides {
        config_file: cli.config.take(),
        data_dir: cli.data_dir.take(),
    })?;
    paths.create_directories()?;
    let _diagnostic_guard = diagnostic::init_logging(&paths.data_dir)?;
    let permissions =
        cagent_agent::permissions::PermissionFile::new(paths.permissions_file.clone(), &workspace)?;
    if !cli.trust && !permissions.is_trusted()? {
        return Err(exec_error(
            3,
            format!(
                "workspace is not trusted: {} (rerun with --trust)",
                workspace.display()
            ),
        ));
    }

    let config = apply_exec_fast_override(ConfigStore::open(&paths.config_file)?, command.fast)?;
    let config_snapshot = config.snapshot();
    let selected_mode = cli
        .mode
        .as_deref()
        .unwrap_or(config_snapshot.default_mode());
    let mode = config_snapshot
        .enabled_mode(selected_mode)
        .map_err(|error| exec_error(2, error.to_string()))?;
    if mode.plan {
        return Err(exec_error(2, "exec does not support planning modes"));
    }
    let ephemeral_store = command.no_session.then(tempfile::tempdir).transpose()?;
    let storage_dir = ephemeral_store.as_ref().map_or_else(
        || paths.data_dir.clone(),
        |directory| directory.path().to_path_buf(),
    );
    let async_runtime = tokio::runtime::Runtime::new()?;
    async_runtime.block_on(async move {
        let mut options = RuntimeOptions::new(storage_dir)
            .with_credential_dir(paths.data_dir.clone())
            .with_models_dev_cache_path(paths.data_dir.join("models.json"))
            .with_config(config)
            .with_instruction_paths(paths.global_instruction_dir.clone(), workspace.clone())
            .with_permissions_file(paths.permissions_file);
        if command.no_session {
            options = options
                .with_global_storage_dir(paths.data_dir.clone())
                .without_session_persistence();
        }
        if cli.trust {
            options = options.with_temporary_workspace_trust();
        }
        let runtime = AgentRuntime::open(options).await?;
        let session = runtime
            .create_session_named(
                NewSession {
                    workspace: workspace.clone(),
                },
                command
                    .name
                    .clone()
                    .or_else(|| (!cli.name.is_empty()).then(|| cli.name.join(" "))),
            )
            .await?;
        if let Some(name) = cli.worktree.as_deref() {
            session
                .enter_worktree(cagent_agent::runtime::worktrees::EnterWorktreeRequest {
                    name: (!name.is_empty()).then(|| name.to_owned()),
                    path: None,
                    base: None,
                })
                .await?;
        }
        if cli.archive && !command.no_session {
            runtime.set_conversation_archived(session.id(), true).await?;
        }
        session.wait_for_startup_resources().await;
        if let Some(agent) = cli.agent.as_deref() {
            session.set_session_agent(agent.to_owned()).await?;
        }
        if let Some(mode) = cli.mode.as_deref() {
            session.set_session_mode(mode.to_owned()).await?;
        }
        let allow_disabled_provider = if let Some(provider) = cli.provider.as_deref() {
            let availability = runtime.provider_availability(provider).await?;
            !availability.enabled
                && availability.descriptor.credential_environment_variable.is_some()
                && matches!(availability.auth, cagent_agent::provider::AuthState::Available { .. })
        } else {
            false
        };
        if let Some(selection) = session_model_override(
            &session,
            cli.provider.as_deref(),
            cli.model.as_deref(),
            cli.effort.as_deref(),
            &runtime.config_snapshot(),
            allow_disabled_provider,
        )
        .await? {
            if allow_disabled_provider {
                session
                    .set_session_exec_model(selection.provider, selection.model, selection.effort)
                    .await?;
            } else {
                session
                    .set_session_model(selection.provider, selection.model, selection.effort)
                    .await?;
            }
        }

        let model_selection = session.model_selection().await?;
        let snapshot = session.attach().await?.snapshot;
        let active_workspace = snapshot.cwd;
        let mut output = ExecOutput::new(command.output);
        emit_exec_header(
            &output,
            &active_workspace,
            session.id(),
            model_selection.as_ref(),
        )?;
        emit_exec(&mut output, "started", serde_json::json!({
            "session_id": session.id(),
            "workspace": active_workspace,
            "persisted": !command.no_session,
            "provider": model_selection.as_ref().map(|selection| selection.0.as_str()),
            "model": model_selection.as_ref().map(|selection| selection.1.as_str()),
            "effort": model_selection.as_ref().and_then(|selection| selection.2.as_deref()),
            "fast": snapshot.fast,
            "fast_effective": snapshot.fast_effective,
        }))?;
        let execution_started_at = Instant::now();
        let mut events = session.subscribe(None);
        let submit = SessionCommand::submit_exec_input(
            prompt.clone(),
            output_schema.clone(),
            tool_policy.clone().unwrap_or_default(),
        );
        session.submit(submit).await?;
        emit_exec(&mut output, "user_message", serde_json::json!({"text": prompt}))?;
        let mut assistant_text = String::new();
        let mut structured_value = None::<Value>;
        let mut exit_code = 0;
        while let Some(event) = events.next().await {
            match event? {
                RuntimeEvent::Durable(event) => match event.kind {
                    DurableEventKind::NodeAppended { node_id, owner_id, node_kind, status, content, .. } => {
                        match node_kind {
                            cagent_agent::protocol::NodeKind::ToolCall => emit_exec(&mut output, "tool_started", serde_json::json!({"node_id": node_id, "status": status, "tool": content}))?,
                            cagent_agent::protocol::NodeKind::ToolResult => emit_exec(&mut output, "tool_completed", serde_json::json!({"node_id": node_id, "call_node_id": owner_id, "status": status, "result": content}))?,
                            _ => {}
                        }
                    }
                    DurableEventKind::AssistantDelta { node_id, delta, .. } => {
                        assistant_text.push_str(&delta);
                        if (command.output == ExecOutputMode::Json && command.delta)
                            || (command.output == ExecOutputMode::Text && output_schema.is_none())
                        {
                            emit_exec(&mut output, "assistant_delta", serde_json::json!({"node_id": node_id, "delta": delta}))?;
                        }
                    }
                    DurableEventKind::AssistantFailed { code, message, .. } => {
                        emit_exec(&mut output, "error", serde_json::json!({"code": code, "message": message}))?;
                        exit_code = if code == "unsupported_structured_output" { 2 } else { 1 };
                        break;
                    }
                    DurableEventKind::ModelUsageRecorded { node_id, usage } => emit_exec(&mut output, "usage", serde_json::json!({"node_id": node_id, "usage": usage}))?,
                    _ => {}
                },
                RuntimeEvent::Transient { event: TransientEvent::TurnFailed { message }, .. } => {
                    emit_exec(&mut output, "error", serde_json::json!({"message": message}))?;
                    exit_code = 1;
                    break;
                }
                RuntimeEvent::Transient { event: TransientEvent::StructuredOutput { value }, .. } => {
                    structured_value = Some(value);
                }
                RuntimeEvent::Transient { event: TransientEvent::TurnCompleted, .. } => break,
                RuntimeEvent::Interaction { request, .. } => {
                    let request = *request;
                    let request_id = request.id;
                    match request.kind {
                    InteractionRequestKind::PermissionApproval { resource, message, .. } => {
                        emit_exec(&mut output, "permission_denied", serde_json::json!({"resource": resource, "message": message}))?;
                        session.submit(SessionCommand::new(SessionAction::RespondToInteraction { request_id, response: serde_json::json!({"decision": "deny"}) })).await?;
                    }
                    InteractionRequestKind::Question { .. } | InteractionRequestKind::PlanCompletion { .. } => {
                        emit_exec(&mut output, "error", serde_json::json!({"message": "headless exec does not support interactive questions or plans"}))?;
                        session.cancel();
                        exit_code = 1;
                        break;
                    }
                    }
                }
                _ => {}
            }
        }
        let stats = session.stats().await?;
        if command.output == ExecOutputMode::Json
            && output_schema.is_none()
            && !command.delta
            && !assistant_text.is_empty()
        {
            emit_exec(&mut output, "assistant_message", serde_json::json!({"text": assistant_text}))?;
        }
        if !output.silent() {
            emit_exec(&mut output, "completed", serde_json::json!({
                "session_id": (!command.no_session).then(|| session.id()),
                "usage": stats.usage.clone(), "stats": stats, "exit_status": exit_code,
                "elapsed_ms": execution_started_at.elapsed().as_millis()
            }))?;
        }
        if exit_code == 0 && output_schema.is_some() {
            let value = structured_value.ok_or_else(|| {
                exec_error(1, "structured output completed without a validated result")
            })?;
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer(&mut stdout, &value)?;
            writeln!(stdout)?;
        }
        if exit_code == 0 {
            Ok(())
        } else {
            // The failure was already emitted as an exec event. Return only
            // the exit status so `main` does not print a duplicate summary.
            Err(exec_error(exit_code, ""))
        }
    })
}

/* fn run_config_impl(
    config_file: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    command: ConfigCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::resolve(PathOverrides {
        config_file,
        data_dir,
    })?;
    let mutating = matches!(command, ConfigCommand::Set { .. } | ConfigCommand::Edit);
    if mutating {
        paths.create_directories()?;
    }
    let config = ConfigStore::open(&paths.config_file)?;
    match command {
        ConfigCommand::View { safe } => {
            let source = config.source()?;
            if safe {
                print_config_source(&redact_config_secrets(&source));
            } else {
                print_config_source(&source);
            }
        }
        ConfigCommand::Get { key } => {
            let source = config.source()?;
            if cagent_agent::config::config_selection_is_secret(&source, &key) {
                return Err(format!("config key cannot be displayed safely: {key}").into());
            }
            let value = config
                .get_value(&key)?
                .ok_or_else(|| format!("config key is not set: {key}"))?;
            println!("{value}");
        }
        ConfigCommand::Set { key, value } => {
            config.set_value(&key, &value)?;
            println!("Set {key} to {value}");
        }
        ConfigCommand::Edit => {
            if !paths.config_file.exists() {
                std::fs::write(&paths.config_file, "version = 1\n")?;
            }
            let editor = std::env::var_os("VISUAL")
                .or_else(|| std::env::var_os("EDITOR"))
                .ok_or("set $VISUAL or $EDITOR before using config edit")?;
            let editor = editor.to_string_lossy();
            let mut command = editor.split_whitespace();
            let executable = command
                .next()
                .ok_or("$VISUAL or $EDITOR must name an executable")?;
            let status = std::process::Command::new(executable)
                .args(command)
                .arg(&paths.config_file)
                .status()?;
            if !status.success() {
                return Err(format!("editor exited with {status}").into());
            }
            config.reload()?;
        }
        ConfigCommand::List => {
            for key in config.keys()? {
                println!("{key}");
            }
        }
    }
    Ok(())
} */

#[allow(dead_code)] // Retained for the JSON config command path.
fn print_config_source(source: &str) {
    let use_color = text_output_uses_color(std::io::stdout().is_terminal());
    for token in cagent_agent::presentation::highlight_code("toml", source) {
        if !use_color {
            print!("{}", token.text);
            continue;
        }
        let color = match token.kind {
            cagent_agent::presentation::CodeTokenKind::Comment => crossterm::style::Color::DarkGrey,
            cagent_agent::presentation::CodeTokenKind::String => crossterm::style::Color::Green,
            cagent_agent::presentation::CodeTokenKind::Number => crossterm::style::Color::Yellow,
            cagent_agent::presentation::CodeTokenKind::Keyword
            | cagent_agent::presentation::CodeTokenKind::Attribute => crossterm::style::Color::Blue,
            cagent_agent::presentation::CodeTokenKind::Constant => crossterm::style::Color::Cyan,
            _ => crossterm::style::Color::Reset,
        };
        print!("{}", token.text.with(color));
    }
}

#[allow(dead_code)] // Retained for the JSON config command path.
fn redact_config_secrets(source: &str) -> String {
    let Ok(mut document) = source.parse::<toml_edit::DocumentMut>() else {
        return "# Configuration could not be safely displayed.\n".into();
    };
    redact_config_table(document.as_table_mut());
    document.to_string()
}

#[allow(dead_code)] // Retained for the JSON config command path.
fn redact_config_table(table: &mut toml_edit::Table) {
    for (key, item) in table.iter_mut() {
        if config_key_is_secret(key.get()) {
            if item.is_value() {
                *item = toml_edit::value("<redacted>");
            } else if let Some(table) = item.as_table_mut() {
                redact_entire_config_table(table);
            }
            continue;
        }
        if let Some(table) = item.as_table_mut() {
            redact_config_table(table);
        } else if let Some(array) = item.as_array_of_tables_mut() {
            for table in array.iter_mut() {
                redact_config_table(table);
            }
        }
    }
}

#[allow(dead_code)] // Retained for the JSON config command path.
fn redact_entire_config_table(table: &mut toml_edit::Table) {
    for (_, item) in table.iter_mut() {
        if item.is_value() {
            *item = toml_edit::value("<redacted>");
        } else if let Some(table) = item.as_table_mut() {
            redact_entire_config_table(table);
        }
    }
}

#[allow(dead_code)] // Retained for the JSON config command path.
fn config_key_is_secret(key: &str) -> bool {
    cagent_agent::config::config_key_is_secret(key)
}

fn text_output_uses_color(stdout_is_terminal: bool) -> bool {
    text_output_uses_color_for(
        stdout_is_terminal,
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
    )
}

fn text_output_uses_color_for(
    stdout_is_terminal: bool,
    no_color: Option<&std::ffi::OsStr>,
    terminal: Option<&std::ffi::OsStr>,
) -> bool {
    stdout_is_terminal && no_color.is_none() && terminal != Some(std::ffi::OsStr::new("dumb"))
}

#[derive(Debug, Eq, PartialEq)]
struct SessionModelOverride {
    provider: String,
    model: String,
    effort: Option<String>,
}

async fn session_model_override(
    session: &cagent_agent::runtime::SessionHandle,
    provider: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    config: &cagent_agent::config::ConfigSnapshot,
    allow_disabled_provider: bool,
) -> Result<Option<SessionModelOverride>, Box<dyn std::error::Error>> {
    let model_input = match (provider, model, effort) {
        (None, None, None) => return Ok(None),
        (Some(provider), None, _) => {
            if provider.trim().is_empty() {
                return Err("--provider must not be empty".into());
            }
            let Some(default_model) = config.default_model() else {
                return Err(
                    "--provider requires --model when no default model is configured".into(),
                );
            };
            let Some(cagent_agent::config::ModelTarget::Model(default_model)) =
                default_model.target.as_ref()
            else {
                return Err("--provider requires --model when default_model uses a tier".into());
            };
            if default_model.provider != provider {
                return Err(format!(
                    "--provider {provider} requires --model because the configured default belongs to {}",
                    default_model.provider
                ).into());
            }
            format!("{provider}/{}", default_model.model)
        }
        (None, Some(model), _) => {
            if model.trim().is_empty() {
                return Err("--model must not be empty".into());
            }
            model.to_owned()
        }
        (Some(provider), Some(model), _) => {
            if provider.trim().is_empty() {
                return Err("--provider must not be empty".into());
            }
            if model.trim().is_empty() || model.contains('/') {
                return Err(
                    "--model must be a non-empty model ID without a provider prefix".into(),
                );
            }
            format!("{provider}/{model}")
        }
        (None, None, Some(_)) => {
            let Some(default_model) = config.default_model() else {
                return Err("--effort requires a configured default model".into());
            };
            let Some(cagent_agent::config::ModelTarget::Model(default_model)) =
                default_model.target.as_ref()
            else {
                return Err("--effort requires --model when default_model uses a tier".into());
            };
            format!("{}/{}", default_model.provider, default_model.model)
        }
    };

    let row = if allow_disabled_provider {
        session
            .resolve_model_picker_row_for_provider(
                provider.expect("provider was supplied"),
                &model_input,
            )
            .await?
    } else {
        session.resolve_model_picker_row(&model_input).await?
    }
    .ok_or_else(|| format!("model argument did not match any enabled model: {model_input}"))?;
    let effort = effort
        .map(|requested| {
            let requested = requested.trim();
            if requested.is_empty() {
                return Err("--effort must not be empty".to_owned());
            }
            row.efforts
                .iter()
                .find(|available| available.eq_ignore_ascii_case(requested))
                .cloned()
                .ok_or_else(|| {
                    if row.efforts.is_empty() {
                        format!(
                            "model {}/{} does not support --effort",
                            row.provider, row.id
                        )
                    } else {
                        format!(
                            "unsupported --effort {requested} for {}/{} (choose from: {})",
                            row.provider,
                            row.id,
                            row.efforts.join(", ")
                        )
                    }
                })
        })
        .transpose()?;

    Ok(Some(SessionModelOverride {
        provider: row.provider,
        model: row.id,
        effort,
    }))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // CLI tests are kept near their command helpers.
mod tests {
    use super::*;
    use crate::diagnostic::{
        DEFAULT_LOG_FILTER, DIAGNOSTIC_LOG_BYTES, DiagnosticLog, logging_filter,
    };

    #[test]
    fn diagnostic_rows_include_session_id_from_their_context() {
        #[derive(Clone)]
        struct TestWriter(std::sync::Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for TestWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("diagnostic output lock")
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for TestWriter {
            type Writer = Self;

            fn make_writer(&'writer self) -> Self::Writer {
                self.clone()
            }
        }

        let output = std::sync::Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_env_filter(tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER))
            .with_writer(TestWriter(output.clone()))
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "cagent_cli",
                "agent.session",
                session_id = "cagent-test-session"
            );
            let _entered = span.enter();
            tracing::info!(target: "cagent_cli", phase = "test", "session diagnostic");
        });

        let bytes = output.lock().expect("diagnostic output lock").clone();
        let row = String::from_utf8(bytes).expect("diagnostic output should be UTF-8");
        assert!(row.contains("session_id=\"cagent-test-session\""), "{row}");
        assert!(row.contains("session diagnostic"), "{row}");
    }

    #[test]
    fn logging_filter_uses_default_without_environment_overrides() {
        assert_safe_logging_filter(logging_filter(None, None), DEFAULT_LOG_FILTER);
    }

    #[test]
    fn logging_filter_supports_rust_log() {
        assert_safe_logging_filter(
            logging_filter(None, Some("cagent_agent=debug")),
            "cagent_agent=debug",
        );
    }

    #[test]
    fn logging_filter_prefers_cagent_log() {
        assert_safe_logging_filter(
            logging_filter(Some("cagent_cli=trace"), Some("warn")),
            "cagent_cli=trace",
        );
    }

    #[test]
    fn logging_filter_skips_empty_and_invalid_values() {
        assert_safe_logging_filter(
            logging_filter(Some("  "), Some("cagent_agent=debug")),
            "cagent_agent=debug",
        );
        assert_safe_logging_filter(logging_filter(Some("["), Some("warn")), "warn");
        assert_safe_logging_filter(logging_filter(Some("["), Some("[")), DEFAULT_LOG_FILTER);
    }

    fn assert_safe_logging_filter(filter: tracing_subscriber::EnvFilter, selected: &str) {
        let rendered = filter.to_string();
        for directive in selected.split(',') {
            assert!(rendered.contains(directive), "{rendered}");
        }
        for target in [
            "tokenize=info",
            "parse=info",
            "expansion=info",
            "notify=info",
        ] {
            assert!(rendered.contains(target), "{rendered}");
        }
    }

    #[test]
    fn diagnostic_log_rotates_and_keeps_bounded_archives() {
        use std::io::Write as _;

        let temporary = tempfile::TempDir::new().unwrap();
        let mut log = DiagnosticLog::open(temporary.path()).unwrap();
        log.write_all(&vec![b'x'; DIAGNOSTIC_LOG_BYTES as usize])
            .unwrap();
        log.write_all(b"next file").unwrap();
        log.flush().unwrap();

        assert_eq!(
            std::fs::metadata(temporary.path().join("diagnostic.log.1"))
                .unwrap()
                .len(),
            DIAGNOSTIC_LOG_BYTES
        );
        assert_eq!(
            std::fs::read(temporary.path().join("diagnostic.log")).unwrap(),
            b"next file"
        );
    }

    #[test]
    fn config_view_color_requires_a_terminal_and_no_color_to_be_unset() {
        assert!(text_output_uses_color_for(
            true,
            None,
            Some(std::ffi::OsStr::new("xterm"))
        ));
        assert!(!text_output_uses_color_for(
            false,
            None,
            Some(std::ffi::OsStr::new("xterm"))
        ));
        assert!(!text_output_uses_color_for(
            true,
            Some(std::ffi::OsStr::new("")),
            Some(std::ffi::OsStr::new("xterm"))
        ));
        assert!(!text_output_uses_color_for(
            true,
            Some(std::ffi::OsStr::new("1")),
            Some(std::ffi::OsStr::new("xterm"))
        ));
        assert!(!text_output_uses_color_for(
            true,
            None,
            Some(std::ffi::OsStr::new("dumb"))
        ));
    }

    #[test]
    fn onboarding_requires_a_missing_config_and_plain_interactive_launch() {
        let missing = Path::new("/tmp/cagent-onboarding-test-config-does-not-exist.toml");
        assert!(should_show_onboarding(missing, None, None, None, None));
        let empty = tempfile::NamedTempFile::new().unwrap();
        assert!(should_show_onboarding(empty.path(), None, None, None, None));
        assert!(!should_show_onboarding(
            Path::new("Cargo.toml"),
            None,
            None,
            None,
            None
        ));
        assert!(!should_show_onboarding(
            missing,
            Some(&CliCommand::Continue),
            None,
            None,
            None
        ));
        assert!(!should_show_onboarding(
            missing,
            None,
            Some("openai"),
            None,
            None
        ));
        assert!(!should_show_onboarding(
            missing,
            None,
            None,
            Some("gpt-5"),
            None
        ));
        assert!(!should_show_onboarding(
            missing,
            None,
            None,
            None,
            Some("high")
        ));
    }

    #[test]
    fn mcp_dispatch_preserves_resume_and_continue_commands() {
        for command in [CliCommand::Resume { id: None }, CliCommand::Continue] {
            let mut command = Some(command);
            assert!(take_mcp_command(&mut command).is_none());
            assert!(matches!(
                command,
                Some(CliCommand::Resume { id: None }) | Some(CliCommand::Continue)
            ));
        }
    }

    #[test]
    fn config_dispatch_preserves_session_commands() {
        let mut command = Some(CliCommand::Config {
            command: ConfigCommand::List,
        });
        assert!(matches!(
            take_config_command(&mut command),
            Some(ConfigCommand::List)
        ));
        assert!(command.is_none());

        let mut command = Some(CliCommand::Continue);
        assert!(take_config_command(&mut command).is_none());
        assert!(matches!(command, Some(CliCommand::Continue)));
    }

    #[test]
    fn exec_cli_accepts_output_mode_no_session_and_fast() {
        let cli = Cli::try_parse_from([
            "cagent",
            "--trust",
            "exec",
            "--output",
            "json",
            "--no-session",
            "--fast",
            "--name",
            "Audit",
            "describe the workspace",
        ])
        .unwrap();
        assert!(cli.trust);
        assert_eq!(cli.initial_prompt, None);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Exec {
                output: ExecOutputMode::Json,
                delta: false,
                output_schema: None,
                output_schema_file: None,
                allow_tools,
                deny_tools,
                no_session: true,
                fast: true,
                prompt: Some(prompt),
                name,
            }) if allow_tools.is_empty()
                && deny_tools.is_empty()
                && prompt == "describe the workspace"
                && name.as_deref() == Some("Audit")
        ));
    }

    #[test]
    fn resume_cli_accepts_a_multi_word_title() {
        let cli = Cli::try_parse_from(["cagent", "resume", "scroll up"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Resume { id: Some(selector) }) if selector == "scroll up"
        ));
    }

    #[test]
    fn worktree_flag_accepts_an_omitted_or_explicit_name() {
        let generated = Cli::try_parse_from(["cagent", "--worktree"]).unwrap();
        assert_eq!(generated.worktree.as_deref(), Some(""));

        let generated_exec =
            Cli::try_parse_from(["cagent", "--worktree", "exec", "inspect"]).unwrap();
        assert_eq!(generated_exec.worktree.as_deref(), Some(""));
        assert!(matches!(
            generated_exec.command,
            Some(CliCommand::Exec { .. })
        ));

        let named = Cli::try_parse_from(["cagent", "--worktree=topic", "exec", "inspect"]).unwrap();
        assert_eq!(named.worktree.as_deref(), Some("topic"));
        assert!(matches!(named.command, Some(CliCommand::Exec { .. })));

        for arguments in [
            ["cagent", "--worktree=topic", "continue"],
            ["cagent", "continue", "--worktree=topic"],
        ] {
            let continued = Cli::try_parse_from(arguments).unwrap();
            assert_eq!(continued.worktree.as_deref(), Some("topic"));
            assert!(matches!(continued.command, Some(CliCommand::Continue)));
        }
        for arguments in [
            ["cagent", "-w", "topic", "continue"],
            ["cagent", "continue", "-w", "topic"],
        ] {
            let continued = Cli::try_parse_from(arguments).unwrap();
            assert_eq!(continued.worktree.as_deref(), Some("topic"));
            assert!(matches!(continued.command, Some(CliCommand::Continue)));
        }
    }

    #[test]
    fn exec_cli_accepts_archive_and_rejects_no_session_combination() {
        let cli =
            Cli::try_parse_from(["cagent", "exec", "--archive", "inspect the workspace"]).unwrap();
        assert!(cli.archive);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Exec {
                no_session: false,
                prompt: Some(prompt),
                ..
            }) if prompt == "inspect the workspace"
        ));

        assert!(
            Cli::try_parse_from(["cagent", "exec", "--no-session", "--archive", "prompt",])
                .is_err()
        );
    }

    #[test]
    fn archive_is_available_without_exec() {
        let cli = Cli::try_parse_from(["cagent", "--archive", "Review session"]).unwrap();
        assert!(cli.archive);
        assert!(cli.command.is_none());
        assert_eq!(cli.name, ["Review session"]);
    }

    #[test]
    fn exec_accepts_a_direct_prompt_and_optional_name() {
        let cli = Cli::try_parse_from([
            "cagent",
            "exec",
            "inspect the implementation",
            "--name",
            "Usage investigation",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Exec {
                prompt: Some(prompt),
                name: Some(name),
                ..
            }) if prompt == "inspect the implementation" && name == "Usage investigation"
        ));
    }

    #[test]
    fn name_is_positional_and_prompt_requires_a_flag() {
        let cli = Cli::try_parse_from(["cagent", "Fix", "the", "bug"]).unwrap();
        assert_eq!(cli.name, vec!["Fix", "the", "bug"]);
        assert_eq!(cli.initial_prompt, None);

        let cli =
            Cli::try_parse_from(["cagent", "Fix", "the", "bug", "--prompt", "Initial prompt"])
                .unwrap();
        assert_eq!(cli.name, vec!["Fix", "the", "bug"]);
        assert_eq!(cli.initial_prompt.as_deref(), Some("Initial prompt"));
    }

    #[test]
    fn obsolete_json_flag_is_rejected() {
        assert!(Cli::try_parse_from(["cagent", "exec", "--json", "prompt"]).is_err());
    }

    #[test]
    fn exec_cli_accepts_repeated_tool_allow_and_deny_options() {
        let cli = Cli::try_parse_from([
            "cagent",
            "exec",
            "--allow-tool",
            "read",
            "--allow-tool",
            "grep",
            "--deny-tool",
            "grep",
            "prompt",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Exec {
                allow_tools,
                deny_tools,
                ..
            }) if allow_tools == vec!["read", "grep"] && deny_tools == vec!["grep"]
        ));
    }

    #[test]
    fn exec_tool_policy_deny_wins_and_empty_names_fail() {
        let command = ExecCommand {
            output: ExecOutputMode::Text,
            delta: false,
            output_schema: None,
            output_schema_file: None,
            allow_tools: vec![" read ".into(), "grep".into()],
            deny_tools: vec!["grep".into()],
            no_session: true,
            fast: false,
            prompt: None,
            name: None,
        };
        let policy = load_exec_tool_policy(&command).unwrap().unwrap();
        assert!(policy.allows("read"));
        assert!(!policy.allows("grep"));
        assert!(!policy.allows("bash"));

        let mut invalid = command;
        invalid.allow_tools = vec![" ".into()];
        assert_eq!(
            load_exec_tool_policy(&invalid)
                .unwrap_err()
                .downcast_ref::<ExecExitError>()
                .unwrap()
                .code,
            2
        );
    }

    #[test]
    fn exec_fast_override_does_not_change_the_source_store() {
        let source = ConfigStore::in_memory(cagent_agent::config::ConfigSnapshot::default());
        let overridden = apply_exec_fast_override(source.clone(), true).unwrap();

        assert!(overridden.snapshot().fast());
        assert!(!source.snapshot().fast());
    }

    #[test]
    fn exec_usage_excludes_cache_reads_from_display_total() {
        let record = serde_json::json!({
            "usage": {
                "input_tokens": 40_000,
                "non_cached_input_tokens": 10_000,
                "cache_read_input_tokens": 30_000,
                "output_tokens": 8_000,
                "total_tokens": 48_000
            }
        });

        assert_eq!(format_exec_usage(&record), "Usage updated: 18000 tokens");
    }

    #[test]
    fn session_summary_tokens_match_statusline_format() {
        let mut usage = cagent_agent::protocol::SessionUsage::default();
        usage.input_tokens = Some(40_000);
        usage.non_cached_input_tokens = Some(10_000);
        usage.cache_read_input_tokens = Some(30_000);
        usage.output_tokens = Some(1_234_000);
        assert_eq!(
            format_session_tokens(&usage),
            "in:10k cache-read:30k out:1.23M"
        );
    }

    #[test]
    fn interactive_session_summary_puts_the_title_on_its_own_heading() {
        let mut usage = cagent_agent::protocol::SessionUsage::default();
        usage.input_tokens = Some(1_080_600);
        usage.non_cached_input_tokens = Some(327_960);
        usage.cache_read_input_tokens = Some(752_640);
        usage.output_tokens = Some(14_100);
        let stats = cagent_agent::protocol::SessionStats {
            id: cagent_agent::protocol::ConversationId::new(),
            title: Some("Greeting".into()),
            message_count: 45,
            total_tokens: 1_094_700,
            total_cost: Some("0.106".into()),
            currency: Some("USD".into()),
            usage,
            cost_source: Some(cagent_agent::protocol::CostSource::ProviderCatalog),
        };

        assert_eq!(format_session_heading(&stats), "Cagent session: Greeting");
        assert_eq!(
            format_session_workspace(Path::new("/projects/cagent"), Path::new("/projects/cagent")),
            None
        );
        assert_eq!(
            format_session_details(&stats),
            "45 messages, in:327.96k cache-read:752.64k out:14.1k, ~$0.106"
        );
    }

    #[test]
    fn session_summary_workspace_is_relative_to_the_launch_directory() {
        assert_eq!(
            format_session_workspace(
                Path::new("/projects/cagent"),
                Path::new("/projects/cagent/crates/cagent-agent"),
            ),
            Some("crates/cagent-agent".into())
        );
        assert_eq!(
            format_session_workspace(Path::new("/projects/cagent"), Path::new("/projects/other")),
            Some("../other".into())
        );
        assert_eq!(
            format_session_workspace(Path::new("relative"), Path::new("/projects/other")),
            Some("/projects/other".into())
        );
    }

    #[test]
    fn empty_sessions_do_not_print_an_exit_message() {
        let stats = cagent_agent::protocol::SessionStats {
            id: cagent_agent::protocol::ConversationId::new(),
            title: None,
            message_count: 0,
            total_tokens: 0,
            total_cost: None,
            currency: None,
            usage: cagent_agent::protocol::SessionUsage::default(),
            cost_source: None,
        };

        assert!(!should_print_session_exit_message(&stats));
    }

    #[test]
    fn non_empty_sessions_print_an_exit_message() {
        let stats = cagent_agent::protocol::SessionStats {
            id: cagent_agent::protocol::ConversationId::new(),
            title: None,
            message_count: 1,
            total_tokens: 0,
            total_cost: None,
            currency: None,
            usage: cagent_agent::protocol::SessionUsage::default(),
            cost_source: None,
        };

        assert!(should_print_session_exit_message(&stats));
    }

    #[test]
    fn completed_exec_summary_uses_compact_session_usage_and_cost() {
        let record = serde_json::json!({
            "stats": {
                "message_count": 6,
                "total_tokens": 35_606,
                "total_cost": "0.006",
                "currency": "USD",
                "cost_source": "provider_catalog",
                "usage": {
                    "input_tokens": 34_710,
                    "non_cached_input_tokens": 13_210,
                    "cache_read_input_tokens": 21_500,
                    "output_tokens": 896,
                    "total_tokens": 35_606,
                    "cost_complete": true,
                    "cost_source": "provider_catalog"
                }
            }
        });

        assert_eq!(
            format_exec_completion(&record),
            "Completed: 6 messages, in:13.21k cache-read:21.5k out:896, ~$0.006"
        );
    }

    #[test]
    fn exec_cost_uses_long_fractional_format_below_one_and_short_format_above_it() {
        let fractional = serde_json::json!({
            "total_cost": "0.001001",
            "currency": "USD",
            "cost_source": "provider_reported"
        });
        assert_eq!(format_exec_cost(&fractional), ", $0.001,001");

        let dollar_amount = serde_json::json!({
            "total_cost": "1.324",
            "currency": "USD",
            "cost_source": "provider_reported"
        });
        assert_eq!(format_exec_cost(&dollar_amount), ", $1.32");
    }

    #[test]
    fn structured_output_requires_a_valid_schema_and_delta_requires_json() {
        let structured = ExecCommand {
            output: ExecOutputMode::Structured,
            delta: false,
            output_schema: None,
            output_schema_file: None,
            allow_tools: vec![],
            deny_tools: vec![],
            no_session: true,
            fast: false,
            prompt: None,
            name: None,
        };
        assert_eq!(
            load_exec_schema(&structured)
                .unwrap_err()
                .downcast_ref::<ExecExitError>()
                .unwrap()
                .code,
            2
        );

        let delta = ExecCommand {
            output: ExecOutputMode::Text,
            delta: true,
            output_schema: None,
            output_schema_file: None,
            allow_tools: vec![],
            deny_tools: vec![],
            no_session: true,
            fast: false,
            prompt: None,
            name: None,
        };
        assert_eq!(
            load_exec_schema(&delta)
                .unwrap_err()
                .downcast_ref::<ExecExitError>()
                .unwrap()
                .code,
            2
        );
    }

    #[test]
    fn temporary_workspace_trust_skips_the_interactive_prompt() {
        assert!(!workspace_trust_prompt_required(true, false));
        assert!(!workspace_trust_prompt_required(false, true));
        assert!(workspace_trust_prompt_required(false, false));
    }

    #[test]
    fn text_exec_output_is_rendered_from_the_json_record() {
        let record = serde_json::json!({
            "version": 1,
            "event": "permission_denied",
            "message": "outside workspace"
        });
        assert_eq!(
            format_exec_text_record(&record).0,
            "[Permission denied]: outside workspace"
        );
    }

    #[test]
    fn text_exec_permission_denial_prefers_the_exact_bash_source() {
        let record = serde_json::json!({
            "version": 1,
            "event": "permission_denied",
            "resource": {
                "tool": "bash",
                "raw_command": "echo \"unapproved\"",
                "command": ["echo", "unapproved"]
            },
            "message": "Allow running the following?"
        });
        assert_eq!(
            format_exec_text_record(&record).0,
            "[Permission denied]: echo \"unapproved\""
        );
    }

    #[test]
    fn text_exec_permission_denial_keeps_file_descriptor_redirections_compact() {
        let record = serde_json::json!({
            "version": 1,
            "event": "permission_denied",
            "resource": {
                "tool": "bash",
                "raw_command": "ls TEST.md 2>& 1"
            }
        });
        assert_eq!(
            format_exec_text_record(&record).0,
            "[Permission denied]: ls TEST.md 2>&1"
        );
        assert!(exec_event_uses_raw_activity("permission_denied"));
    }

    #[test]
    fn text_exec_permission_denial_falls_back_to_command_and_path_resources() {
        let command = serde_json::json!({
            "version": 1,
            "event": "permission_denied",
            "resource": {"tool": "bash", "command": ["cargo", "test"]}
        });
        assert_eq!(
            format_exec_text_record(&command).0,
            "[Permission denied]: cargo test"
        );

        let path = serde_json::json!({
            "version": 1,
            "event": "permission_denied",
            "resource": {"tool": "read", "path": "/tmp/secret.txt"}
        });
        assert_eq!(
            format_exec_text_record(&path).0,
            "[Permission denied]: read /tmp/secret.txt"
        );
    }

    #[test]
    fn text_exec_started_output_is_compact() {
        let record = serde_json::json!({
            "version": 1,
            "event": "started",
            "session_id": "session-123"
        });
        assert_eq!(format_exec_text_record(&record).0, "Started");
    }

    #[test]
    fn text_exec_grep_output_includes_the_pattern() {
        let record = serde_json::json!({
            "version": 1,
            "event": "tool_started",
            "tool": {
                "name": "grep",
                "arguments": {
                    "pattern": "sub-agent usage",
                    "paths": ["crates"]
                }
            }
        });
        assert_eq!(
            format_exec_text_record(&record).0,
            "[grep]: sub-agent usage"
        );
    }
}

#[allow(clippy::too_many_lines)]
#[allow(dead_code)] // Retained for the JSON MCP command path.
fn run_mcp_impl(
    config_file: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    command: mcp::McpCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = std::env::current_dir()?.canonicalize()?;
    let paths = AppPaths::resolve(PathOverrides {
        config_file,
        data_dir,
    })?;
    paths.create_directories()?;
    let permissions =
        cagent_agent::permissions::PermissionFile::new(paths.permissions_file.clone(), &workspace)?;
    let trusted = permissions.is_trusted()?;
    let config = ConfigStore::open(&paths.config_file)?;
    let default_agent = config.snapshot().default_agent().to_owned();
    let service =
        cagent_agent::mcp::McpConfigService::from_config_store(config, &workspace, trusted)?;
    match command {
        mcp::McpCommand::Add {
            name,
            scope,
            url,
            json,
            yes,
            command,
        } => {
            let location = mcp_location(scope)?;
            if let Some(path) = json {
                let source = read_json_source(&path)?;
                let preview = service.preview_import_json(location, &source, name.as_deref())?;
                let confirmed = confirm_if_needed(
                    preview.requires_confirmation,
                    yes,
                    "Replace existing MCP definitions?",
                )?;
                let servers = service.apply_import(&default_agent, preview, confirmed)?;
                print_mcp_servers(&servers, false)?;
            } else {
                let name =
                    name.ok_or("mcp add requires NAME unless imported JSON contains names")?;
                let transport = if let Some(url) = url {
                    if !command.is_empty() {
                        return Err("--url cannot be combined with a stdio command".into());
                    }
                    cagent_agent::mcp::McpTransportConfig::StreamableHttp {
                        url,
                        headers: BTreeMap::default(),
                        allow_insecure: false,
                    }
                } else {
                    let (executable, args) = command
                        .split_first()
                        .ok_or("stdio mcp add requires an executable after --")?;
                    cagent_agent::mcp::McpTransportConfig::Stdio {
                        command: executable.clone(),
                        args: args.to_vec(),
                        cwd: None,
                        env: BTreeMap::default(),
                        env_remove: Vec::new(),
                        inherit_env: true,
                    }
                };
                let definition = cagent_agent::mcp::McpServerDefinition {
                    transport,
                    enabled: true,
                    agents: Vec::new(),
                    eager: false,
                    startup_timeout_seconds: 10,
                    request_timeout_seconds: 60,
                    read_only_tools: Vec::new(),
                    ..cagent_agent::mcp::McpServerDefinition::default()
                };
                let preview = service.preview_mutations(
                    &default_agent,
                    vec![cagent_agent::mcp::McpMutation::Put {
                        location,
                        name,
                        definition,
                    }],
                )?;
                let confirmed = confirm_if_needed(
                    preview.requires_confirmation,
                    yes,
                    "Replace the existing MCP definition?",
                )?;
                let servers = service.apply_mutations(&default_agent, preview, confirmed)?;
                print_mcp_servers(&servers, false)?;
            }
        }
        mcp::McpCommand::List { all, json } => {
            let servers = if all {
                service.list_all_effective(&default_agent)?
            } else {
                service.list_effective(&default_agent)?
            };
            print_mcp_servers(&servers, json)?;
        }
        mcp::McpCommand::Get { name, json } => {
            let server = service
                .get_effective(&default_agent, &name)?
                .ok_or_else(|| format!("unknown MCP server: {name}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&server)?);
            } else {
                println!(
                    "{}\t{:?}\t{}\t{:?}",
                    server.name,
                    server.location.scope,
                    if server.definition.enabled && server.allowed_for_agent {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    server.status
                );
            }
        }
        mcp::McpCommand::Enable(target) => {
            let location = mcp_location(target.scope)?;
            let servers = service.set_enabled(&default_agent, location, &target.name, true)?;
            print_mcp_servers(&servers, false)?;
        }
        mcp::McpCommand::Disable(target) => {
            let location = mcp_location(target.scope)?;
            let servers = service.set_enabled(&default_agent, location, &target.name, false)?;
            print_mcp_servers(&servers, false)?;
        }
        mcp::McpCommand::Remove { target, yes } => {
            if !confirm_if_needed(true, yes, "Remove this MCP definition?")? {
                return Ok(());
            }
            let preview = service.preview_mutations(
                &default_agent,
                vec![cagent_agent::mcp::McpMutation::Remove {
                    location: mcp_location(target.scope)?,
                    name: target.name,
                }],
            )?;
            let servers = service.apply_mutations(&default_agent, preview, true)?;
            print_mcp_servers(&servers, false)?;
        }
        _ => {
            return Err("this MCP command is handled by the dedicated MCP command runner".into());
        }
    }
    Ok(())
}

#[allow(dead_code)] // Retained for the JSON MCP command path.
fn mcp_location(
    scope: mcp::CliMcpScope,
) -> Result<cagent_agent::mcp::McpLocation, Box<dyn std::error::Error>> {
    Ok(match scope {
        mcp::CliMcpScope::Global => cagent_agent::mcp::McpLocation::global(),
        mcp::CliMcpScope::Project => cagent_agent::mcp::McpLocation::project(),
    })
}

#[allow(dead_code)] // Retained for the JSON MCP command path.
fn read_json_source(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    if path == std::path::Path::new("-") {
        let mut source = String::new();
        std::io::stdin().read_to_string(&mut source)?;
        Ok(source)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}

#[allow(dead_code)] // Retained for the JSON MCP command path.
fn confirm_if_needed(
    required: bool,
    yes: bool,
    prompt: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !required || yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err("confirmation is required; pass --yes in non-interactive use".into());
    }
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[allow(dead_code)] // Retained for the JSON MCP command path.
fn print_mcp_servers(
    servers: &[cagent_agent::mcp::McpEffectiveServer],
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(servers)?);
    } else {
        for server in servers {
            println!(
                "{}\t{:?}\t{}\t{:?}",
                server.name,
                server.location.scope,
                if server.definition.enabled && server.allowed_for_agent {
                    "enabled"
                } else {
                    "disabled"
                },
                server.status
            );
        }
    }
    Ok(())
}

fn workspace_trust_preflight(
    permissions_path: &std::path::Path,
    workspace: &std::path::Path,
    temporary_workspace_trust: bool,
    terminal: &mut ratatui::DefaultTerminal,
    startup_started_at: Instant,
) -> Result<bool, Box<dyn std::error::Error>> {
    let _preflight_span =
        tracing::trace_span!("frontend.startup.phase", phase = "trust_preflight").entered();
    let (permissions, persisted_workspace_trust) = {
        let _span = tracing::trace_span!("frontend.startup.phase", phase = "trust_check").entered();
        let permissions = cagent_agent::permissions::PermissionFile::new(
            permissions_path.to_path_buf(),
            workspace,
        )?;
        let persisted_workspace_trust = permissions.is_trusted()?;
        (permissions, persisted_workspace_trust)
    };
    if !workspace_trust_prompt_required(temporary_workspace_trust, persisted_workspace_trust) {
        tracing::trace!(
            phase = "trust_preflight_skipped",
            persisted = persisted_workspace_trust,
            temporary = temporary_workspace_trust,
            elapsed = ?startup_started_at.elapsed(),
            "startup milestone reached"
        );
        return Ok(true);
    }

    let project_config_exists = workspace.join(".cagent/config.toml").exists();
    let choice = {
        let _span =
            tracing::trace_span!("frontend.startup.phase", phase = "trust_prompt").entered();
        trust::run_workspace_trust_prompt(terminal, workspace, project_config_exists)?
    };
    tracing::trace!(
        phase = "trust_decision",
        choice = ?choice,
        elapsed = ?startup_started_at.elapsed(),
        "startup milestone reached"
    );
    match choice {
        trust::WorkspaceTrustChoice::Trust => {
            let _span =
                tracing::trace_span!("frontend.startup.phase", phase = "trust_persist").entered();
            permissions.set_trusted(true)?;
            tracing::trace!(
                phase = "trust_persisted",
                elapsed = ?startup_started_at.elapsed(),
                "startup milestone reached"
            );
            Ok(true)
        }
        trust::WorkspaceTrustChoice::ContinueUntrusted => {
            tracing::trace!(
                phase = "trust_continue_untrusted",
                elapsed = ?startup_started_at.elapsed(),
                "startup milestone reached"
            );
            Ok(true)
        }
        trust::WorkspaceTrustChoice::Exit => {
            tracing::trace!(
                phase = "trust_exit",
                elapsed = ?startup_started_at.elapsed(),
                "startup milestone reached"
            );
            Ok(false)
        }
    }
}

fn workspace_trust_prompt_required(
    temporary_workspace_trust: bool,
    persisted_workspace_trust: bool,
) -> bool {
    !temporary_workspace_trust && !persisted_workspace_trust
}

fn print_session_summary(
    stats: &cagent_agent::protocol::SessionStats,
    launch_workspace: &Path,
    session_workspace: &Path,
) {
    print!(
        "{} {}",
        ">_".cyan().bold(),
        format_session_heading(stats).bold()
    );
    if let Some(workspace) = format_session_workspace(launch_workspace, session_workspace) {
        print!(" {workspace}");
    }
    println!();
    println!("{}", format_session_details(stats));
}

fn should_print_session_exit_message(stats: &cagent_agent::protocol::SessionStats) -> bool {
    stats.message_count > 0
}

fn format_session_heading(stats: &cagent_agent::protocol::SessionStats) -> String {
    let title = stats
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Cagent");
    format!("Cagent session: {title}")
}

fn format_session_workspace(launch_workspace: &Path, session_workspace: &Path) -> Option<String> {
    if launch_workspace == session_workspace {
        return None;
    }
    Some(
        relative_path(launch_workspace, session_workspace)
            .unwrap_or_else(|| session_workspace.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }

    let mut relative = PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn format_session_details(stats: &cagent_agent::protocol::SessionStats) -> String {
    let plural = if stats.message_count == 1 { "" } else { "s" };
    let mut details = format!(
        "{} message{plural}, {}",
        stats.message_count,
        format_session_tokens(&stats.usage)
    );
    if let Some(cost) = stats.total_cost.as_deref() {
        let estimated = !matches!(
            stats.cost_source,
            Some(cagent_agent::protocol::CostSource::ProviderReported)
        );
        let amount = cagent_agent::presentation::format_currency(
            cost,
            cagent_agent::presentation::CurrencyFormat::Short,
        );
        let cost = match stats.currency.as_deref() {
            Some("USD") => format!("{}${amount}", if estimated { "~" } else { "" }),
            Some(currency) => format!("{}{amount} {currency}", if estimated { "~" } else { "" }),
            None => amount,
        };
        let _ = write!(details, ", {cost}");
    }
    details
}

fn format_session_tokens(usage: &cagent_agent::protocol::SessionUsage) -> String {
    let input = usage
        .non_cached_input_tokens
        .or_else(|| {
            usage
                .input_tokens
                .zip(usage.cache_read_input_tokens)
                .map(|(input, cached)| input.saturating_sub(cached))
        })
        .or(usage.input_tokens)
        .map(cagent_agent::presentation::format_compact_tokens)
        .unwrap_or_else(|| "—".into());
    let cached = usage
        .cache_read_input_tokens
        .map(cagent_agent::presentation::format_compact_tokens)
        .unwrap_or_else(|| "—".into());
    let cache_write = usage
        .cache_write_input_tokens
        .map(cagent_agent::presentation::format_compact_tokens)
        .map_or_else(String::new, |tokens| format!(" cache-write:{tokens}"));
    let output = usage
        .output_tokens
        .map(cagent_agent::presentation::format_compact_tokens)
        .unwrap_or_else(|| "—".into());
    format!("in:{input} cache-read:{cached}{cache_write} out:{output}")
}
