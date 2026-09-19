//! Native agent-control requests and results.

use serde::{Deserialize, Serialize};

/// The label used for the synthetic answer offered by interactive frontends.
pub const QUESTION_NONE_OF_THE_ABOVE: &str = "None of the above";

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionPrompt {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionRequest {
    pub questions: Vec<QuestionPrompt>,
}

impl QuestionRequest {
    /// Validates the semantic constraints that JSON Schema cannot express.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=3).contains(&self.questions.len()) {
            return Err("request_user_input requires between 1 and 3 questions".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for prompt in &self.questions {
            if prompt.id.trim().is_empty()
                || prompt.header.trim().is_empty()
                || prompt.question.trim().is_empty()
            {
                return Err(
                    "request_user_input ids, headers, and prompts must be non-empty".into(),
                );
            }
            if !ids.insert(&prompt.id) {
                return Err(format!(
                    "request_user_input ids must be unique: {}",
                    prompt.id
                ));
            }
            if !(2..=3).contains(&prompt.options.len()) {
                return Err(format!(
                    "request_user_input {} must provide between 2 and 3 options",
                    prompt.id
                ));
            }
            if prompt.options.iter().any(|option| {
                option.label.trim().is_empty() || option.description.trim().is_empty()
            }) {
                return Err(format!(
                    "request_user_input {} options must have non-empty labels and descriptions",
                    prompt.id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionResult {
    pub answers: std::collections::BTreeMap<String, QuestionAnswer>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancelled: bool,
}
