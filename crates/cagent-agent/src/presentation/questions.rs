//! Frontend-neutral projection for completed native questions.

use crate::{QUESTION_NONE_OF_THE_ABOVE, QuestionAnswer, QuestionRequest, QuestionResult};
use serde::{Deserialize, Serialize};

/// A completed question ready for transcript rendering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuestionTranscriptEntry {
    pub header: String,
    pub question: String,
    pub answer: Option<String>,
    pub note: Option<String>,
}

/// A completed native question tool call ready for transcript rendering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuestionTranscript {
    pub answered: usize,
    pub total: usize,
    pub cancelled: bool,
    pub entries: Vec<QuestionTranscriptEntry>,
}

/// Parses a native question call and its normal structured result.
///
/// Returns `None` for unrelated tool calls or malformed historical data.
#[must_use]
pub fn project_question_transcript(
    name: &str,
    arguments: &serde_json::Value,
    result: Option<&serde_json::Value>,
) -> Option<QuestionTranscript> {
    if name != "request_user_input" {
        return None;
    }
    let request = serde_json::from_value::<QuestionRequest>(arguments.clone()).ok()?;
    let result = serde_json::from_value::<QuestionResult>(result?.clone()).ok()?;
    let entries = request
        .questions
        .into_iter()
        .map(|prompt| {
            let QuestionAnswer { selection, note } =
                result.answers.get(&prompt.id).cloned().unwrap_or_default();
            QuestionTranscriptEntry {
                header: prompt.header,
                question: prompt.question,
                answer: selection.or_else(|| {
                    result
                        .answers
                        .contains_key(&prompt.id)
                        .then(|| QUESTION_NONE_OF_THE_ABOVE.into())
                }),
                note,
            }
        })
        .collect::<Vec<_>>();
    let answered = entries
        .iter()
        .filter(|entry| entry.answer.is_some())
        .count();
    Some(QuestionTranscript {
        answered,
        total: entries.len(),
        cancelled: result.cancelled,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_structured_answer_and_synthetic_none() {
        let transcript = project_question_transcript(
            "request_user_input",
            &serde_json::json!({"questions": [{
                "id": "scope", "header": "Scope", "question": "Where?",
                "options": [{"label": "Root", "description": "Main"}, {"label": "Both", "description": "Everywhere"}]
            }]}),
            Some(&serde_json::json!({"answers": {"scope": {"selection": null, "note": "Use both."}}})),
        )
        .unwrap();
        assert_eq!(transcript.answered, 1);
        assert_eq!(
            transcript.entries[0].answer.as_deref(),
            Some(QUESTION_NONE_OF_THE_ABOVE)
        );
        assert_eq!(transcript.entries[0].note.as_deref(), Some("Use both."));
    }
}
