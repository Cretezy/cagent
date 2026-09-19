use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::config::DEFAULT_AGENT_NAME;

/// Stable IDs for the independently configurable statusline modules.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusLineModule {
    Mode,
    Agent,
    Model,
    Provider,
    Fast,
    #[serde(rename = "provider_usage")]
    ProviderUsage,
    Context,
    Cost,
    Tokens,
    #[serde(rename = "token_rate")]
    TokenRate,
    Cache,
    #[serde(rename = "cache_tokens")]
    CacheTokens,
    ContextTokens,
    Hint,
}

impl StatusLineModule {
    pub const ALL: [Self; 14] = [
        Self::Mode,
        Self::Agent,
        Self::Model,
        Self::Provider,
        Self::Fast,
        Self::Context,
        Self::ProviderUsage,
        Self::Cost,
        Self::Tokens,
        Self::TokenRate,
        Self::Cache,
        Self::CacheTokens,
        Self::ContextTokens,
        Self::Hint,
    ];

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Mode => "active permission and behavior mode",
            Self::Agent => "active non-default agent profile",
            Self::Model => "model and reasoning effort",
            Self::Provider => "active model provider",
            Self::Fast => "whether the Fast service tier is active",
            Self::ProviderUsage => "provider-reported remaining limits",
            Self::Context => "context window percentage",
            Self::Cost => "complete session cost",
            Self::Tokens => "session input and output tokens",
            Self::TokenRate => "input and output tokens per second over the last minute",
            Self::Cache => "session cache read rate",
            Self::CacheTokens => "session cache read and write tokens",
            Self::ContextTokens => "context window tokens used",
            Self::Hint => "available composer action shortcuts",
        }
    }

    #[must_use]
    pub const fn default_color(self) -> StatusLineColor {
        match self {
            Self::Mode => StatusLineColor::White,
            Self::Agent => StatusLineColor::White,
            Self::Model => StatusLineColor::LightCyan,
            Self::Provider => StatusLineColor::Magenta,
            Self::Fast => StatusLineColor::Red,
            Self::ProviderUsage => StatusLineColor::Green,
            Self::Context => StatusLineColor::Yellow,
            Self::Cost => StatusLineColor::LightRed,
            Self::Tokens => StatusLineColor::Gray,
            Self::TokenRate => StatusLineColor::Gray,
            Self::Cache => StatusLineColor::Blue,
            Self::CacheTokens => StatusLineColor::Gray,
            Self::ContextTokens => StatusLineColor::White,
            Self::Hint => StatusLineColor::DarkGray,
        }
    }
}

impl fmt::Display for StatusLineModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mode => "mode",
            Self::Agent => "agent",
            Self::Model => "model",
            Self::Provider => "provider",
            Self::Fast => "fast",
            Self::ProviderUsage => "provider_usage",
            Self::Context => "context",
            Self::Cost => "cost",
            Self::Tokens => "tokens",
            Self::TokenRate => "token_rate",
            Self::Cache => "cache",
            Self::CacheTokens => "cache_tokens",
            Self::ContextTokens => "context_tokens",
            Self::Hint => "hint",
        })
    }
}

impl FromStr for StatusLineModule {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "mode" => Ok(Self::Mode),
            "agent" => Ok(Self::Agent),
            "model" => Ok(Self::Model),
            "provider" => Ok(Self::Provider),
            "fast" => Ok(Self::Fast),
            "provider_usage" => Ok(Self::ProviderUsage),
            "context" => Ok(Self::Context),
            "cost" => Ok(Self::Cost),
            "tokens" => Ok(Self::Tokens),
            "token_rate" => Ok(Self::TokenRate),
            "cache" => Ok(Self::Cache),
            "cache_tokens" => Ok(Self::CacheTokens),
            "context_tokens" => Ok(Self::ContextTokens),
            "hint" => Ok(Self::Hint),
            _ => Err(format!("unknown statusline module {value:?}")),
        }
    }
}

/// Portable foreground colors supported by statusline configuration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum StatusLineColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
    Rgb(u8, u8, u8),
}

impl StatusLineColor {
    pub const NAMED: [Self; 16] = [
        Self::Black,
        Self::Red,
        Self::Green,
        Self::Yellow,
        Self::Blue,
        Self::Magenta,
        Self::Cyan,
        Self::Gray,
        Self::DarkGray,
        Self::LightRed,
        Self::LightGreen,
        Self::LightYellow,
        Self::LightBlue,
        Self::LightMagenta,
        Self::LightCyan,
        Self::White,
    ];
}

impl fmt::Display for StatusLineColor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Black => formatter.write_str("black"),
            Self::Red => formatter.write_str("red"),
            Self::Green => formatter.write_str("green"),
            Self::Yellow => formatter.write_str("yellow"),
            Self::Blue => formatter.write_str("blue"),
            Self::Magenta => formatter.write_str("magenta"),
            Self::Cyan => formatter.write_str("cyan"),
            Self::Gray => formatter.write_str("gray"),
            Self::DarkGray => formatter.write_str("dark-gray"),
            Self::LightRed => formatter.write_str("light-red"),
            Self::LightGreen => formatter.write_str("light-green"),
            Self::LightYellow => formatter.write_str("light-yellow"),
            Self::LightBlue => formatter.write_str("light-blue"),
            Self::LightMagenta => formatter.write_str("light-magenta"),
            Self::LightCyan => formatter.write_str("light-cyan"),
            Self::White => formatter.write_str("white"),
            Self::Rgb(red, green, blue) => write!(formatter, "#{red:02X}{green:02X}{blue:02X}"),
        }
    }
}

