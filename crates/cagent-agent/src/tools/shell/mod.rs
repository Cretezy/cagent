//! Bash parsing, execution, and supervised background terminals.
//!
//! Parsing is deliberately an authorization aid. It does not sandbox a child
//! process and callers must make that limitation visible when asking users.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

mod analysis;
mod safe;
mod terminal;

pub use analysis::*;
pub use safe::*;
pub use terminal::*;

pub const DEFAULT_TERMINAL_BUFFER_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_MODEL_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_NESTED_DEPTH: u8 = 4;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BashRequest {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub forward_env: Vec<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    pub wait: bool,
}

/// The bounded, model-facing representation of shell output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellOutputExcerpt {
    pub output: String,
    pub truncated: bool,
    pub discarded_bytes: usize,
}

/// Creates a UTF-8-safe head/tail excerpt for model-facing shell results.
#[must_use]
pub fn shell_output_excerpt(output: &str, limit: usize) -> ShellOutputExcerpt {
    debug_assert!(limit > 0);
    if output.len() <= limit {
        return ShellOutputExcerpt {
            output: output.to_owned(),
            truncated: false,
            discarded_bytes: 0,
        };
    }

    let mut head_end = limit / 2;
    while head_end > 0 && !output.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = output.len() - limit / 2;
    while tail_start < output.len() && !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let omitted = output.len() - head_end - (output.len() - tail_start);
    ShellOutputExcerpt {
        output: format!(
            "{}\n… {omitted} bytes omitted …\n{}",
            &output[..head_end],
            &output[tail_start..]
        ),
        truncated: true,
        discarded_bytes: omitted,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BashBackendKind {
    Native,
    GitBash,
    Wsl,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BashBackendInfo {
    pub kind: BashBackendKind,
    pub executable: PathBuf,
    pub host_cwd: PathBuf,
    pub shell_cwd: String,
}

#[derive(Clone, Debug)]
struct BashBackend {
    kind: BashBackendKind,
    executable: PathBuf,
}

#[derive(Debug)]
struct ShellInventoryState {
    started: AtomicBool,
    ready: OnceLock<ShellCommandInventory>,
    readiness: tokio::sync::watch::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct ShellExecutor {
    backend: BashBackend,
    workspace: PathBuf,
    fallback_inventory: ShellCommandInventory,
    inventory: Arc<ShellInventoryState>,
    credential_variables: Vec<String>,
    protected_credential_variables: Vec<String>,
    model_output_bytes: usize,
    terminal_buffer_bytes: usize,
    default_timeout_seconds: u64,
    terminal_mode: crate::config::TerminalMode,
}

impl ShellExecutor {
    /// Creates an executor rooted at the canonical workspace.
    ///
    /// # Errors
    /// Returns an error when the workspace cannot be canonicalized or Bash is unavailable.
    pub fn new(workspace: &Path, credential_variables: Vec<String>) -> Result<Self, ShellError> {
        let workspace = workspace.canonicalize().map_err(ShellError::Io)?;
        let backend = discover_bash()?;
        let fallback_inventory = ShellCommandInventory::empty(backend_info(&backend, &workspace));
        let (readiness, _) = tokio::sync::watch::channel(false);
        Ok(Self {
            backend,
            workspace,
            fallback_inventory,
            inventory: Arc::new(ShellInventoryState {
                started: AtomicBool::new(false),
                ready: OnceLock::new(),
                readiness,
            }),
            credential_variables,
            protected_credential_variables: Vec::new(),
            model_output_bytes: DEFAULT_MODEL_OUTPUT_BYTES,
            terminal_buffer_bytes: DEFAULT_TERMINAL_BUFFER_BYTES,
            default_timeout_seconds: 600,
            terminal_mode: crate::config::TerminalMode::Normal,
        })
    }

    #[must_use]
    pub fn with_output_limits(
        mut self,
        model_output_bytes: usize,
        terminal_buffer_bytes: usize,
    ) -> Self {
        assert!(model_output_bytes > 0 && model_output_bytes <= terminal_buffer_bytes);
        self.model_output_bytes = model_output_bytes;
        self.terminal_buffer_bytes = terminal_buffer_bytes;
        self
    }

    #[must_use]
    pub fn model_output_bytes(&self) -> usize {
        self.model_output_bytes
    }

    #[must_use]
    pub fn terminal_buffer_bytes(&self) -> usize {
        self.terminal_buffer_bytes
    }

    #[must_use]
    pub fn with_default_timeout(mut self, seconds: u64) -> Self {
        assert!((1..=600).contains(&seconds));
        self.default_timeout_seconds = seconds;
        self
    }

    #[must_use]
    pub fn with_terminal_mode(mut self, terminal_mode: crate::config::TerminalMode) -> Self {
        self.terminal_mode = terminal_mode;
        self
    }

    #[must_use]
    pub fn terminal_mode(&self) -> crate::config::TerminalMode {
        self.terminal_mode
    }

    #[must_use]
    pub fn default_timeout_seconds(&self) -> u64 {
        self.default_timeout_seconds
    }

    #[must_use]
    pub fn with_protected_credential_variables(mut self, variables: Vec<String>) -> Self {
        self.protected_credential_variables = variables;
        self
    }

    #[must_use]
    pub fn bash(&self) -> &Path {
        &self.backend.executable
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    #[must_use]
    pub fn inventory(&self) -> &ShellCommandInventory {
        self.inventory
            .ready
            .get()
            .unwrap_or(&self.fallback_inventory)
    }

    #[must_use]
    pub fn inventory_ready(&self) -> bool {
        self.inventory.ready.get().is_some()
    }

    /// Waits until the asynchronous command-inventory probe has completed.
    ///
    /// Completion is distinct from success: callers must continue checking
    /// `inventory().probe_succeeded` before granting automatic authorization.
    ///
    /// # Errors
    /// Returns [`ShellError::Cancelled`] when the enclosing turn is cancelled.
    pub async fn wait_for_inventory(
        &self,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<(), ShellError> {
        self.start_inventory_probe();
        let mut readiness = self.inventory.readiness.subscribe();
        loop {
            if *readiness.borrow_and_update() || self.inventory_ready() {
                return Ok(());
            }
            tokio::select! {
                changed = readiness.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                () = cancellation.cancelled() => return Err(ShellError::Cancelled),
            }
        }
    }

    /// Starts the backend-specific clean-Bash inventory probe once.
    ///
    /// Until it completes successfully, callers observe the empty fallback
    /// inventory and therefore cannot automatically authorize Bash commands.
    pub fn start_inventory_probe(&self) {
        self.start_inventory_probe_for_session(None);
    }

    pub(crate) fn start_inventory_probe_for_session(
        &self,
        session_id: Option<crate::ConversationId>,
    ) {
        if self
            .inventory
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let backend = self.backend.clone();
        let workspace = self.workspace.clone();
        let inventory = self.inventory.clone();
        let span = tracing::info_span!(
            parent: None,
            "agent.shell.inventory",
            session_id = tracing::field::Empty,
            backend = ?backend.kind,
            success = tracing::field::Empty,
            ready = tracing::field::Empty,
        );
        if let Some(session_id) = session_id {
            span.record("session_id", tracing::field::display(session_id));
        }
        tracing::trace!(parent: &span, "shell inventory probe started");
        tokio::task::spawn_blocking(move || {
            let discovered = discover_safe_command_inventory(&backend, &workspace);
            let success = discovered.probe_succeeded;
            let _ = inventory.ready.set(discovered);
            inventory.readiness.send_replace(true);
            span.record("success", success);
            span.record("ready", true);
            tracing::trace!(parent: &span, success, ready = true, "shell inventory probe completed");
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("invalid Bash request: {0}")]
    Invalid(String),
    #[error("Bash wait cancelled; supervised terminal termination was requested")]
    Cancelled,
    #[error("Bash executable is unavailable: {0}")]
    BashUnavailable(String),
    #[error("terminal {0} does not exist")]
    UnknownTerminal(TerminalId),
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("shell I/O error: {0}")]
    Io(#[source] std::io::Error),
}

fn validate_bash_request(request: &BashRequest) -> Result<(), ShellError> {
    if request.command.trim().is_empty() {
        return Err(ShellError::Invalid("command cannot be empty".into()));
    }
    if request
        .timeout
        .is_some_and(|seconds| seconds == 0 || seconds > 600)
    {
        return Err(ShellError::Invalid(
            "timeout must be between 1 and 600 seconds".into(),
        ));
    }
    Ok(())
}

fn configure_backend_pty_command(
    command: &mut CommandBuilder,
    backend: &BashBackend,
    cwd: &Path,
    source: &str,
) {
    match backend.kind {
        BashBackendKind::Wsl => {
            command.arg("--exec");
            command.arg("bash");
            command.arg("-lc");
            command.arg(wsl_source(cwd, source));
        }
        BashBackendKind::Native | BashBackendKind::GitBash => {
            command.arg("-lc");
            command.arg(source);
            command.cwd(cwd);
        }
    }
}

fn wsl_source(cwd: &Path, source: &str) -> String {
    format!(
        "cd -- {} && {source}",
        shell_quote(&shell_path(BashBackendKind::Wsl, cwd))
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_path(kind: BashBackendKind, path: &Path) -> String {
    let host = path.to_string_lossy().replace('\\', "/");
    if kind != BashBackendKind::Wsl {
        return host;
    }
    let bytes = host.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' && bytes[0].is_ascii_alphabetic() {
        format!(
            "/mnt/{}/{}",
            char::from(bytes[0]).to_ascii_lowercase(),
            &host[3..]
        )
    } else {
        host
    }
}

fn discover_bash() -> Result<BashBackend, ShellError> {
    if let Some(path) = std::env::var_os("CAGENT_BASH").filter(|path| !path.is_empty()) {
        let executable = PathBuf::from(path);
        let file_name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let kind =
            if file_name.eq_ignore_ascii_case("wsl.exe") || file_name.eq_ignore_ascii_case("wsl") {
                BashBackendKind::Wsl
            } else if cfg!(windows) {
                BashBackendKind::GitBash
            } else {
                BashBackendKind::Native
            };
        return Ok(BashBackend { kind, executable });
    }
    #[cfg(windows)]
    {
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
        ] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return Ok(BashBackend {
                    kind: BashBackendKind::GitBash,
                    executable: path,
                });
            }
        }
        let wsl = PathBuf::from(r"C:\Windows\System32\wsl.exe");
        if wsl.is_file() {
            return Ok(BashBackend {
                kind: BashBackendKind::Wsl,
                executable: wsl,
            });
        }
        return Err(ShellError::BashUnavailable(
            "install Git Bash or WSL, or set CAGENT_BASH".into(),
        ));
    }
    #[cfg(not(windows))]
    {
        for candidate in ["/bin/bash", "/usr/bin/bash"] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return Ok(BashBackend {
                    kind: BashBackendKind::Native,
                    executable: path,
                });
            }
        }
        Err(ShellError::BashUnavailable(
            "install Bash or set CAGENT_BASH".into(),
        ))
    }
}

fn discover_safe_command_inventory(
    backend: &BashBackend,
    workspace: &Path,
) -> ShellCommandInventory {
    let mut inventory = ShellCommandInventory::empty(backend_info(backend, workspace));
    let names = safe_command_registry()
        .iter()
        .flat_map(|spec| std::iter::once(spec.canonical).chain(spec.aliases.iter().copied()))
        .collect::<Vec<_>>();
    let quoted_names = names
        .iter()
        .map(|name| shell_quote(name))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "for c in {quoted_names}; do k=$(type -t -- \"$c\" 2>/dev/null || true); p=''; if [ \"$k\" = file ]; then p=$(type -P -- \"$c\" 2>/dev/null || true); fi; printf '%s\\t%s\\t%s\\n' \"$c\" \"$k\" \"$p\"; done"
    );
    let mut command = std::process::Command::new(&backend.executable);
    match backend.kind {
        BashBackendKind::Wsl => {
            command.args(["--exec", "bash", "-lc", &script]);
        }
        BashBackendKind::Native | BashBackendKind::GitBash => {
            command.args(["-lc", &script]).current_dir(workspace);
        }
    }
    let Ok(output) = command.output() else {
        return inventory;
    };
    if !output.status.success() {
        return inventory;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    apply_inventory_probe_output(&mut inventory, &stdout, backend.kind);
    inventory.probe_succeeded = true;
    inventory
}

fn backend_info(backend: &BashBackend, workspace: &Path) -> BashBackendInfo {
    BashBackendInfo {
        kind: backend.kind,
        executable: backend.executable.clone(),
        host_cwd: workspace.to_path_buf(),
        shell_cwd: shell_path(backend.kind, workspace),
    }
}

fn apply_inventory_probe_output(
    inventory: &mut ShellCommandInventory,
    stdout: &str,
    backend_kind: BashBackendKind,
) {
    for line in stdout.lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(name), Some(kind), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(spec) = safe_command_registry()
            .iter()
            .find(|spec| spec.canonical == name || spec.aliases.contains(&name))
        else {
            continue;
        };
        let resolution_kind = match kind {
            "builtin" if spec.builtin => SafeCommandResolutionKind::BashBuiltin,
            "file" if !spec.builtin && !path.is_empty() => {
                SafeCommandResolutionKind::ExternalExecutable
            }
            _ => continue,
        };
        let entry = SafeCommandAvailability {
            canonical_command: spec.canonical.into(),
            invocation_name: name.into(),
            resolution_kind,
            resolved_executable: (resolution_kind == SafeCommandResolutionKind::ExternalExecutable)
                .then(|| path.to_owned()),
            backend: backend_kind,
            hardening_available: spec.hardening != SafeCommandHardening::Git
                || resolution_kind == SafeCommandResolutionKind::ExternalExecutable,
        };
        let replace = !inventory.commands.contains_key(spec.canonical)
            || (spec.canonical == "fd" && name == "fd");
        if replace {
            inventory.commands.insert(spec.canonical.into(), entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn shell_inventory_starts_unavailable_and_becomes_backend_specific() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        assert!(!executor.inventory_ready());
        assert!(!executor.inventory().probe_succeeded);
        assert!(executor.inventory().commands.is_empty());

        executor.start_inventory_probe();
        executor.start_inventory_probe();
        tokio::time::timeout(
            Duration::from_secs(3),
            executor.wait_for_inventory(&CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(executor.inventory().probe_succeeded);
        assert!(executor.inventory().available("sed").is_some());
        std::fs::write(temporary.path().join("TEST.md"), "line\n").unwrap();
        let request = BashRequest {
            command: "sed -n '1p' TEST.md; rustfmt --check TEST.md".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        let analysis = analyze_shell(&request.command).unwrap();
        let authorization = authorize_available_safe_bash_segments(
            &request,
            executor.inventory(),
            &analysis,
            3,
            false,
        )
        .unwrap();
        assert!(authorization.segment(0).is_some());
        assert!(authorization.tier(1).is_some());
        assert_eq!(executor.inventory().backend.kind, executor.backend.kind);
        assert_eq!(executor.inventory().backend.host_cwd, executor.workspace);
    }

    #[tokio::test]
    async fn shell_inventory_wait_is_notified_after_delayed_completion() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        executor.inventory.started.store(true, Ordering::Release);
        let inventory = executor.inventory.clone();
        let completion = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let _ = inventory
                .ready
                .set(ShellCommandInventory::synthetic(&["cargo"]));
            inventory.readiness.send_replace(true);
        });

        executor
            .wait_for_inventory(&CancellationToken::new())
            .await
            .unwrap();
        completion.await.unwrap();
        assert!(executor.inventory().probe_succeeded);
    }

    #[tokio::test]
    async fn shell_inventory_wait_releases_on_failed_probe_and_on_cancellation() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        executor.inventory.started.store(true, Ordering::Release);
        let inventory = executor.inventory.clone();
        let failed = executor.fallback_inventory.clone();
        tokio::spawn(async move {
            let _ = inventory.ready.set(failed);
            inventory.readiness.send_replace(true);
        });

        executor
            .wait_for_inventory(&CancellationToken::new())
            .await
            .unwrap();
        assert!(executor.inventory_ready());
        assert!(!executor.inventory().probe_succeeded);

        let cancelled_executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        cancelled_executor
            .inventory
            .started
            .store(true, Ordering::Release);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            cancelled_executor.wait_for_inventory(&cancellation).await,
            Err(ShellError::Cancelled)
        ));
    }

    #[test]
    fn parses_compound_wrapped_substitution_and_redirection() {
        let parsed = analyze_shell(
            "A=1 bash -lc 'cargo test && git push' | tee $(printf out).txt > ../result.txt",
        )
        .unwrap();
        let commands = parsed
            .segments
            .iter()
            .map(|segment| segment.words.join(" "))
            .collect::<Vec<_>>();
        assert!(
            commands
                .iter()
                .any(|command| command.starts_with("bash -lc"))
        );
        assert!(commands.contains(&"cargo test".into()));
        assert!(commands.contains(&"git push".into()));
        assert!(commands.contains(&"printf out".into()));
        assert!(parsed.operators.contains(&ShellOperator::And));
        assert!(parsed.operators.contains(&ShellOperator::Pipeline));
        assert!(parsed.operators.contains(&ShellOperator::Substitution));
        assert!(
            parsed
                .paths
                .iter()
                .any(|path| path.value == "../result.txt" && path.access == ShellPathAccess::Write)
        );
    }

    #[test]
    fn assignment_shaped_arguments_after_command_name_remain_arguments() {
        let command = "rustfmt --edition 2024 --config skip_children=true --check src/lib.rs";
        let analysis = analyze_shell(command).unwrap();

        assert!(!analysis.has_assignment);
        assert_eq!(
            analysis.segments[0].words,
            [
                "rustfmt",
                "--edition",
                "2024",
                "--config",
                "skip_children=true",
                "--check",
                "src/lib.rs",
            ]
        );

        let assignment = analyze_shell("RUST_LOG=debug rustfmt --check src/lib.rs").unwrap();
        assert!(assignment.has_assignment);
    }

    #[test]
    fn truncated_permission_suggestions_match_the_command_arguments() {
        let analysis = analyze_shell("cargo test -p cagent-agent").unwrap();

        assert_eq!(
            analysis.segments[0].suggested_command.as_deref(),
            Some(["cargo".into(), "test".into(), "*".into()].as_slice())
        );
        assert_eq!(
            analyze_shell("cargo test").unwrap().segments[0]
                .suggested_command
                .as_deref(),
            Some(["cargo".into(), "test".into()].as_slice())
        );
    }

    #[test]
    fn null_device_redirections_do_not_require_path_permission() {
        for source in [
            "printf ok > /dev/null",
            "printf ok >> /dev/null",
            "printf ok 2> /dev/null",
            "printf ok &> /dev/null",
        ] {
            let analysis =
                analyze_shell(source).unwrap_or_else(|error| panic!("{source}: {error}"));
            assert!(analysis.paths.is_empty(), "{source}: {:#?}", analysis.paths);
        }
    }

    #[test]
    fn stderr_descriptor_duplication_is_read_safe_and_has_no_path() {
        for source in ["cat TEST.md 2>&1", "cat TEST.md 2>&2"] {
            let analysis = analyze_shell(source).unwrap();
            assert!(analysis.only_read_safe_redirections, "{source}");
            assert_eq!(
                analysis
                    .paths
                    .iter()
                    .map(|path| path.value.as_str())
                    .collect::<Vec<_>>(),
                ["TEST.md"],
                "{source}"
            );
        }
    }

    #[test]
    fn other_device_and_external_redirections_remain_permission_checked() {
        for target in ["/dev/random", "/tmp/cagent-output"] {
            let source = format!("printf ok > {target}");
            let analysis =
                analyze_shell(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(
                analysis.paths,
                vec![ShellPath {
                    value: target.into(),
                    access: ShellPathAccess::Write,
                    dynamic: false,
                    source: "redirection".into(),
                    segment_index: None,
                }],
            );
        }
    }

    #[test]
    fn dynamic_nested_source_is_opaque_and_has_no_interpreter_suggestion() {
        let parsed = analyze_shell("bash -lc \"$SCRIPT\"").unwrap();
        assert!(parsed.opaque);
        let bash = parsed
            .segments
            .iter()
            .find(|segment| segment.words.first().is_some_and(|word| word == "bash"))
            .unwrap();
        assert!(bash.opaque);
        assert!(bash.suggested_command.is_none());
    }

    #[test]
    fn parser_fixtures_cover_wrappers_subshells_and_spec_examples() {
        let fixtures = [
            ("cargo test && git push", vec!["cargo test", "git push"]),
            (
                "find . -name '*.tmp' | xargs rm",
                vec!["find . -name *.tmp", "xargs rm"],
            ),
            (
                "eval 'printf ok; printf done'",
                vec!["eval printf ok; printf done", "printf ok", "printf done"],
            ),
            (
                "env A=1 nohup printf ok",
                vec!["env A=1 nohup printf ok", "nohup printf ok", "printf ok"],
            ),
            (
                "(printf sub); printf $(printf nested)",
                vec!["printf sub", "printf nested"],
            ),
        ];
        for (source, expected) in fixtures {
            let analysis =
                analyze_shell(source).unwrap_or_else(|error| panic!("{source}: {error}"));
            let actual = analysis
                .segments
                .iter()
                .map(|segment| segment.words.join(" "))
                .collect::<Vec<_>>();
            for command in expected {
                assert!(
                    actual.iter().any(|candidate| candidate == command),
                    "{source}: missing {command:?} in {actual:?}"
                );
            }
        }
    }

    #[test]
    fn broad_destructive_commands_cannot_be_suggested() {
        let parsed = analyze_shell("rm -rf /").unwrap();
        assert!(parsed.segments[0].broad_destructive);
        assert!(parsed.segments[0].suggested_command.is_none());
    }

    #[test]
    fn classifier_output_is_strict_and_bounded() {
        let output = parse_classifier_output(
            r#"{"decision":"allow","reason":"read-only test","risk":"low","user_authorization":"low"}"#,
        )
        .unwrap();
        assert_eq!(output.decision, AutoClassifierDecision::Allow);
        assert!(
            parse_classifier_output(
                r#"{"decision":"allow","reason":"ok","risk":"low","user_authorization":"low","extra":true}"#
            )
            .is_err()
        );
        assert!(parse_classifier_output("not json").is_err());
        assert!(
            parse_classifier_output(&format!(
                r#"{{"decision":"ask","reason":"{}","risk":"high","user_authorization":"unknown"}}"#,
                "x".repeat(241)
            ))
            .is_err()
        );
    }

    #[test]
    fn classifier_stage_one_accepts_only_needs_review() {
        assert!(!parse_classifier_stage_one(r#"{"needs_review":false}"#).unwrap());
        assert!(parse_classifier_stage_one(r#"{"needs_review":true}"#).unwrap());
        assert!(parse_classifier_stage_one(r#"{"block":false}"#).is_err());
        assert!(parse_classifier_stage_one(r#"{"needs_review":false,"reason":"no"}"#).is_err());
    }

    #[test]
    fn classifier_high_risk_allow_requires_authorization_and_clear_guards() {
        let mut output = AutoClassifierOutput {
            decision: AutoClassifierDecision::Allow,
            reason: "explicitly requested production migration for the established target".into(),
            risk: AutoClassifierRisk::High,
            user_authorization: AutoReviewAuthorization::High,
        };
        for (authorization, expected) in [
            (
                AutoReviewAuthorization::Unknown,
                crate::PermissionEffect::Ask,
            ),
            (AutoReviewAuthorization::Low, crate::PermissionEffect::Ask),
            (
                AutoReviewAuthorization::Medium,
                crate::PermissionEffect::Allow,
            ),
            (
                AutoReviewAuthorization::High,
                crate::PermissionEffect::Allow,
            ),
        ] {
            output.user_authorization = authorization;
            assert_eq!(
                constrained_classifier_effect(Ok(&output), AutoClassifierGuard::Clear),
                expected,
            );
            assert_eq!(
                constrained_classifier_effect(Ok(&output), AutoClassifierGuard::RequiresApproval),
                crate::PermissionEffect::Ask,
            );
            assert_eq!(
                constrained_classifier_effect(Ok(&output), AutoClassifierGuard::ExplicitDeny),
                crate::PermissionEffect::Deny,
            );
        }
    }

    #[test]
    fn classifier_failures_and_hard_limits_fall_back_safely() {
        let allow = AutoClassifierOutput {
            decision: AutoClassifierDecision::Allow,
            reason: "safe".into(),
            risk: AutoClassifierRisk::Low,
            user_authorization: AutoReviewAuthorization::Low,
        };
        let deny = AutoClassifierOutput {
            decision: AutoClassifierDecision::Deny,
            reason: "critical".into(),
            risk: AutoClassifierRisk::Critical,
            user_authorization: AutoReviewAuthorization::Unknown,
        };
        let critical_allow = AutoClassifierOutput {
            decision: AutoClassifierDecision::Allow,
            reason: "incorrect model allow".into(),
            risk: AutoClassifierRisk::Critical,
            user_authorization: AutoReviewAuthorization::High,
        };
        assert_eq!(
            constrained_classifier_effect(Err("timeout"), AutoClassifierGuard::Clear),
            crate::PermissionEffect::Ask
        );
        assert_eq!(
            constrained_classifier_effect(Ok(&allow), AutoClassifierGuard::RequiresApproval,),
            crate::PermissionEffect::Ask
        );
        assert_eq!(
            constrained_classifier_effect(Ok(&allow), AutoClassifierGuard::RequiresApproval,),
            crate::PermissionEffect::Ask
        );
        assert_eq!(
            constrained_classifier_effect(Ok(&allow), AutoClassifierGuard::RequiresApproval,),
            crate::PermissionEffect::Ask
        );
        assert_eq!(
            constrained_classifier_effect(Ok(&allow), AutoClassifierGuard::ExplicitDeny,),
            crate::PermissionEffect::Deny
        );
        assert_eq!(
            constrained_classifier_effect(Ok(&allow), AutoClassifierGuard::Clear),
            crate::PermissionEffect::Allow
        );
        assert_eq!(
            constrained_classifier_effect(Ok(&deny), AutoClassifierGuard::Clear),
            crate::PermissionEffect::Ask
        );
        assert_eq!(
            constrained_classifier_effect(Ok(&critical_allow), AutoClassifierGuard::Clear),
            crate::PermissionEffect::Ask
        );
    }

    #[test]
    fn auto_review_actions_are_tagged_and_legacy_records_still_load() {
        let action = AutoReviewAction::WebSearch {
            provider: "exa".into(),
            query: "rust serde".into(),
        };
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            serde_json::json!({"type":"web_search","provider":"exa","query":"rust serde"})
        );
        let actions = [
            AutoReviewAction::Bash {
                command: "pwd".into(),
                cwd: "/workspace".into(),
                segments: Vec::new(),
                operators: Vec::new(),
                paths: Vec::new(),
            },
            AutoReviewAction::WebFetch {
                url: "https://example.test/next".into(),
                format: crate::WebFetchFormat::Text,
                redirect_chain: vec!["http://example.test/".into()],
            },
            AutoReviewAction::Mcp {
                server: "docs".into(),
                operation: "search".into(),
                arguments: serde_json::json!({"query":"serde"}),
                description: Some("Search docs".into()),
                configured_read_only: true,
            },
            AutoReviewAction::Write {
                tool: "apply_patch".into(),
                diff: crate::SemanticDiff::default(),
                paths: vec!["/workspace/src/lib.rs".into()],
            },
            AutoReviewAction::Read {
                tool: "bash".into(),
                path: "/shared/notes.md".into(),
            },
        ];
        assert_eq!(
            actions
                .iter()
                .map(|action| serde_json::to_value(action).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned())
                .collect::<Vec<_>>(),
            ["bash", "web_fetch", "mcp", "write", "read"]
        );
        let record: AutoClassifierRecord = serde_json::from_value(serde_json::json!({
            "provider": "mock",
            "model": "fast",
            "latency_millis": 12,
            "output": {"decision":"allow","reason":"legacy","risk":"low"},
            "usage": {}
        }))
        .unwrap();
        assert_eq!(record.status, AutoReviewStatus::Completed);
        assert_eq!(record.provider.as_deref(), Some("mock"));
        assert_eq!(
            record.output.user_authorization,
            AutoReviewAuthorization::Unknown
        );
        assert!(record.evidence.is_empty());
    }

    #[tokio::test]
    async fn background_terminal_has_monotonic_cursors_and_accepts_input() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, _events) = TerminalSupervisor::new(&executor);
        let owner = crate::ConversationId::new();
        let tool_call = crate::NodeId::new();
        let started = supervisor
            .start(
                owner,
                tool_call,
                &BashRequest {
                    command: "read value; printf 'got:%s' \"$value\"".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: false,
                },
            )
            .unwrap();
        supervisor
            .write(&TerminalWriteRequest {
                id: started.id,
                data: "hello\n".into(),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let output = supervisor
                .output(&TerminalOutputRequest {
                    id: started.id,
                    cursor: None,
                })
                .unwrap();
            if output.output.contains("got:hello") {
                assert!(output.cursor >= 9);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }

        let sleeping = supervisor
            .start(
                owner,
                tool_call,
                &BashRequest {
                    command: "sleep 30".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: false,
                },
            )
            .unwrap();
        assert_eq!(
            supervisor
                .kill(&TerminalKillRequest {
                    id: sleeping.id,
                    force: true,
                })
                .await
                .unwrap(),
            TerminalStatus::Killed
        );
        assert_eq!(
            supervisor
                .output(&TerminalOutputRequest {
                    id: sleeping.id,
                    cursor: None,
                })
                .unwrap()
                .status,
            TerminalStatus::Killed
        );
    }

    #[tokio::test]
    async fn background_terminal_publishes_start_before_any_output() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, mut events) = TerminalSupervisor::new(&executor);
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command: "sleep 30".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: false,
                },
            )
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        let TerminalSupervisorEvent::Started(snapshot) = event else {
            panic!("silent terminal should publish its running snapshot immediately");
        };
        assert_eq!(snapshot.id, started.id);
        assert_eq!(snapshot.status, TerminalStatus::Running);
        assert!(snapshot.output.is_empty());
        supervisor
            .kill(&TerminalKillRequest {
                id: started.id,
                force: true,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn background_terminal_streams_and_detects_nonzero_exit_without_polling() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, mut events) = TerminalSupervisor::new(&executor);
        let tool_call_node_id = crate::NodeId::new();
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                tool_call_node_id,
                &BashRequest {
                    command: "printf streamed; exit 7".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: false,
                },
            )
            .unwrap();

        let completed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let TerminalSupervisorEvent::Completed(id) = events.recv().await.unwrap() {
                    break supervisor.snapshot(id).unwrap();
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(completed.id, started.id);
        assert_eq!(completed.tool_call_node_id, Some(tool_call_node_id));
        assert_eq!(completed.status, TerminalStatus::Exited);
        assert_eq!(completed.exit_code, Some(7));
        assert!(completed.output.contains("streamed"));
        assert!(completed.completed_at.is_some());
    }

    #[tokio::test]
    async fn completed_terminal_processes_can_be_evicted() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, _events) = TerminalSupervisor::new(&executor);
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command: "printf done".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: Some(5),
                    wait: true,
                },
            )
            .unwrap();

        let completed = supervisor
            .wait(started.id, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.output, "done");
        assert_eq!(supervisor.counts(), (0, 1));

        supervisor.remove_completed(started.id);

        assert_eq!(supervisor.counts(), (0, 0));
        assert!(matches!(
            supervisor.snapshot(started.id),
            Err(ShellError::UnknownTerminal(id)) if id == started.id
        ));
    }

    #[tokio::test]
    async fn terminal_output_updates_are_coalesced_until_acknowledged() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, mut events) = TerminalSupervisor::new(&executor);
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command: "i=0; while [ $i -lt 20000 ]; do printf 'output-%s\\n' \"$i\"; i=$((i + 1)); done".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: Some(5),
                    wait: false,
                },
            )
            .unwrap();

        let mut updates = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await.unwrap() {
                    TerminalSupervisorEvent::Updated(id) if id == started.id => updates += 1,
                    TerminalSupervisorEvent::Completed(id) if id == started.id => break,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(updates, 1);
    }

    #[tokio::test]
    async fn acknowledging_terminal_output_allows_another_update() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, mut events) = TerminalSupervisor::new(&executor);
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command: "printf first; sleep 0.2; printf second".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: Some(5),
                    wait: false,
                },
            )
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(events.recv().await, Some(TerminalSupervisorEvent::Updated(id)) if id == started.id) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        supervisor.acknowledge_update(started.id).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(events.recv().await, Some(TerminalSupervisorEvent::Updated(id)) if id == started.id) {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn windows_backend_paths_keep_host_and_shell_forms_distinct() {
        let host = Path::new(r"C:\Users\Ada Lovelace\project");
        assert_eq!(
            shell_path(BashBackendKind::GitBash, host),
            "C:/Users/Ada Lovelace/project"
        );
        assert_eq!(
            shell_path(BashBackendKind::Wsl, host),
            "/mnt/c/Users/Ada Lovelace/project"
        );
        assert_eq!(
            wsl_source(host, "printf ok"),
            "cd -- '/mnt/c/Users/Ada Lovelace/project' && printf ok"
        );
    }

    #[test]
    fn inventory_probe_accepts_only_expected_builtins_and_external_files() {
        for kind in [
            BashBackendKind::Native,
            BashBackendKind::GitBash,
            BashBackendKind::Wsl,
        ] {
            let backend = BashBackendInfo {
                kind,
                executable: PathBuf::from("bash"),
                host_cwd: PathBuf::from("."),
                shell_cwd: ".".into(),
            };
            let mut inventory = ShellCommandInventory::empty(backend);
            apply_inventory_probe_output(
                &mut inventory,
                "cd\tbuiltin\t\ncat\tfile\t/usr/bin/cat\nrg\talias\t\nfind\tfunction\t\nfdfind\tfile\t/usr/bin/fdfind\n",
                kind,
            );
            assert!(inventory.available("cd").is_some());
            assert!(inventory.available("cat").is_some());
            assert!(inventory.available("rg").is_none());
            assert!(inventory.available("find").is_none());
            assert_eq!(inventory.available("fd").unwrap().invocation_name, "fdfind");
        }
    }

    #[test]
    fn bash_request_rejects_invalid_timeout() {
        let request = BashRequest {
            command: "printf ok".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: Some(601),
            wait: true,
        };
        assert!(matches!(
            validate_bash_request(&request),
            Err(ShellError::Invalid(_))
        ));
    }

    #[test]
    fn bash_request_requires_wait_and_rejects_the_removed_background_flag() {
        assert!(
            serde_json::from_value::<BashRequest>(serde_json::json!({
                "command": "true"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<BashRequest>(serde_json::json!({
                "command": "true",
                "wait": true,
                "background": false
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn supervised_wait_returns_the_same_durable_terminal_completion() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, _events) = TerminalSupervisor::new(&executor);
        let tool_call_node_id = crate::NodeId::new();
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                tool_call_node_id,
                &BashRequest {
                    command: "printf joined".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: Some(5),
                    wait: true,
                },
            )
            .unwrap();
        let completed = supervisor
            .wait(started.id, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.id, started.id);
        assert_eq!(completed.tool_call_node_id, Some(tool_call_node_id));
        assert_eq!(completed.status, TerminalStatus::Exited);
        assert_eq!(completed.output, "joined");
    }

    #[tokio::test]
    async fn supervised_terminal_disables_implicit_pagers_for_waiting_calls() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, _events) = TerminalSupervisor::new(&executor);
        let mut env = BTreeMap::new();
        env.insert("PAGER".into(), "interactive-pager".into());
        env.insert("JJ_PAGER".into(), "interactive-jj-pager".into());
        env.insert("GIT_PAGER".into(), "interactive-git-pager".into());
        env.insert("GH_PAGER".into(), "interactive-gh-pager".into());
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command:
                        "printf '%s|%s|%s|%s' \"$PAGER\" \"$JJ_PAGER\" \"$GIT_PAGER\" \"$GH_PAGER\""
                            .into(),
                    cwd: None,
                    env,
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: true,
                },
            )
            .unwrap();

        let completed = supervisor
            .wait(started.id, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.output, "cat|cat|cat|cat");
    }

    #[tokio::test]
    async fn background_terminal_also_disables_implicit_pagers() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, _events) = TerminalSupervisor::new(&executor);
        let mut env = BTreeMap::new();
        env.insert("PAGER".into(), "interactive-pager".into());
        env.insert("JJ_PAGER".into(), "interactive-jj-pager".into());
        env.insert("GIT_PAGER".into(), "interactive-git-pager".into());
        env.insert("GH_PAGER".into(), "interactive-gh-pager".into());
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command:
                        "printf '%s|%s|%s|%s' \"$PAGER\" \"$JJ_PAGER\" \"$GIT_PAGER\" \"$GH_PAGER\""
                            .into(),
                    cwd: None,
                    env,
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: false,
                },
            )
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let output = supervisor
                .output(&TerminalOutputRequest {
                    id: started.id,
                    cursor: None,
                })
                .unwrap();
            if output.output.contains("cat|cat|cat|cat") {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[tokio::test]
    async fn dumb_terminal_mode_overrides_request_term() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new())
            .unwrap()
            .with_terminal_mode(crate::config::TerminalMode::Dumb);
        let (supervisor, _events) = TerminalSupervisor::new(&executor);
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command: "printf '%s' \"$TERM\"".into(),
                    cwd: None,
                    env: BTreeMap::from([("TERM".into(), "xterm-256color".into())]),
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: true,
                },
            )
            .unwrap();

        let completed = supervisor
            .wait(started.id, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.output, "dumb");
    }

    #[tokio::test]
    async fn cancelling_a_wait_terminates_the_terminal() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, _events) = TerminalSupervisor::new(&executor);
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command: "sleep 30".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: true,
                },
            )
            .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            supervisor.wait(started.id, &cancellation).await,
            Err(ShellError::Cancelled)
        ));
        assert!(matches!(
            supervisor.snapshot(started.id).unwrap().status,
            TerminalStatus::Terminating | TerminalStatus::Killed
        ));
        assert_eq!(
            supervisor
                .kill(&TerminalKillRequest {
                    id: started.id,
                    force: true,
                })
                .await
                .unwrap(),
            TerminalStatus::Killed
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graceful_kill_stays_terminating_until_force_escalation_reaps_process() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = ShellExecutor::new(temporary.path(), Vec::new()).unwrap();
        let (supervisor, mut events) = TerminalSupervisor::new(&executor);
        let supervisor = supervisor.with_termination_grace(Duration::from_millis(75));
        let started = supervisor
            .start(
                crate::ConversationId::new(),
                crate::NodeId::new(),
                &BashRequest {
                    command: "trap '' TERM; printf ready; while :; do sleep 1; done".into(),
                    cwd: None,
                    env: BTreeMap::new(),
                    forward_env: Vec::new(),
                    timeout: None,
                    wait: false,
                },
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if supervisor
                    .snapshot(started.id)
                    .unwrap()
                    .output
                    .contains("ready")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        assert_eq!(
            supervisor
                .request_kill(&TerminalKillRequest {
                    id: started.id,
                    force: false,
                })
                .unwrap(),
            TerminalStatus::Terminating
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(
            supervisor.snapshot(started.id).unwrap().status,
            TerminalStatus::Terminating
        );
        assert!(!matches!(
            events.try_recv(),
            Ok(TerminalSupervisorEvent::Completed(_))
        ));

        let status = supervisor
            .kill(&TerminalKillRequest {
                id: started.id,
                force: false,
            })
            .await
            .unwrap();
        assert_eq!(status, TerminalStatus::Killed);
        let final_snapshot = supervisor.snapshot(started.id).unwrap();
        assert!(final_snapshot.completed_at.is_some());
    }
}
