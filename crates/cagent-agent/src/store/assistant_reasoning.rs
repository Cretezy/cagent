//! Private replay state, deliberately separate from transcript hydration.

#[allow(clippy::wildcard_imports)]
use super::*;

fn validate(reasoning: &[ModelInput]) -> Result<(), RuntimeError> {
    if reasoning
        .iter()
        .all(|item| matches!(item, ModelInput::ProviderReasoning { .. }))
    {
        Ok(())
    } else {
        Err(RuntimeError::InvalidOption(
            "assistant reasoning may contain only provider reasoning items".into(),
        ))
    }
}

pub(super) fn save(
    transaction: &Transaction<'_>,
    node_id: NodeId,
    reasoning: &[ModelInput],
) -> Result<(), RuntimeError> {
    validate(reasoning)?;
    transaction.execute(
        "INSERT INTO assistant_reasoning(node_id, reasoning_json) VALUES (?1, ?2)",
        params![node_id.to_string(), serde_json::to_string(reasoning)?],
    )?;
    Ok(())
}

pub(super) fn load(
    connection: &Connection,
    node_id: NodeId,
) -> Result<Vec<ModelInput>, RuntimeError> {
    let encoded: Option<String> = connection
        .query_row(
            "SELECT reasoning_json FROM assistant_reasoning WHERE node_id = ?1",
            [node_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(encoded) = encoded else {
        return Ok(Vec::new());
    };
    // Decode errors may quote raw JSON values; do not expose opaque payloads.
    let reasoning = serde_json::from_str::<Vec<ModelInput>>(&encoded)
        .map_err(|_| RuntimeError::InvalidOption("invalid stored assistant reasoning".into()))?;
    validate(&reasoning)?;
    Ok(reasoning)
}