impl FromStr for StatusLineColor {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.to_ascii_lowercase();
        let color = match normalized.as_str() {
            "black" => Self::Black,
            "red" => Self::Red,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "blue" => Self::Blue,
            "magenta" => Self::Magenta,
            "cyan" => Self::Cyan,
            "gray" => Self::Gray,
            "dark-gray" => Self::DarkGray,
            "light-red" => Self::LightRed,
            "light-green" => Self::LightGreen,
            "light-yellow" => Self::LightYellow,
            "light-blue" => Self::LightBlue,
            "light-magenta" => Self::LightMagenta,
            "light-cyan" => Self::LightCyan,
            "white" => Self::White,
            _ => {
                let Some(hex) = value.strip_prefix('#').filter(|hex| hex.len() == 6) else {
                    return Err(format!("invalid statusline color {value:?}"));
                };
                let component = |range| {
                    u8::from_str_radix(&hex[range], 16)
                        .map_err(|_| format!("invalid statusline color {value:?}"))
                };
                Self::Rgb(component(0..2)?, component(2..4)?, component(4..6)?)
            }
        };
        Ok(color)
    }
}

/// Resolved statusline layout and colors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusLineConfig {
    pub modules: Vec<StatusLineModule>,
    pub colors: BTreeMap<StatusLineModule, StatusLineColor>,
}

impl StatusLineConfig {
    #[must_use]
    pub fn color(&self, module: StatusLineModule) -> StatusLineColor {
        if module == StatusLineModule::Mode {
            return StatusLineColor::White;
        }
        self.colors
            .get(&module)
            .copied()
            .unwrap_or_else(|| module.default_color())
    }
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self {
            modules: vec![
                StatusLineModule::Mode,
                StatusLineModule::Agent,
                StatusLineModule::Model,
                StatusLineModule::Provider,
                StatusLineModule::Fast,
                StatusLineModule::Context,
                StatusLineModule::ProviderUsage,
                StatusLineModule::Cost,
                StatusLineModule::Hint,
            ],
            colors: StatusLineModule::ALL
                .into_iter()
                .map(|module| (module, module.default_color()))
                .collect(),
        }
    }
}

/// Current session values consumed by the frontend-neutral statusline formatter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StatusLineValues {
    pub mode: String,
    /// Resolved from the active mode profile for the rendered statusline.
    pub mode_color: Option<StatusLineColor>,
    pub agent: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub fast: bool,
    pub provider_usage: Option<crate::ProviderUsageReport>,
    pub effort: Option<String>,
    pub context_percent: Option<u64>,
    pub context_used_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub usage: crate::SessionUsage,
    pub token_rate: TokenRate,
    pub subagents: usize,
    pub background: usize,
    pub composer_has_text: bool,
    pub conversation_is_working: bool,
    /// Replaces the standard composer/work hint for a focused editing state.
    pub primary_hint: Option<String>,
    /// Contextual frontend hints appended to the standard composer/work hint.
    pub hints: Vec<String>,
}

/// Average tokens per second during the trailing one-minute statusline window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenRate {
    pub input: u64,
    pub output: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TokenRateSample {
    at: Duration,
    rate: TokenRate,
}

/// Tracks positive changes in cumulative session usage over a trailing minute.
#[derive(Clone, Debug, Default)]
pub struct TokenRateTracker {
    model: Option<String>,
    baseline: Option<TokenRate>,
    samples: VecDeque<TokenRateSample>,
    current: TokenRate,
    model_work: Duration,
    model_active: bool,
    last_updated: Option<Instant>,
}

impl TokenRateTracker {
    pub const WINDOW: Duration = Duration::from_secs(60);

    /// Observes cumulative usage and whether model work is currently active.
    pub fn observe(&mut self, usage: &crate::SessionUsage, model: Option<&str>, active: bool) {
        self.observe_at(usage, model, active, Instant::now());
    }

    #[must_use]
    pub const fn current(&self) -> TokenRate {
        self.current
    }

    fn observe_at(
        &mut self,
        usage: &crate::SessionUsage,
        model: Option<&str>,
        active: bool,
        now: Instant,
    ) {
        self.advance_clock(now);
        let observed = TokenRate {
            input: display_input_tokens(usage).unwrap_or_default(),
            output: usage.output_tokens.unwrap_or_default(),
        };
        if self.model.as_deref() != model {
            self.model = model.map(str::to_owned);
            self.baseline = Some(observed);
            self.samples.clear();
            self.current = TokenRate::default();
            self.model_work = Duration::ZERO;
            self.model_active = active;
            return;
        }
        let Some(baseline) = self.baseline.replace(observed) else {
            self.model_active = active;
            self.expire_samples();
            return;
        };
        if observed.input < baseline.input || observed.output < baseline.output {
            self.samples.clear();
            self.current = TokenRate::default();
            self.model_active = active;
            return;
        }
        let rate = TokenRate {
            input: observed.input.saturating_sub(baseline.input),
            output: observed.output.saturating_sub(baseline.output),
        };
        self.model_active = active;
        if rate == TokenRate::default() {
            return;
        }
        self.samples.push_back(TokenRateSample {
            at: self.model_work,
            rate,
        });
        self.expire_samples();
    }

    fn advance_clock(&mut self, now: Instant) {
        if let Some(last_updated) = self.last_updated
            && self.model_active
        {
            self.model_work = self
                .model_work
                .saturating_add(now.saturating_duration_since(last_updated));
        }
        self.last_updated = Some(now);
    }

