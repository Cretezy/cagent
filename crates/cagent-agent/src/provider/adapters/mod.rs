//! Concrete provider adapters.

pub(crate) mod anthropic;
mod chatgpt;
pub(crate) mod gemini;
mod github_copilot;
pub(crate) mod openai;
pub(crate) mod openai_compatible;
pub(crate) mod opencode;

pub use anthropic::AnthropicProvider;
pub use chatgpt::ChatGptProvider;
pub use gemini::GeminiProvider;
pub use github_copilot::GitHubCopilotProvider;
pub use openai::OpenAiProvider;
pub use openai_compatible::OpenAiCompatibleProvider;
pub use opencode::OpenCodeProvider;
