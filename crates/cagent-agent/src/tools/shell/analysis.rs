#![allow(clippy::derivable_impls)] // The explicit default documents the conservative risk fallback.

use super::*;
use brush_parser::ast;
use brush_parser::word::WordPiece;
use brush_parser::{Parser, ParserOptions, SourceInfo};
use std::io::Cursor;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOperator {
    Pipeline,
    And,
    Or,
    Sequence,
    Background,
    Subshell,
    Substitution,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellPathAccess {
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShellPath {
    pub value: String,
    pub access: ShellPathAccess,
    pub dynamic: bool,
    pub source: String,
    /// Owning command for argument paths. Unattributed paths (including
    /// redirections) must retain their independent permission checks.
    #[serde(default)]
    pub segment_index: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShellSegment {
    pub words: Vec<String>,
    pub source_words: Vec<String>,
    pub raw: String,
    pub depth: u8,
    pub opaque: bool,
    pub broad_destructive: bool,
    pub suggested_command: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShellAnalysis {
    pub source: String,
    pub segments: Vec<ShellSegment>,
    pub operators: Vec<ShellOperator>,
    pub paths: Vec<ShellPath>,
    pub has_redirection: bool,
    pub only_read_safe_redirections: bool,
    pub has_assignment: bool,
    pub opaque: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoClassifierDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoClassifierRisk {
    Unknown,
    Low,
    Medium,
    High,
    Critical,
}

impl Default for AutoClassifierRisk {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewAuthorization {
    #[default]
    Unknown,
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewStatus {
    #[default]
    Completed,
    Failed,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutoClassifierOutput {
    pub decision: AutoClassifierDecision,
    pub reason: String,
    #[serde(default)]
    pub risk: AutoClassifierRisk,
    #[serde(default)]
    pub user_authorization: AutoReviewAuthorization,
}

impl From<&AutoClassifierOutput> for crate::AutoReviewSummary {
    fn from(output: &AutoClassifierOutput) -> Self {
        Self {
            decision: format!("{:?}", output.decision).to_ascii_lowercase(),
            risk: format!("{:?}", output.risk).to_ascii_lowercase(),
            authorization: format!("{:?}", output.user_authorization).to_ascii_lowercase(),
            reason: output.reason.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoReviewEvidenceRecord {
    pub command: String,
    pub cwd: String,
    pub exit_status: Option<i32>,
    pub duration_millis: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoClassifierRecord {
    #[serde(default)]
    pub status: AutoReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub latency_millis: u64,
    pub output: AutoClassifierOutput,
    #[serde(default)]
    pub usage: crate::ModelUsage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<AutoReviewEvidenceRecord>,
}

impl AutoClassifierRecord {
    #[must_use]
    pub fn failure(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let reason = if reason.chars().count() > 240 {
            reason.chars().take(239).collect::<String>() + "…"
        } else {
            reason
        };
        Self {
            status: AutoReviewStatus::Failed,
            provider: None,
            model: None,
            latency_millis: 0,
            output: AutoClassifierOutput {
                decision: AutoClassifierDecision::Ask,
                reason,
                risk: AutoClassifierRisk::Unknown,
                user_authorization: AutoReviewAuthorization::Unknown,
            },
            usage: crate::ModelUsage::default(),
            evidence: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutoReviewAction {
    Bash {
        command: String,
        cwd: String,
        segments: Vec<ShellSegment>,
        operators: Vec<ShellOperator>,
        paths: Vec<ShellPath>,
    },
    Write {
        tool: String,
        diff: crate::SemanticDiff,
        paths: Vec<String>,
    },
    Read {
        tool: String,
        path: String,
    },
    WebSearch {
        provider: String,
        query: String,
    },
    WebFetch {
        url: String,
        format: crate::WebFetchFormat,
        redirect_chain: Vec<String>,
    },
    Mcp {
        server: String,
        operation: String,
        arguments: serde_json::Value,
        description: Option<String>,
        configured_read_only: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewScope {
    WholeAction,
    ExternalReadBoundary,
    ExternalWriteBoundary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewIneligibilityReason {
    PolicyNotAuto,
    AlreadyDecided,
    ExplicitDeny,
    OperationRequiresApproval,
    NoUnresolvedExternalBoundary,
    OpaqueCommand,
    DynamicPath,
    DestructiveOperation,
    ExternalWorkingDirectory,
    WritePolicyNotAuto,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AutoReviewEligibility {
    Eligible {
        scope: AutoReviewScope,
    },
    Ineligible {
        reason: AutoReviewIneligibilityReason,
    },
}

impl AutoReviewEligibility {
    #[must_use]
    pub const fn scope(self) -> Option<AutoReviewScope> {
        match self {
            Self::Eligible { scope } => Some(scope),
            Self::Ineligible { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AutoReviewContext<'a> {
    pub eligibility: AutoReviewEligibility,
    pub operation: &'a crate::PermissionDecision,
    pub external: Option<&'a crate::PermissionDecision>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AutoReviewEnvelope<'a> {
    pub transcript: serde_json::Value,
    pub action: &'a AutoReviewAction,
    pub mode: &'a str,
    pub read_policy: crate::ReadPolicy,
    pub write_policy: crate::WritePolicy,
    pub run_policy: crate::RunPolicy,
    pub auto_level: crate::AutoLevel,
    pub review_scope: AutoReviewScope,
    pub operation_decision: &'a crate::PermissionDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_decision: Option<&'a crate::PermissionDecision>,
    pub rules: AutoReviewRules<'a>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AutoReviewRules<'a> {
    pub project: &'a [crate::PermissionRule],
    pub agent: &'a [crate::PermissionRule],
    pub mode: &'a [crate::PermissionRule],
    pub global: &'a [crate::PermissionRule],
}

/// Parses the classifier's strict JSON response.
///
/// # Errors
/// Returns an error for malformed JSON, unknown fields, or an invalid reason.
pub fn parse_classifier_output(source: &str) -> Result<AutoClassifierOutput, String> {
    let mut source = source.trim().as_bytes().to_vec();
    let output: AutoClassifierOutput = simd_json::serde::from_slice(&mut source)
        .map_err(|error| format!("classifier returned invalid JSON: {error}"))?;
    if output.reason.trim().is_empty() || output.reason.chars().count() > 240 {
        return Err("classifier reason must contain 1 to 240 characters".into());
    }
    Ok(output)
}

/// Parses the fast classifier pass.
///
/// # Errors
/// Returns an error for malformed JSON or an unknown response shape.
pub fn parse_classifier_stage_one(source: &str) -> Result<bool, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StageOne {
        needs_review: bool,
    }

    let mut input = source.trim().as_bytes().to_vec();
    simd_json::serde::from_slice::<StageOne>(&mut input)
        .map(|output| output.needs_review)
        .map_err(|error| format!("classifier returned invalid stage-1 JSON: {error}"))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutoClassifierGuard {
    #[default]
    Clear,
    RequiresApproval,
    ExplicitDeny,
}

#[must_use]
pub fn constrained_classifier_effect(
    output: Result<&AutoClassifierOutput, &str>,
    guard: AutoClassifierGuard,
) -> crate::PermissionEffect {
    if guard == AutoClassifierGuard::ExplicitDeny {
        return crate::PermissionEffect::Deny;
    }
    match output {
        Ok(output)
            if output.decision == AutoClassifierDecision::Allow
                && guard == AutoClassifierGuard::Clear
                && matches!(
                    output.risk,
                    AutoClassifierRisk::Low | AutoClassifierRisk::Medium
                ) =>
        {
            crate::PermissionEffect::Allow
        }
        Ok(output)
            if output.decision == AutoClassifierDecision::Allow
                && guard == AutoClassifierGuard::Clear
                && output.risk == AutoClassifierRisk::High
                && matches!(
                    output.user_authorization,
                    AutoReviewAuthorization::Medium | AutoReviewAuthorization::High
                ) =>
        {
            crate::PermissionEffect::Allow
        }
        Ok(_) | Err(_) => crate::PermissionEffect::Ask,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShellParseError {
    #[error("Bash parse failed: {0}")]
    Parse(String),
}

/// Parses Bash into an AST and extracts independently authorized executable
/// segments, nested literal shell source, substitutions, and static paths.
///
/// # Errors
/// Returns an error when the command is not valid Bash syntax.
pub fn analyze_shell(source: &str) -> Result<ShellAnalysis, ShellParseError> {
    analyze_shell_with_options(source, ShellAnalysisOptions::default())
}

#[derive(Clone, Copy, Default)]
struct ShellAnalysisOptions {
    allow_path_patterns: bool,
}

pub(super) fn analyze_shell_for_presentation(
    source: &str,
) -> Result<ShellAnalysis, ShellParseError> {
    analyze_shell_with_options(
        source,
        ShellAnalysisOptions {
            allow_path_patterns: true,
        },
    )
}

fn analyze_shell_with_options(
    source: &str,
    options: ShellAnalysisOptions,
) -> Result<ShellAnalysis, ShellParseError> {
    let mut analysis = ShellAnalysis {
        source: source.into(),
        segments: Vec::new(),
        operators: Vec::new(),
        paths: Vec::new(),
        has_redirection: false,
        only_read_safe_redirections: true,
        has_assignment: false,
        opaque: false,
    };
    analyze_source(source, 0, &mut analysis, options)?;
    analysis.opaque |= analysis.segments.iter().any(|segment| segment.opaque)
        || analysis.paths.iter().any(|path| path.dynamic);
    Ok(analysis)
}

fn analyze_source(
    source: &str,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    if depth > MAX_NESTED_DEPTH {
        analysis.opaque = true;
        return Ok(());
    }
    let parser_options = ParserOptions::default();
    let info = SourceInfo {
        source: format!("cagent-bash-depth-{depth}"),
    };
    let mut parser = Parser::new(Cursor::new(source.as_bytes()), &parser_options, &info);
    let program = parser
        .parse_program()
        .map_err(|error| ShellParseError::Parse(error.to_string()))?;
    for list in &program.complete_commands {
        analyze_list(list, depth, analysis, options)?;
    }
    Ok(())
}

fn analyze_list(
    list: &ast::CompoundList,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    for (item_index, ast::CompoundListItem(and_or, separator)) in list.0.iter().enumerate() {
        analyze_pipeline(&and_or.first, depth, analysis, options)?;
        for additional in &and_or.additional {
            match additional {
                ast::AndOr::And(pipeline) => {
                    analysis.operators.push(ShellOperator::And);
                    analyze_pipeline(pipeline, depth, analysis, options)?;
                }
                ast::AndOr::Or(pipeline) => {
                    analysis.operators.push(ShellOperator::Or);
                    analyze_pipeline(pipeline, depth, analysis, options)?;
                }
            }
        }
        if item_index + 1 < list.0.len() || matches!(separator, ast::SeparatorOperator::Async) {
            analysis.operators.push(match separator {
                ast::SeparatorOperator::Async => ShellOperator::Background,
                ast::SeparatorOperator::Sequence => ShellOperator::Sequence,
            });
        }
    }
    Ok(())
}

fn analyze_pipeline(
    pipeline: &ast::Pipeline,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    for (index, command) in pipeline.seq.iter().enumerate() {
        if index > 0 {
            analysis.operators.push(ShellOperator::Pipeline);
        }
        analyze_command(command, depth, analysis, options)?;
    }
    Ok(())
}

fn analyze_command(
    command: &ast::Command,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    match command {
        ast::Command::Simple(command) => analyze_simple(command, depth, analysis, options),
        ast::Command::Compound(command, redirects) => {
            if !matches!(command, ast::CompoundCommand::ForClause(_)) {
                analysis.operators.push(ShellOperator::Subshell);
            }
            analyze_compound(command, depth, analysis, options)?;
            if let Some(redirects) = redirects {
                for redirect in &redirects.0 {
                    analyze_redirect(redirect, depth, analysis, options)?;
                }
            }
            Ok(())
        }
        ast::Command::Function(function) => {
            analysis.opaque = true;
            analyze_compound(&function.body.0, depth, analysis, options)?;
            if let Some(redirects) = &function.body.1 {
                for redirect in &redirects.0 {
                    analyze_redirect(redirect, depth, analysis, options)?;
                }
            }
            Ok(())
        }
        ast::Command::ExtendedTest(_) => {
            analysis.segments.push(ShellSegment {
                words: vec!["[[".into()],
                source_words: vec!["[[".into()],
                raw: command.to_string(),
                depth,
                opaque: true,
                broad_destructive: false,
                suggested_command: None,
            });
            Ok(())
        }
    }
}

fn analyze_compound(
    command: &ast::CompoundCommand,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    match command {
        ast::CompoundCommand::BraceGroup(group) => analyze_list(&group.0, depth, analysis, options),
        ast::CompoundCommand::Subshell(group) => analyze_list(&group.0, depth, analysis, options),
        ast::CompoundCommand::ForClause(clause) => {
            analyze_list(&clause.body.0, depth, analysis, options)
        }
        ast::CompoundCommand::ArithmeticForClause(clause) => {
            analyze_list(&clause.body.0, depth, analysis, options)
        }
        ast::CompoundCommand::CaseClause(clause) => {
            for case in &clause.cases {
                if let Some(command) = &case.cmd {
                    analyze_list(command, depth, analysis, options)?;
                }
            }
            Ok(())
        }
        ast::CompoundCommand::IfClause(clause) => {
            analyze_list(&clause.condition, depth, analysis, options)?;
            analyze_list(&clause.then, depth, analysis, options)?;
            if let Some(elses) = &clause.elses {
                for branch in elses {
                    if let Some(condition) = &branch.condition {
                        analyze_list(condition, depth, analysis, options)?;
                    }
                    analyze_list(&branch.body, depth, analysis, options)?;
                }
            }
            Ok(())
        }
        ast::CompoundCommand::WhileClause(clause) | ast::CompoundCommand::UntilClause(clause) => {
            analyze_list(&clause.0, depth, analysis, options)?;
            analyze_list(&clause.1.0, depth, analysis, options)
        }
        ast::CompoundCommand::Arithmetic(_) => {
            analysis.opaque = true;
            Ok(())
        }
    }
}

fn analyze_simple(
    command: &ast::SimpleCommand,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    let mut raw_words = Vec::new();
    let mut literals = Vec::new();
    if let Some(word) = &command.word_or_name {
        inspect_word(word, depth, analysis, options)?;
        raw_words.push(word.value.clone());
        literals.push(literal_word(word));
    }
    for item in command.prefix.iter().flat_map(|prefix| &prefix.0) {
        match item {
            ast::CommandPrefixOrSuffixItem::Word(word) => {
                inspect_word(word, depth, analysis, options)?;
                raw_words.push(word.value.clone());
                literals.push(literal_word(word));
            }
            ast::CommandPrefixOrSuffixItem::AssignmentWord(_, word) => {
                analysis.has_assignment = true;
                inspect_word(word, depth, analysis, options)?;
            }
            ast::CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
                analyze_redirect(redirect, depth, analysis, options)?;
            }
            ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_, subshell) => {
                analysis.operators.push(ShellOperator::Substitution);
                analyze_list(&subshell.0, depth.saturating_add(1), analysis, options)?;
            }
        }
    }
    for item in command.suffix.iter().flat_map(|suffix| &suffix.0) {
        match item {
            ast::CommandPrefixOrSuffixItem::Word(word)
            | ast::CommandPrefixOrSuffixItem::AssignmentWord(_, word) => {
                // Bash only treats NAME=VALUE tokens before the command name as
                // environment assignments. In argument position they are
                // ordinary words, as in `rustfmt --config skip_children=true`.
                inspect_word(word, depth, analysis, options)?;
                raw_words.push(word.value.clone());
                literals.push(literal_word(word));
            }
            ast::CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
                analyze_redirect(redirect, depth, analysis, options)?;
            }
            ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_, subshell) => {
                analysis.operators.push(ShellOperator::Substitution);
                analyze_list(&subshell.0, depth.saturating_add(1), analysis, options)?;
            }
        }
    }
    if raw_words.is_empty() {
        return Ok(());
    }
    let words = literals
        .iter()
        .zip(&raw_words)
        .map(|(literal, raw)| literal.clone().unwrap_or_else(|| raw.clone()))
        .collect::<Vec<_>>();
    let first_dynamic = literals.iter().position(Option::is_none);
    let broad_destructive = is_broad_destructive(&words);
    let opaque = literals.first().is_none_or(Option::is_none);
    let suggested_command = (!opaque && !broad_destructive).then(|| narrow_suggestion(&words));
    analysis.segments.push(ShellSegment {
        raw: command.to_string(),
        words: words.clone(),
        source_words: raw_words,
        depth,
        opaque,
        broad_destructive,
        suggested_command,
    });

    collect_file_arguments(&words, &literals, analysis);
    unwrap_literal_invocation(&words, &literals, depth, first_dynamic, analysis, options)
}

fn inspect_word(
    word: &ast::Word,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    let pieces = brush_parser::word::parse(&word.value, &ParserOptions::default())
        .map_err(|error| ShellParseError::Parse(error.to_string()))?;
    inspect_word_pieces(&pieces, depth, analysis, false, options)
}

fn inspect_word_pieces(
    pieces: &[brush_parser::word::WordPieceWithSource],
    depth: u8,
    analysis: &mut ShellAnalysis,
    in_double_quotes: bool,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(value)
                if !in_double_quotes
                    && value
                        .chars()
                        .any(|character| matches!(character, '*' | '?' | '[' | '{')) =>
            {
                if options.allow_path_patterns {
                    continue;
                }
                analysis.opaque = true;
            }
            WordPiece::TildePrefix(user) if options.allow_path_patterns && user.is_empty() => {}
            WordPiece::TildePrefix(_)
            | WordPiece::ParameterExpansion(_)
            | WordPiece::ArithmeticExpression(_) => {
                analysis.opaque = true;
            }
            WordPiece::CommandSubstitution(source)
            | WordPiece::BackquotedCommandSubstitution(source) => {
                analysis.operators.push(ShellOperator::Substitution);
                analyze_source(source, depth.saturating_add(1), analysis, options)?;
            }
            WordPiece::DoubleQuotedSequence(nested) => {
                inspect_word_pieces(nested, depth, analysis, true, options)?;
            }
            WordPiece::GettextDoubleQuotedSequence(nested) => {
                analysis.opaque = true;
                inspect_word_pieces(nested, depth, analysis, true, options)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn literal_word(word: &ast::Word) -> Option<String> {
    let pieces = brush_parser::word::parse(&word.value, &ParserOptions::default()).ok()?;
    let mut result = String::new();
    if append_literal_pieces(&pieces, &mut result) {
        Some(result)
    } else {
        None
    }
}

fn append_literal_pieces(
    pieces: &[brush_parser::word::WordPieceWithSource],
    output: &mut String,
) -> bool {
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(value)
            | WordPiece::SingleQuotedText(value)
            | WordPiece::AnsiCQuotedText(value)
            | WordPiece::EscapeSequence(value) => output.push_str(value),
            WordPiece::DoubleQuotedSequence(nested) => {
                if !append_literal_pieces(nested, output) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn analyze_redirect(
    redirect: &ast::IoRedirect,
    depth: u8,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    analysis.has_redirection = true;
    if is_safe_stderr_to_stdout_redirect(redirect) {
        return Ok(());
    }
    match redirect {
        ast::IoRedirect::File(_, kind, target) => {
            if let ast::IoFileRedirectTarget::Filename(word)
            | ast::IoFileRedirectTarget::Duplicate(word) = target
            {
                inspect_word(word, depth, analysis, options)?;
                let literal = literal_word(word);
                if literal.as_deref() != Some("/dev/null") {
                    analysis.only_read_safe_redirections = false;
                }
                push_redirect_path(
                    analysis,
                    literal,
                    &word.value,
                    match kind {
                        ast::IoFileRedirectKind::Read => ShellPathAccess::Read,
                        ast::IoFileRedirectKind::ReadAndWrite => ShellPathAccess::ReadWrite,
                        _ => ShellPathAccess::Write,
                    },
                );
            } else {
                analysis.only_read_safe_redirections = false;
            }
        }
        ast::IoRedirect::OutputAndError(word, _) => {
            inspect_word(word, depth, analysis, options)?;
            let literal = literal_word(word);
            if literal.as_deref() != Some("/dev/null") {
                analysis.only_read_safe_redirections = false;
            }
            push_redirect_path(analysis, literal, &word.value, ShellPathAccess::Write);
        }
        ast::IoRedirect::HereString(_, word) => {
            analysis.only_read_safe_redirections = false;
            inspect_word(word, depth, analysis, options)?;
        }
        ast::IoRedirect::HereDocument(_, document) => {
            analysis.only_read_safe_redirections = false;
            inspect_word(&document.doc, depth, analysis, options)?;
        }
    }
    Ok(())
}

fn is_safe_stderr_to_stdout_redirect(redirect: &ast::IoRedirect) -> bool {
    match redirect {
        ast::IoRedirect::File(
            Some(2),
            ast::IoFileRedirectKind::DuplicateOutput,
            ast::IoFileRedirectTarget::Fd(1),
        )
        | ast::IoRedirect::File(
            Some(2),
            ast::IoFileRedirectKind::DuplicateOutput,
            ast::IoFileRedirectTarget::Fd(2),
        ) => true,
        // brush-parser represents source `2>&1` and `2>&2` as filename targets
        // in some cases even though Bash treats them as descriptor duplication.
        // Keep this narrow: dynamic words, other descriptors, and every other
        // redirection remain permission-checked.
        ast::IoRedirect::File(
            Some(2),
            ast::IoFileRedirectKind::DuplicateOutput,
            ast::IoFileRedirectTarget::Filename(word),
        )
        | ast::IoRedirect::File(
            Some(2),
            ast::IoFileRedirectKind::DuplicateOutput,
            ast::IoFileRedirectTarget::Duplicate(word),
        ) => matches!(word.value.as_str(), "1" | "2"),
        _ => false,
    }
}

/// Records a static redirection target for authorization.
///
/// `/dev/null` is a Unix null device rather than a user-controlled file. It is
/// intentionally exempt from filesystem permission checks so routine stderr
/// suppression does not prompt for external writes.
fn push_redirect_path(
    analysis: &mut ShellAnalysis,
    literal: Option<String>,
    fallback: &str,
    access: ShellPathAccess,
) {
    if literal.as_deref() == Some("/dev/null") {
        return;
    }
    analysis.paths.push(ShellPath {
        value: literal.clone().unwrap_or_else(|| fallback.into()),
        access,
        dynamic: literal.is_none(),
        source: "redirection".into(),
        segment_index: None,
    });
}

fn unwrap_literal_invocation(
    words: &[String],
    literals: &[Option<String>],
    depth: u8,
    first_dynamic: Option<usize>,
    analysis: &mut ShellAnalysis,
    options: ShellAnalysisOptions,
) -> Result<(), ShellParseError> {
    let Some(program) = literals.first().and_then(Clone::clone) else {
        return Ok(());
    };
    let basename = Path::new(&program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&program);
    if matches!(basename, "bash" | "sh" | "zsh") {
        let source_index =
            words.iter().enumerate().skip(1).find_map(|(index, value)| {
                matches!(value.as_str(), "-c" | "-lc").then_some(index + 1)
            });
        match source_index
            .and_then(|index| literals.get(index))
            .and_then(Clone::clone)
        {
            Some(source) if depth < MAX_NESTED_DEPTH => {
                analyze_source(&source, depth + 1, analysis, options)?;
            }
            _ => mark_last_opaque(analysis),
        }
    } else if basename == "eval" {
        let source = literals
            .iter()
            .skip(1)
            .cloned()
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join(" "));
        match source {
            Some(source) if depth < MAX_NESTED_DEPTH => {
                analyze_source(&source, depth + 1, analysis, options)?;
            }
            _ => mark_last_opaque(analysis),
        }
    } else if matches!(
        basename,
        "env" | "command" | "builtin" | "nohup" | "time" | "sudo"
    ) {
        let index = wrapper_command_index(basename, words);
        if index < words.len() && first_dynamic.is_none_or(|dynamic| dynamic > index) {
            let nested = words[index..].join(" ");
            analyze_source(&nested, depth.saturating_add(1), analysis, options)?;
        } else {
            mark_last_opaque(analysis);
        }
    }
    Ok(())
}

fn wrapper_command_index(wrapper: &str, words: &[String]) -> usize {
    let mut index = 1;
    while index < words.len() {
        let word = &words[index];
        if (wrapper == "env" && word.contains('='))
            || (word.starts_with('-') && word.as_str() != "--")
        {
            index += 1;
        } else if word == "--" {
            index += 1;
            break;
        } else {
            break;
        }
    }
    index
}

fn mark_last_opaque(analysis: &mut ShellAnalysis) {
    analysis.opaque = true;
    if let Some(segment) = analysis.segments.last_mut() {
        segment.opaque = true;
        segment.suggested_command = None;
    }
}

fn narrow_suggestion(words: &[String]) -> Vec<String> {
    let retain = if words.len() <= 1 { 1 } else { 2 };
    let mut suggestion = words.iter().take(retain).cloned().collect::<Vec<_>>();
    if words.len() > retain {
        suggestion.push("*".into());
    }
    suggestion
}

fn is_broad_destructive(words: &[String]) -> bool {
    let command = words.first().map(String::as_str).unwrap_or_default();
    let has_recursive = words
        .iter()
        .any(|word| matches!(word.as_str(), "-r" | "-R" | "-rf" | "-fr" | "--recursive"));
    let broad_target = words.iter().skip(1).any(|word| {
        matches!(
            word.as_str(),
            "/" | "/*" | "." | ".." | "~" | "$HOME" | "${HOME}"
        )
    });
    (matches!(command, "rm" | "chmod" | "chown") && has_recursive && broad_target)
        || (command == "dd" && words.iter().any(|word| word.starts_with("of=/dev/")))
        || (matches!(command, "mkfs" | "wipefs")
            && words.iter().any(|word| word.starts_with("/dev/")))
}

fn collect_file_arguments(
    words: &[String],
    literals: &[Option<String>],
    analysis: &mut ShellAnalysis,
) {
    let command = words.first().map(String::as_str).unwrap_or_default();
    let access = match command {
        "cat" | "head" | "tail" | "less" | "stat" | "file" | "grep" | "rg" | "find" => {
            Some(ShellPathAccess::Read)
        }
        "rm" | "mkdir" | "rmdir" | "touch" | "truncate" | "chmod" | "chown" => {
            Some(ShellPathAccess::Write)
        }
        "cp" | "mv" | "install" => Some(ShellPathAccess::ReadWrite),
        _ => None,
    };
    let Some(access) = access else { return };
    for (index, word) in words.iter().enumerate().skip(1) {
        if word.starts_with('-') || (command == "find" && index > 1) {
            continue;
        }
        let literal = literals.get(index).and_then(Clone::clone);
        analysis.paths.push(ShellPath {
            value: literal.clone().unwrap_or_else(|| word.clone()),
            access,
            dynamic: literal.is_none(),
            source: "argument".into(),
            segment_index: Some(analysis.segments.len() - 1),
        });
    }
}

#[cfg(test)]
mod safe_tests {
    use super::*;

    #[test]
    fn documented_safe_examples_are_classified() {
        for example in safe_shell_examples() {
            assert!(classify_read_safe_shell(example).is_some(), "{example}");
        }
    }

    #[test]
    fn unsafe_or_ambiguous_shell_syntax_falls_back() {
        for command in [
            "uniq input output",
            "base64 --output x input",
            "find . -exec rm {} \\;",
            "find . -files0-from /tmp/roots",
            "rg --pre processor needle",
            "rg --search-zip needle archive.zip",
            "rg -z needle archive.zip",
            "grep -R needle src",
            "grep --dereference-recursive needle src",
            "du -L src",
            "du --dereference src",
            "git branch new-branch",
            "git diff --output=diff.txt",
            "git diff --ext-diff",
            "sed -i 's/a/b/' file",
            "sed -n '1,2,3p' file",
            "./cat README.md",
            "FOO=bar cat README.md",
            "cat README.md > out",
            "cat $(echo README.md)",
            "cat *.md",
            "echo ~",
            "f() { cat README.md; }",
            "if true; then cat README.md; fi",
            "cat README.md &",
            "cd crates || cat Cargo.toml",
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }

        assert!(classify_read_safe_shell("jq --from-file filter.jq data.json").is_none());
        assert!(classify_read_safe_shell("jq 'include \"helper\"; .' data.json").is_none());
        assert_eq!(
            classify_read_safe_shell("rg -f /tmp/patterns src")
                .unwrap()
                .paths,
            ["/tmp/patterns", "src"]
        );
        assert_eq!(
            classify_read_safe_shell("grep -f /tmp/patterns src")
                .unwrap()
                .paths,
            ["/tmp/patterns", "src"]
        );
    }

    #[test]
    fn quoted_metacharacters_are_literal_across_safe_commands() {
        let inventory = ShellCommandInventory::synthetic(&["echo", "cat", "rg"]);
        for command in [
            r#"echo "*""#,
            r#"echo "?""#,
            r#"echo "[abc]""#,
            r#"echo "{a,b}""#,
            r#"cat "README[1].md""#,
            r#"rg -n "ShellExecutor|TerminalSupervisor|ShellConfig|shell:" "src[legacy]""#,
        ] {
            let assessment = crate::tools::shell::safe::assess_read_safe_shell(command);
            assert!(
                assessment.read_safe,
                "{command}: {:?}",
                assessment.fallback_reason
            );
            assert!(
                crate::tools::shell::safe::authorize_available_safe_shell(command, &inventory)
                    .is_some(),
                "{command}"
            );
        }

        assert_eq!(
            authorize_available_safe_shell(r#"cat "README[1].md""#, &inventory)
                .unwrap()
                .paths,
            ["README[1].md"]
        );
    }

    #[test]
    fn reported_compound_search_is_safe_when_commands_are_available() {
        let inventory = ShellCommandInventory::synthetic(&["rg", "head"]);
        let command = r#"rg -n "ShellExecutor|TerminalSupervisor|ShellConfig|shell:" . | head -n 120; rg -n "\[shell\]|output_bytes|buffer_bytes|ansi_output|terminal" . | head -180"#;
        let classification = authorize_available_safe_shell(command, &inventory)
            .expect("registered, available compound search should be read-safe");

        assert_eq!(classification.paths, ["."]);
        assert_eq!(classification.exploration.as_ref().map(Vec::len), Some(2));
        assert_eq!(classification.presentation, None);
    }

    #[test]
    fn leading_dash_rg_searches_are_safe_in_compounds() {
        let inventory = ShellCommandInventory::synthetic(&["rg", "head"]);
        let command = r#"rg -n -i -- '--prompt|prompt' SPEC.md | head -n 120; rg -n -i -- '--shell|shell' SPEC.md | head -180"#;
        let classification = authorize_available_safe_shell(command, &inventory)
            .expect("correctly terminated leading-dash rg searches should be read-safe");

        assert_eq!(classification.paths, ["SPEC.md"]);
        assert_eq!(
            classification
                .exploration
                .unwrap()
                .into_iter()
                .map(|activity| (activity.query, activity.paths))
                .collect::<Vec<_>>(),
            [
                (Some("--prompt|prompt".into()), vec!["SPEC.md".into()]),
                (Some("--shell|shell".into()), vec!["SPEC.md".into()]),
            ]
        );
    }

    #[test]
    fn tail_numeric_shorthand_is_safe_in_compound_searches() {
        let command = r#"sed -n '680,750p' crates/cagent-agent/src/config/mod.rs; sed -n '900,940p' crates/cagent-agent/src/config/mod.rs; rg -n "ConfigSnapshot::parse|status_line\(|diff_context_lines\(|composer_max_rows\(" crates/cagent-agent/src/config/mod.rs | tail -40"#;

        assert!(classify_read_safe_shell(command).is_some());
        let inventory = ShellCommandInventory::synthetic(&["sed", "rg", "tail"]);
        assert!(authorize_available_safe_shell(command, &inventory).is_some());
    }

    #[test]
    fn unquoted_globs_and_dynamic_double_quotes_fall_back() {
        for command in [
            "echo *",
            "echo ?",
            "echo [abc]",
            "echo {a,b}",
            "cat *.md",
            r#"cat "$var""#,
            r#"cat "$(printf README.md)""#,
            "cat `printf README.md`",
            r#"echo "$((1 + 1))""#,
            r#"echo $"literal""#,
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
    }

    #[test]
    fn literal_printf_escapes_are_safe_but_directives_and_patterns_are_not() {
        let inventory = ShellCommandInventory::synthetic(&["printf"]);
        for command in [
            r#"printf '\n--- docs ---\n'"#,
            r#"printf '\e[36mSearching docs...\e[0m\n'"#,
            r#"printf '%s\n' '--- files ---'"#,
            r#"printf '100%%\n'"#,
        ] {
            assert!(classify_read_safe_shell(command).is_some(), "{command}");
            assert!(
                authorize_available_safe_shell(command, &inventory).is_some(),
                "{command}"
            );
        }

        for command in [
            r#"printf '%q' 'text'"#,
            r#"printf '%s\n'"#,
            r#"printf '%s %s\n' 'only one'"#,
            r#"printf '*.md'"#,
            r#"printf '{docs}'"#,
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
    }

    #[test]
    fn literal_echo_operands_that_start_with_dashes_are_safe() {
        let inventory = ShellCommandInventory::synthetic(&["cat", "head", "echo"]);
        for command in [
            "echo ---",
            "echo --heading",
            "cat SPEC.md 2>/dev/null | head -100; echo ---; cat TEST.md 2>/dev/null | head -100",
        ] {
            assert!(classify_read_safe_shell(command).is_some(), "{command}");
            assert!(
                authorize_available_safe_shell(command, &inventory).is_some(),
                "{command}"
            );
        }
    }

    #[test]
    fn standalone_commands_have_semantic_presentation() {
        assert_eq!(
            classify_read_safe_shell(
                "sed -n '180,300p' crates/cagent-agent/src/tools/shell/analysis.rs"
            )
            .unwrap()
            .presentation,
            Some(SafeShellPresentation::Read)
        );
        assert_eq!(
            classify_read_safe_shell("cat README.md")
                .unwrap()
                .presentation,
            Some(SafeShellPresentation::Read)
        );
        let head = classify_read_safe_shell("head -n 80").unwrap();
        assert_eq!(head.presentation, None);
        assert!(head.paths.is_empty());
        assert_eq!(
            classify_read_safe_shell("rg --files").unwrap().presentation,
            Some(SafeShellPresentation::List)
        );
        assert_eq!(
            classify_read_safe_shell("rg needle src")
                .unwrap()
                .presentation,
            Some(SafeShellPresentation::Search)
        );
        assert_eq!(
            classify_read_safe_shell("fd 'SPEC.md|Cargo.toml|AGENTS.md' .")
                .unwrap()
                .presentation,
            Some(SafeShellPresentation::List)
        );
        let compound = classify_read_safe_shell("cat README.md && ls").unwrap();
        assert_eq!(compound.presentation, None);
        assert_eq!(
            compound
                .exploration
                .unwrap()
                .into_iter()
                .map(|activity| (activity.presentation, activity.paths))
                .collect::<Vec<_>>(),
            [
                (SafeShellPresentation::Read, vec!["README.md".into()]),
                (SafeShellPresentation::List, vec![".".into()]),
            ]
        );
        let jq = classify_read_safe_shell("cat README.md | jq .").unwrap();
        assert_eq!(jq.paths, ["README.md"]);
        assert_eq!(jq.exploration.as_ref().map(Vec::len), Some(1));
        let nested = classify_read_safe_shell("bash -lc 'cat README.md'").unwrap();
        assert_eq!(nested.presentation, Some(SafeShellPresentation::Read));
        assert_eq!(nested.paths, ["README.md"]);

        let after_cd =
            classify_read_safe_shell("cd crates && cat cagent-agent/Cargo.toml").unwrap();
        assert_eq!(after_cd.paths, ["crates", "crates/cagent-agent/Cargo.toml"]);
        assert_eq!(
            classify_read_safe_shell("find src tests -name '*.rs'")
                .unwrap()
                .paths,
            ["src", "tests"]
        );
    }
}