    fn expire_samples(&mut self) {
        while self
            .samples
            .front()
            .is_some_and(|sample| self.model_work.saturating_sub(sample.at) >= Self::WINDOW)
        {
            self.samples.pop_front();
        }
        let total = self
            .samples
            .iter()
            .fold(TokenRate::default(), |total, sample| TokenRate {
                input: total.input.saturating_add(sample.rate.input),
                output: total.output.saturating_add(sample.rate.output),
            });
        self.current = TokenRate {
            input: average_per_second(total.input, self.model_work),
            output: average_per_second(total.output, self.model_work),
        };
    }
}

fn average_per_second(tokens: u64, model_work: Duration) -> u64 {
    let seconds = model_work.min(TokenRateTracker::WINDOW).as_secs().max(1);
    tokens.saturating_add(seconds / 2) / seconds
}

/// One semantic statusline module ready for frontend styling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusLineSegment {
    pub module: StatusLineModule,
    pub label: Option<String>,
    pub value: String,
    /// Optional styled pieces of `value`. When present, their text concatenates to `value`.
    pub value_spans: Vec<StatusLineValueSpan>,
    /// Model reasoning effort, kept separate so frontends can style it independently.
    pub effort: Option<String>,
    pub color: StatusLineColor,
}

/// A piece of a status-line value with its own text emphasis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusLineValueSpan {
    pub text: String,
    pub bold: bool,
}

impl StatusLineSegment {
    #[must_use]
    pub fn text(&self) -> String {
        format!(
            "{}{}{}",
            self.label.as_deref().unwrap_or_default(),
            self.value,
            self.effort
                .as_deref()
                .map_or_else(String::new, |effort| format!(" {effort}"))
        )
    }

    fn width(&self) -> usize {
        self.text().width()
    }
}

/// Formats, compacts, and responsively filters statusline modules for `width` columns.
#[must_use]
pub fn prepare_status_line(
    config: &StatusLineConfig,
    values: &StatusLineValues,
    width: usize,
) -> Vec<StatusLineSegment> {
    const REMOVAL_ORDER: [StatusLineModule; 14] = [
        StatusLineModule::Hint,
        StatusLineModule::CacheTokens,
        StatusLineModule::Cache,
        StatusLineModule::TokenRate,
        StatusLineModule::Tokens,
        StatusLineModule::ContextTokens,
        StatusLineModule::ProviderUsage,
        StatusLineModule::Fast,
        StatusLineModule::Provider,
        StatusLineModule::Agent,
        StatusLineModule::Context,
        StatusLineModule::Cost,
        StatusLineModule::Model,
        StatusLineModule::Mode,
    ];

    let mut segments = config
        .modules
        .iter()
        .filter_map(|module| segment(*module, config.color(*module), values))
        .collect::<Vec<_>>();
    if let Some(mode_color) = values.mode_color
        && let Some(mode) = segments
            .iter_mut()
            .find(|segment| segment.module == StatusLineModule::Mode)
    {
        mode.color = mode_color;
    }
    if status_width(&segments) <= width {
        return segments;
    }

    if let Some(context) = segments
        .iter_mut()
        .find(|segment| segment.module == StatusLineModule::Context)
    {
        context.label = None;
    }

    for module in REMOVAL_ORDER {
        if status_width(&segments) <= width || segments.len() <= 1 {
            break;
        }
        if let Some(index) = segments.iter().position(|segment| segment.module == module) {
            segments.remove(index);
        }
    }

    if status_width(&segments) > width
        && let Some(segment) = segments.first_mut()
    {
        fit_segment(segment, width);
    }
    segments
}

fn segment(
    module: StatusLineModule,
    color: StatusLineColor,
    values: &StatusLineValues,
) -> Option<StatusLineSegment> {
    let value_spans = Vec::new();
    let (label, value) = match module {
        StatusLineModule::Mode => (
            None,
            if values.mode.is_empty() {
                "ask".into()
            } else {
                values.mode.clone()
            },
        ),
        StatusLineModule::Agent
            if values.agent.is_empty() || values.agent == DEFAULT_AGENT_NAME =>
        {
            return None;
        }
        StatusLineModule::Agent => (None, values.agent.clone()),
        StatusLineModule::Model => {
            let value = values
                .model
                .as_ref()
                .map_or_else(|| "none".into(), Clone::clone);
            (None, value)
        }
        StatusLineModule::Provider => (
            None,
            values.provider.clone().unwrap_or_else(|| "none".into()),
        ),
        StatusLineModule::Fast if !values.fast => return None,
        StatusLineModule::Fast => (None, "fast".into()),
        StatusLineModule::ProviderUsage => {
            let report = values.provider_usage.as_ref()?;
            if report.windows.is_empty() {
                return None;
            }
            (
                None,
                report
                    .windows
                    .iter()
                    .map(|window| format!("{} {}% left", window.label, window.remaining_percent))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
        StatusLineModule::Context => {
            let percent = values.context_percent?;
            let value = format!("{percent}% ctx used");
            (None, value)
        }
        StatusLineModule::Cost => (None, format_session_cost(&values.usage)?),
        StatusLineModule::Tokens => {
            let usage = &values.usage;
            if usage.input_tokens.is_none() && usage.output_tokens.is_none() {
                return None;
            }
            let input = display_input_tokens(usage);
            let input = input
                .map(format_compact_tokens)
                .unwrap_or_else(|| "—".into());
            let output = usage
                .output_tokens
                .map(format_compact_tokens)
                .unwrap_or_else(|| "—".into());
            (None, format!("in:{input} out:{output}"))
        }
        StatusLineModule::TokenRate => {
            if values.token_rate == TokenRate::default() {
                return None;
            }
            (
                None,
                format!(
                    "in:{}/s out:{}/s",
                    values.token_rate.input, values.token_rate.output
                ),
            )
        }
        StatusLineModule::Cache => {
            let rate = values.usage.cache_rate_percent()?;
            (None, format!("{rate}% cached"))
        }
        StatusLineModule::CacheTokens => {
            let usage = &values.usage;
            if usage.cache_read_input_tokens.is_none() && usage.cache_write_input_tokens.is_none() {
                return None;
            }
            let read = usage
                .cache_read_input_tokens
                .map(format_compact_tokens)
                .unwrap_or_else(|| "—".into());
            let written = usage
                .cache_write_input_tokens
                .map(format_compact_tokens)
                .unwrap_or_else(|| "—".into());
            (None, format!("cache r:{read}/w:{written}"))
        }
        StatusLineModule::ContextTokens => {
            let (Some(used), Some(window)) = (values.context_used_tokens, values.context_window)
            else {
                return None;
            };
            (
                None,
                format!(
                    "{}/{}",
                    format_compact_tokens(used),
                    format_compact_tokens(window)
                ),
            )
        }
        StatusLineModule::Hint => {
            let mut hints = Vec::new();
            if let Some(hint) = &values.primary_hint {
                return Some(StatusLineSegment {
                    module,
                    label: None,
                    value: hint.clone(),
                    value_spans: Vec::new(),
                    effort: None,
                    color,
                });
            } else if values.composer_has_text {
                hints.push(if values.conversation_is_working {
                    "Tab to queue, Enter to steer".into()
                } else {
                    "Enter to send, Ctrl+C to clear".into()
                });
            }
            hints.extend(values.hints.iter().filter(|hint| !hint.is_empty()).cloned());
            if !values.composer_has_text && (values.background != 0 || values.subagents != 0) {
                hints.push(format!(
                    "Alt+↓ {} background",
                    values.background.saturating_add(values.subagents)
                ));
            }
            if hints.is_empty() {
                return None;
            }
            (None, hints.join(" · "))
        }
    };
    let effort = (module == StatusLineModule::Model && values.model.is_some())
        .then(|| values.effort.clone())
        .flatten();
    Some(StatusLineSegment {
        module,
        label,
        value,
        value_spans,
        effort,
        color,
    })
}

fn display_input_tokens(usage: &crate::SessionUsage) -> Option<u64> {
    usage
        .non_cached_input_tokens
        .or_else(|| {
            usage
                .input_tokens
                .zip(usage.cache_read_input_tokens)
                .map(|(input, cached)| input.saturating_sub(cached))
        })
        .or(usage.input_tokens)
}

fn status_width(segments: &[StatusLineSegment]) -> usize {
    segments.iter().map(StatusLineSegment::width).sum::<usize>()
        + segments.len().saturating_sub(1) * 3
}

fn fit_segment(segment: &mut StatusLineSegment, width: usize) {
    if segment.width() <= width {
        return;
    }
    segment.value_spans.clear();
    if segment
        .label
        .as_deref()
        .is_some_and(|label| label.width() >= width)
    {
        segment.label = None;
    }
    let label_width = segment.label.as_deref().map_or(0, UnicodeWidthStr::width);
    let available = width.saturating_sub(label_width);
    if let Some(effort) = segment.effort.as_deref() {
        let effort_width = effort.width().saturating_add(1);
        if effort_width < available {
            segment.value = ellipsize(&segment.value, available - effort_width);
            return;
        }
        segment.effort = None;
    }
    segment.value = ellipsize(&segment.value, available);
}

pub fn format_compact_tokens(tokens: u64) -> String {
    const UNITS: [&str; 5] = ["", "k", "M", "B", "T"];
    if tokens < 1_000 {
        tokens.to_string()
    } else {
        let mut unit = 0;
        let mut scale = 1_u128;
        while u128::from(tokens) >= scale.saturating_mul(1_000) && unit < UNITS.len() - 1 {
            scale *= 1_000;
            unit += 1;
        }

        // Round to two fractional digits with integer arithmetic so large
        // token counts do not lose precision through floating-point values.
        let mut hundredths = (u128::from(tokens) * 100 + scale / 2) / scale;
        if hundredths >= 100_000 && unit < UNITS.len() - 1 {
            hundredths = (hundredths + 500) / 1_000;
            unit += 1;
        }
        let whole = hundredths / 100;
        let fraction = hundredths % 100;
        let value = if fraction == 0 {
            whole.to_string()
        } else if fraction.is_multiple_of(10) {
            format!("{whole}.{}", fraction / 10)
        } else {
            format!("{whole}.{fraction:02}")
        };
        format!("{value}{}", UNITS[unit])
    }
}

#[must_use]
pub fn format_session_cost(usage: &crate::SessionUsage) -> Option<String> {
    let cost = usage.total_cost()?;
    let prefix = if usage.cost_source != Some(crate::CostSource::ProviderReported) {
        "~"
    } else {
        ""
    };
    let amount = format_currency(
        cost.total_cost.as_deref().unwrap_or("0"),
        CurrencyFormat::Short,
    );
    Some(if cost.currency == "USD" {
        format!("{prefix}${amount}")
    } else {
        format!("{prefix}{amount} {}", cost.currency)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrencyFormat {
    /// Use three fractional digits below one dollar and two otherwise.
    /// Fractional digits are not grouped.
    Short,
    /// Preserve up to six fractional digits and group fractional digits in
    /// threes below one dollar; use the ordinary two-decimal form otherwise.
    Long,
}

/// Formats a positive decimal cost for a frontend display.
#[must_use]
pub fn format_currency(value: &str, format: CurrencyFormat) -> String {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty() || !whole.chars().all(|digit| digit.is_ascii_digit()) {
        return "0.00".into();
    }
    if !fraction.chars().all(|digit| digit.is_ascii_digit()) {
        return "0.00".into();
    }
    let mut digits = whole.trim_start_matches('0').to_owned();
    if digits.is_empty() {
        digits.push('0');
    }
    let minimum_fraction_digits = if digits == "0" { 3 } else { 2 };
    let maximum_fraction_digits = match (format, digits == "0") {
        (CurrencyFormat::Short, true) => 3,
        (CurrencyFormat::Long, true) => 6,
        // Both displays use the ordinary two-decimal currency form once the
        // amount reaches one dollar. Long only differs below one dollar,
        // where it preserves and groups the recorded fractional digits.
        (_, false) => 2,
    };
    let fraction = format!("{fraction:0<minimum_fraction_digits$}");
    let fraction = &fraction[..fraction.len().min(maximum_fraction_digits)];

    let fraction = match (format, digits == "0") {
        (CurrencyFormat::Long, true) => group_digits_from_left(fraction),
        _ => fraction.to_owned(),
    };
    format!("{}.{}", group_digits_from_right(&digits), fraction)
}

/// Formats a cost using the detailed representation used by exec output.
///
/// Kept as a convenience wrapper for callers that previously used the
/// formatter before display modes were introduced.
#[must_use]
pub fn format_cost_amount(value: &str) -> String {
    format_currency(value, CurrencyFormat::Long)
}

fn group_digits_from_right(digits: &str) -> String {
    let first_group_len = match digits.len() % 3 {
        0 => 3,
        length => length,
    };
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    grouped.push_str(&digits[..first_group_len]);
    for chunk in digits.as_bytes()[first_group_len..].chunks(3) {
        grouped.push(',');
        grouped.push_str(std::str::from_utf8(chunk).expect("cost digits are ASCII"));
    }
    grouped
}

fn group_digits_from_left(digits: &str) -> String {
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.bytes().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(char::from(digit));
    }
    grouped
}

fn ellipsize(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".into();
    }
    let mut result = String::new();
    let target = width - 1;
    for grapheme in value.graphemes(true) {
        if result.width() + grapheme.width() > target {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> StatusLineValues {
        StatusLineValues {
            mode: "ask".into(),
            mode_color: Some(StatusLineColor::LightYellow),
            agent: "review".into(),
            model: Some("gpt-example".into()),
            provider: Some("openrouter".into()),
            fast: false,
            provider_usage: None,
            effort: Some("high".into()),
            context_percent: Some(42),
            context_used_tokens: Some(42_000),
            context_window: Some(128_000),
            usage: crate::SessionUsage::default(),
            token_rate: TokenRate::default(),
            subagents: 2,
            background: 1,
            composer_has_text: false,
            conversation_is_working: false,
            primary_hint: None,
            hints: Vec::new(),
        }
    }

    #[test]
    fn defaults_use_expected_colors() {
        let config = StatusLineConfig::default();
        assert_eq!(
            config.modules,
            [
                StatusLineModule::Mode,
                StatusLineModule::Agent,
                StatusLineModule::Model,
                StatusLineModule::Provider,
                StatusLineModule::Fast,
                StatusLineModule::Context,
                StatusLineModule::ProviderUsage,
                StatusLineModule::Cost,
                StatusLineModule::Hint,
            ]
        );
        assert_eq!(config.color(StatusLineModule::Mode), StatusLineColor::White);
        assert_eq!(
            config.color(StatusLineModule::ProviderUsage),
            StatusLineColor::Green
        );
        assert_eq!(
            config.color(StatusLineModule::Agent),
            StatusLineColor::White
        );
        assert_eq!(
            config.color(StatusLineModule::Cost),
            StatusLineColor::LightRed
        );
        assert_eq!(
            config.color(StatusLineModule::Tokens),
            StatusLineColor::Gray
        );
        assert_eq!(config.color(StatusLineModule::Cache), StatusLineColor::Blue);
        assert_eq!(config.color(StatusLineModule::Fast), StatusLineColor::Red);
    }

    #[test]
    fn fast_is_hidden_until_effective() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Fast],
            ..StatusLineConfig::default()
        };
        let mut values = values();
        assert!(prepare_status_line(&config, &values, 80).is_empty());
        values.fast = true;
        assert_eq!(prepare_status_line(&config, &values, 80)[0].text(), "fast");
    }

    #[test]
    fn provider_usage_formats_one_or_multiple_windows_and_hides_without_data() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::ProviderUsage],
            ..StatusLineConfig::default()
        };
        let mut values = values();
        assert!(prepare_status_line(&config, &values, 80).is_empty());

        values.provider_usage = Some(crate::ProviderUsageReport {
            windows: vec![crate::ProviderUsageWindow {
                id: "codex:weekly".into(),
                label: "weekly".into(),
                remaining_percent: 10,
            }],
        });
        assert_eq!(
            prepare_status_line(&config, &values, 80)[0].text(),
            "weekly 10% left"
        );

        values.provider_usage.as_mut().unwrap().windows.insert(
            0,
            crate::ProviderUsageWindow {
                id: "codex:5h".into(),
                label: "5h".into(),
                remaining_percent: 80,
            },
        );
        assert_eq!(
            prepare_status_line(&config, &values, 80)[0].text(),
            "5h 80% left, weekly 10% left"
        );
    }

    #[test]
    fn provider_usage_is_removed_early_on_narrow_terminals() {
        let config = StatusLineConfig {
            modules: vec![
                StatusLineModule::Mode,
                StatusLineModule::ProviderUsage,
                StatusLineModule::Model,
            ],
            ..StatusLineConfig::default()
        };
        let mut values = values();
        values.provider_usage = Some(crate::ProviderUsageReport {
            windows: vec![crate::ProviderUsageWindow {
                id: "codex:weekly".into(),
                label: "weekly".into(),
                remaining_percent: 10,
            }],
        });
        let segments = prepare_status_line(&config, &values, 24);
        assert!(
            !segments
                .iter()
                .any(|segment| segment.module == StatusLineModule::ProviderUsage)
        );
    }

    #[test]
    fn colors_parse_named_and_hex_values() {
        assert_eq!("LIGHT-BLUE".parse(), Ok(StatusLineColor::LightBlue));
        assert_eq!("#7aA2f7".parse(), Ok(StatusLineColor::Rgb(122, 162, 247)));
        assert_eq!(StatusLineColor::Rgb(122, 162, 247).to_string(), "#7AA2F7");
        assert!("#xyzxyz".parse::<StatusLineColor>().is_err());
    }

    #[test]
    fn formatting_preserves_configured_order_and_semantics() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Model, StatusLineModule::Mode],
            ..StatusLineConfig::default()
        };
        let segments = prepare_status_line(&config, &values(), 80);
        assert_eq!(
            segments
                .iter()
                .map(StatusLineSegment::text)
                .collect::<Vec<_>>(),
            ["gpt-example high", "ask"]
        );
        assert_eq!(segments[0].value, "gpt-example");
        assert_eq!(segments[0].effort.as_deref(), Some("high"));
    }

    #[test]
    fn mode_uses_the_active_mode_color() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Mode],
            colors: [(StatusLineModule::Mode, StatusLineColor::LightYellow)]
                .into_iter()
                .collect(),
        };
        let segments = prepare_status_line(&config, &values(), 80);

        assert_eq!(segments[0].color, StatusLineColor::LightYellow);
    }

    #[test]
    fn usage_modules_show_cost_tokens_cache_and_context_tokens() {
        let mut values = values();
        values.usage.add(&crate::ModelUsage {
            input_tokens: Some(40_000),
            non_cached_input_tokens: Some(10_000),
            cache_read_input_tokens: Some(30_000),
            cache_write_input_tokens: Some(5_000),
            output_tokens: Some(8_000),
            total_tokens: Some(48_000),
            cost: Some(crate::ModelCost {
                total_cost: Some("0.001001".into()),
                currency: "USD".into(),
                pricing_source: "provider_reported".into(),
                pricing_version: "response".into(),
                ..crate::ModelCost::default()
            }),
            ..crate::ModelUsage::default()
        });
        let config = StatusLineConfig {
            modules: vec![
                StatusLineModule::Cost,
                StatusLineModule::Tokens,
                StatusLineModule::Cache,
                StatusLineModule::CacheTokens,
                StatusLineModule::ContextTokens,
            ],
            ..StatusLineConfig::default()
        };
        let text = prepare_status_line(&config, &values, 120)
            .into_iter()
            .map(|segment| segment.text())
            .collect::<Vec<_>>();
        assert_eq!(
            text,
            [
                "$0.001",
                "in:10k out:8k",
                "85% cached",
                "cache r:30k/w:5k",
                "42k/128k"
            ]
        );
        let tokens = prepare_status_line(&config, &values, 120)
            .into_iter()
            .find(|segment| segment.module == StatusLineModule::Tokens)
            .expect("tokens segment");
        assert!(tokens.value_spans.is_empty());
    }

    #[test]
    fn token_and_cache_modules_are_independently_toggleable() {
        let mut values = values();
        values.usage.add(&crate::ModelUsage {
            input_tokens: Some(711_790),
            non_cached_input_tokens: Some(453_740),
            cache_read_input_tokens: Some(258_050),
            cache_write_input_tokens: Some(0),
            output_tokens: Some(996),
            ..crate::ModelUsage::default()
        });
        let tokens_config = StatusLineConfig {
            modules: vec![StatusLineModule::Tokens],
            ..StatusLineConfig::default()
        };
        assert_eq!(
            prepare_status_line(&tokens_config, &values, 80)[0].text(),
            "in:453.74k out:996"
        );

        let cache_tokens_config = StatusLineConfig {
            modules: vec![StatusLineModule::CacheTokens],
            ..StatusLineConfig::default()
        };
        assert_eq!(
            prepare_status_line(&cache_tokens_config, &values, 80)[0].text(),
            "cache r:258.05k/w:0"
        );

        let cache_config = StatusLineConfig {
            modules: vec![StatusLineModule::Cache],
            ..StatusLineConfig::default()
        };
        assert_eq!(
            prepare_status_line(&cache_config, &values, 80)[0].text(),
            "36% cached"
        );
    }

    #[test]
    fn token_rate_tracks_new_uncached_tokens_during_model_work() {
        let start = Instant::now();
        let mut tracker = TokenRateTracker::default();
        let mut usage = crate::SessionUsage::default();
        usage.input_tokens = Some(100);
        usage.cache_read_input_tokens = Some(10);
        usage.output_tokens = Some(20);
        tracker.observe_at(&usage, Some("openai/gpt-a"), true, start);
        assert_eq!(tracker.current(), TokenRate::default());

        usage.input_tokens = Some(150);
        usage.cache_read_input_tokens = Some(20);
        usage.output_tokens = Some(50);
        tracker.observe_at(
            &usage,
            Some("openai/gpt-a"),
            false,
            start + Duration::from_secs(10),
        );
        assert_eq!(
            tracker.current(),
            TokenRate {
                input: 4,
                output: 3
            }
        );

        // Time and unrelated snapshots do not change the displayed rate.
        tracker.observe_at(
            &usage,
            Some("openai/gpt-a"),
            false,
            start + Duration::from_secs(100),
        );
        assert_eq!(tracker.current().input, 4);
        tracker.observe_at(
            &usage,
            Some("openai/gpt-a"),
            true,
            start + Duration::from_secs(100),
        );
        tracker.observe_at(
            &usage,
            Some("openai/gpt-a"),
            true,
            start + Duration::from_secs(160),
        );
        assert_eq!(tracker.current().input, 4);

        usage.input_tokens = Some(210);
        tracker.observe_at(
            &usage,
            Some("openai/gpt-a"),
            false,
            start + Duration::from_secs(160),
        );
        assert_eq!(tracker.current().input, 1);
    }

    #[test]
    fn token_rate_accumulates_deltas_and_resets_on_regression_or_model_change() {
        let start = Instant::now();
        let mut tracker = TokenRateTracker::default();
        let usage = |input, output| {
            let mut usage = crate::SessionUsage::default();
            usage.non_cached_input_tokens = Some(input);
            usage.output_tokens = Some(output);
            usage
        };
        tracker.observe_at(&usage(100, 20), Some("model-a"), true, start);
        tracker.observe_at(
            &usage(130, 30),
            Some("model-a"),
            true,
            start + Duration::from_secs(5),
        );
        tracker.observe_at(
            &usage(150, 45),
            Some("model-a"),
            true,
            start + Duration::from_secs(10),
        );
        assert_eq!(
            tracker.current(),
            TokenRate {
                input: 5,
                output: 3
            }
        );

        tracker.observe_at(
            &usage(10, 2),
            Some("model-a"),
            true,
            start + Duration::from_secs(15),
        );
        assert_eq!(tracker.current(), TokenRate::default());
        tracker.observe_at(
            &usage(20, 4),
            Some("model-b"),
            true,
            start + Duration::from_secs(20),
        );
        assert_eq!(tracker.current(), TokenRate::default());
    }

    #[test]
    fn token_rate_formats_exactly_hides_zero_and_is_removed_early() {
        assert!(
            !StatusLineConfig::default()
                .modules
                .contains(&StatusLineModule::TokenRate)
        );
        assert_eq!(
            StatusLineModule::TokenRate.default_color(),
            StatusLineColor::Gray
        );
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Mode, StatusLineModule::TokenRate],
            ..StatusLineConfig::default()
        };
        let mut values = values();
        assert_eq!(prepare_status_line(&config, &values, 80).len(), 1);
        values.token_rate = TokenRate {
            input: 123,
            output: 4_232,
        };
        assert_eq!(
            prepare_status_line(&config, &values, 80)[1].text(),
            "in:123/s out:4232/s"
        );
        assert_eq!(
            prepare_status_line(&config, &values, 8)
                .iter()
                .map(|segment| segment.module)
                .collect::<Vec<_>>(),
            [StatusLineModule::Mode]
        );
    }

    #[test]
    fn currency_formats_support_short_and_long_amounts() {
        assert_eq!(format_currency("0", CurrencyFormat::Short), "0.000");
        assert_eq!(format_currency("0.001001", CurrencyFormat::Short), "0.001");
        assert_eq!(
            format_currency("0.001001", CurrencyFormat::Long),
            "0.001,001"
        );
        assert_eq!(format_currency("0.1", CurrencyFormat::Short), "0.100");
        assert_eq!(format_currency("0.023123", CurrencyFormat::Short), "0.023");
        assert_eq!(
            format_currency("0.023123", CurrencyFormat::Long),
            "0.023,123"
        );
        assert_eq!(
            format_currency("0.0006226", CurrencyFormat::Long),
            "0.000,622"
        );
        assert_eq!(format_currency("not-a-cost", CurrencyFormat::Short), "0.00");
        assert_eq!(format_currency("1.2", CurrencyFormat::Short), "1.20");
        assert_eq!(format_currency("1.234", CurrencyFormat::Short), "1.23");
        assert_eq!(format_currency("1.234", CurrencyFormat::Long), "1.23");
        assert_eq!(
            format_currency("1234567.8901234", CurrencyFormat::Short),
            "1,234,567.89"
        );
        assert_eq!(
            format_currency("1234567.8901234", CurrencyFormat::Long),
            "1,234,567.89"
        );
    }

    #[test]
    fn compact_token_format_uses_two_decimals_and_readable_units() {
        assert_eq!(format_compact_tokens(999), "999");
        assert_eq!(format_compact_tokens(1_000), "1k");
        assert_eq!(format_compact_tokens(10_000), "10k");
        assert_eq!(format_compact_tokens(1_234_000), "1.23M");
        assert_eq!(format_compact_tokens(1_200_000), "1.2M");
        assert_eq!(format_compact_tokens(999_999), "1M");
    }

    #[test]
    fn responsive_layout_keeps_cost_before_identity_and_context_details() {
        let mut values = values();
        values.usage.add(&crate::ModelUsage {
            input_tokens: Some(40_000),
            cache_read_input_tokens: Some(30_000),
            output_tokens: Some(8_000),
            total_tokens: Some(48_000),
            cost: Some(crate::ModelCost {
                total_cost: Some("0.23".into()),
                currency: "USD".into(),
                pricing_source: "provider_reported".into(),
                ..crate::ModelCost::default()
            }),
            ..crate::ModelUsage::default()
        });
        let config = StatusLineConfig {
            modules: StatusLineModule::ALL.to_vec(),
            ..StatusLineConfig::default()
        };
        let segments = prepare_status_line(&config, &values, 76);
        assert!(
            segments
                .iter()
                .any(|segment| segment.module == StatusLineModule::Cost)
        );
    }

    #[test]
    fn narrow_default_layout_keeps_cost_over_provider_agent_and_context() {
        let mut values = values();
        values.usage.add(&crate::ModelUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            total_tokens: Some(1_100),
            cost: Some(crate::ModelCost {
                total_cost: Some("0.01".into()),
                currency: "USD".into(),
                pricing_source: "models_dev".into(),
                pricing_version: "catalog".into(),
                ..crate::ModelCost::default()
            }),
            ..crate::ModelUsage::default()
        });

        let segments = prepare_status_line(&StatusLineConfig::default(), &values, 32);
        assert!(
            segments
                .iter()
                .any(|segment| segment.module == StatusLineModule::Cost)
        );
        assert!(
            !segments
                .iter()
                .any(|segment| segment.module == StatusLineModule::Provider)
        );
        assert!(
            !segments
                .iter()
                .any(|segment| segment.module == StatusLineModule::Agent)
        );
        assert!(
            !segments
                .iter()
                .any(|segment| segment.module == StatusLineModule::Context)
        );
    }

    #[test]
    fn default_agent_is_unavailable() {
        let mut values = values();
        values.agent = DEFAULT_AGENT_NAME.into();
        assert!(
            !prepare_status_line(&StatusLineConfig::default(), &values, 120)
                .iter()
                .any(|segment| segment.module == StatusLineModule::Agent)
        );
    }

    #[test]
    fn zero_work_modules_are_unavailable() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Mode],
            ..StatusLineConfig::default()
        };
        let mut values = values();
        values.subagents = 0;
        values.background = 0;

        assert_eq!(
            prepare_status_line(&config, &values, 80)
                .iter()
                .map(StatusLineSegment::text)
                .collect::<Vec<_>>(),
            ["ask"]
        );
    }

    #[test]
    fn hint_reflects_composer_conversation_and_background_state() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Hint],
            ..StatusLineConfig::default()
        };
        let mut values = values();
        values.background = 0;
        values.subagents = 0;

        assert!(prepare_status_line(&config, &values, 80).is_empty());

        values.composer_has_text = true;
        let segments = prepare_status_line(&config, &values, 80);
        assert_eq!(segments[0].text(), "Enter to send, Ctrl+C to clear");
        assert_eq!(segments[0].color, StatusLineColor::DarkGray);

        values.conversation_is_working = true;
        let segments = prepare_status_line(&config, &values, 80);
        assert_eq!(segments[0].text(), "Tab to queue, Enter to steer");

        values.composer_has_text = false;
        values.background = 1;
        let segments = prepare_status_line(&config, &values, 80);
        assert_eq!(segments[0].text(), "Alt+↓ 1 background");

        values.background = 0;
        values.subagents = 1;
        let segments = prepare_status_line(&config, &values, 80);
        assert_eq!(segments[0].text(), "Alt+↓ 1 background");

        values.background = 2;
        values.subagents = 3;
        let segments = prepare_status_line(&config, &values, 80);
        assert_eq!(segments[0].text(), "Alt+↓ 5 background");

        values.hints.push("Alt+↑ queued".into());
        let segments = prepare_status_line(&config, &values, 80);
        assert_eq!(segments[0].text(), "Alt+↑ queued · Alt+↓ 5 background");

        values.primary_hint = Some("Enter to save, Ctrl+C to unqueue".into());
        assert_eq!(
            prepare_status_line(&config, &values, 80)[0].text(),
            "Enter to save, Ctrl+C to unqueue"
        );
    }

    #[test]
    fn agent_is_rendered_without_a_label() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Agent],
            ..StatusLineConfig::default()
        };
        let segments = prepare_status_line(&config, &values(), 80);

        assert_eq!(segments[0].label, None);
        assert_eq!(segments[0].text(), "review");
    }

    #[test]
    fn responsive_priority_does_not_follow_configured_order() {
        let config = StatusLineConfig {
            modules: vec![
                StatusLineModule::Provider,
                StatusLineModule::Mode,
                StatusLineModule::Model,
            ],
            ..StatusLineConfig::default()
        };
        let segments = prepare_status_line(&config, &values(), 24);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.module)
                .collect::<Vec<_>>(),
            [StatusLineModule::Mode, StatusLineModule::Model]
        );
    }

    #[test]
    fn context_compacts_before_modules_disappear() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Mode, StatusLineModule::Context],
            ..StatusLineConfig::default()
        };
        let segments = prepare_status_line(&config, &values(), 18);
        assert_eq!(
            segments
                .iter()
                .map(StatusLineSegment::text)
                .collect::<Vec<_>>(),
            ["ask", "42% ctx used"]
        );

        let mut values = values();
        values.context_percent = None;
        assert_eq!(
            prepare_status_line(&config, &values, 80)
                .iter()
                .map(StatusLineSegment::text)
                .collect::<Vec<_>>(),
            ["ask"]
        );
    }

    #[test]
    fn context_uses_plain_confirmed_percentage_and_hides_unknown_values() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Context],
            ..StatusLineConfig::default()
        };
        assert_eq!(
            prepare_status_line(&config, &values(), 80)
                .into_iter()
                .map(|segment| segment.text())
                .collect::<Vec<_>>(),
            ["42% ctx used"]
        );

        let mut values = values();
        values.context_percent = None;
        assert!(prepare_status_line(&config, &values, 80).is_empty());
    }

    #[test]
    fn final_module_is_ellipsized_to_fit() {
        let config = StatusLineConfig {
            modules: vec![StatusLineModule::Model],
            ..StatusLineConfig::default()
        };
        let segments = prepare_status_line(&config, &values(), 6);
        assert_eq!(segments[0].text(), "… high");
    }
}
