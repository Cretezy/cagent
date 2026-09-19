//! Frontend-neutral provider and model picker projections.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{AuthFlow, ModelRef, ProviderAvailability, ReasoningControl, ResolvedModelCatalog};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderPickerRow {
    pub id: String,
    pub label: String,
    pub status: String,
    pub setup_instructions: Option<String>,
    pub configuration_instructions: String,
    pub credential_environment_variable: Option<String>,
    pub enabled: bool,
    pub managed_auth: bool,
    #[serde(default)]
    pub api_key_auth: bool,
    #[serde(default)]
    pub has_managed_api_key: bool,
    #[serde(default)]
    pub auth_flows: Vec<AuthFlow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelPickerRow {
    pub provider: String,
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub reasoning_control: Option<ReasoningControl>,
    pub badges: Vec<String>,
    pub current: bool,
    #[serde(default)]
    pub favourite: bool,
    #[serde(default)]
    pub release_date: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "effort", rename_all = "snake_case")]
pub enum EffortPickerFlow {
    NoneSupported,
    Automatic(String),
    Choose,
}

#[must_use]
pub fn project_provider_picker(
    providers: impl IntoIterator<Item = ProviderAvailability>,
) -> Vec<ProviderPickerRow> {
    providers
        .into_iter()
        .map(|provider| {
            let status = provider.status_text().to_owned();
            let setup_instructions = provider.setup_instructions();
            let configuration_instructions = provider.configuration_instructions();
            let credential_environment_variable =
                provider.descriptor.credential_environment_variable.clone();
            let auth_flows = provider.descriptor.auth_flows.clone();
            ProviderPickerRow {
                id: provider.descriptor.id,
                label: provider.descriptor.display_name,
                status,
                setup_instructions,
                configuration_instructions,
                credential_environment_variable,
                enabled: provider.enabled,
                managed_auth: provider.descriptor.credential_source
                    == crate::CredentialSource::Subscription,
                api_key_auth: provider.descriptor.supports_managed_api_key,
                has_managed_api_key: provider.has_managed_api_key,
                auth_flows,
            }
        })
        .collect()
}

#[must_use]
pub fn project_model_picker(
    provider: &str,
    current: Option<&ModelRef>,
    favourites: &BTreeSet<ModelRef>,
    resolved: ResolvedModelCatalog,
) -> Vec<ModelPickerRow> {
    let ResolvedModelCatalog {
        catalog, aliases, ..
    } = resolved;
    let mut rows = catalog
        .models
        .into_iter()
        .filter(|model| {
            model.capabilities.supports_tools == Some(true)
                && model.capabilities.supports_text_input == Some(true)
                && model.capabilities.supports_text_output == Some(true)
        })
        .map(|model| {
            let release_date =
                model_release_date(&model.raw_metadata, &model.id).map(str::to_owned);
            let efforts = model.capabilities.reasoning_efforts.unwrap_or_default();
            let reasoning_control = model.capabilities.reasoning_control;
            let mut badges = Vec::new();
            if let Some(context_window) = model.capabilities.context_window {
                badges.push(format_context_window(context_window));
            }
            if model.capabilities.supports_streaming == Some(false) {
                badges.push("no-stream".into());
            }
            let model_aliases = aliases
                .iter()
                .filter_map(|(alias, target)| (target == &model.id).then_some(alias.as_str()))
                .collect::<Vec<_>>();
            let display_name = model
                .raw_metadata
                .get("upstream_provider_name")
                .and_then(serde_json::Value::as_str)
                .filter(|publisher| !model.display_name.starts_with(&format!("{publisher}:")))
                .map_or(model.display_name.clone(), |publisher| {
                    format!("{publisher}: {}", model.display_name)
                });
            let label = if model_aliases.is_empty() {
                display_name
            } else {
                format!("{display_name} · alias:{}", model_aliases.join(","))
            };
            ModelPickerRow {
                current: current.is_some_and(|current| {
                    current.provider == provider && current.model == model.id
                }),
                favourite: favourites.contains(&ModelRef {
                    provider: provider.into(),
                    model: model.id.clone(),
                }),
                provider: provider.into(),
                id: model.id,
                label,
                efforts,
                reasoning_control,
                badges,
                release_date,
            }
        })
        .collect::<Vec<_>>();
    prioritize_current_model(&mut rows);
    rows
}

/// Keeps the current model at the top, then favourites, then orders by newest release.
pub fn prioritize_current_model(rows: &mut [ModelPickerRow]) {
    rows.sort_by(|left, right| {
        right
            .current
            .cmp(&left.current)
            .then_with(|| right.favourite.cmp(&left.favourite))
            .then_with(|| right.release_date.cmp(&left.release_date))
    });
}

fn model_release_date<'a>(metadata: &'a serde_json::Value, model_id: &str) -> Option<&'a str> {
    metadata
        .get("release_date")
        .or_else(|| metadata.get("models_dev")?.get("release_date"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            metadata
                .get("provider")
                .unwrap_or(metadata)
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(|version| {
                    version
                        .strip_prefix(&format!("{model_id}-"))
                        .unwrap_or(version)
                })
        })
}

#[must_use]
pub fn filter_provider_picker_rows<'a>(
    rows: &'a [ProviderPickerRow],
    query: &str,
) -> Vec<&'a ProviderPickerRow> {
    let query = query.to_lowercase();
    rows.iter()
        .filter(|row| {
            query.is_empty()
                || row.id.to_lowercase().contains(&query)
                || row.label.to_lowercase().contains(&query)
        })
        .collect()
}

#[must_use]
pub fn filter_model_picker_rows<'a>(
    rows: &'a [ModelPickerRow],
    query: &str,
) -> Vec<&'a ModelPickerRow> {
    let query = query.to_lowercase();
    rows.iter()
        .filter(|row| {
            query.is_empty()
                || row.provider.to_lowercase().contains(&query)
                || row.id.to_lowercase().contains(&query)
                || format!("{}/{}", row.provider, row.id)
                    .to_lowercase()
                    .contains(&query)
                || row.label.to_lowercase().contains(&query)
        })
        .collect()
}

/// Resolves a model picker argument using stable, user-visible precedence.
///
/// An exact `provider/model` reference wins, followed by an exact model ID
/// across providers. Otherwise the first row produced by the picker filter is
/// returned.
#[must_use]
pub fn resolve_model_picker_row<'a>(
    rows: &'a [ModelPickerRow],
    input: &str,
) -> Option<&'a ModelPickerRow> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    if let Some((provider, model)) = input.split_once('/')
        && let Some(row) = rows
            .iter()
            .find(|row| row.provider == provider && row.id == model)
    {
        return Some(row);
    }
    if let Some(row) = rows.iter().find(|row| row.id == input) {
        return Some(row);
    }
    filter_model_picker_rows(rows, input).into_iter().next()
}

/// Prepares the frontend-neutral text shown after a model selection changes.
#[must_use]
pub fn model_selection_message(provider: &str, model: &str) -> String {
    format!("Model changed to {provider}/{model}")
}

/// Prepares the frontend-neutral metadata shown beside a model picker label.
#[must_use]
pub fn model_picker_detail(row: &ModelPickerRow) -> String {
    let mut metadata = Vec::with_capacity(
        row.badges.len() + usize::from(row.favourite) + usize::from(row.current),
    );
    metadata.extend(row.badges.iter().cloned());
    if row.current {
        metadata.push(if row.favourite {
            "current ★".to_owned()
        } else {
            "current".to_owned()
        });
    } else if row.favourite {
        metadata.push("★".to_owned());
    }

    let identity = format!("{} ({})", row.provider, row.id);
    if metadata.is_empty() {
        identity
    } else {
        format!("{identity} · {}", metadata.join(" · "))
    }
}

/// Chooses an effort when Cagent must automatically fall back to another model.
#[must_use]
pub fn fallback_model_effort(efforts: &[String], preferred: Option<&str>) -> Option<String> {
    preferred
        .and_then(|preferred| efforts.iter().find(|effort| effort.as_str() == preferred))
        .or_else(|| efforts.first())
        .cloned()
}

#[must_use]
pub fn effort_picker_flow(efforts: &[String]) -> EffortPickerFlow {
    match efforts {
        [] => EffortPickerFlow::NoneSupported,
        [effort] => EffortPickerFlow::Automatic(effort.clone()),
        _ => EffortPickerFlow::Choose,
    }
}

fn format_context_window(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("ctx:{}m", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("ctx:{}k", tokens / 1_000)
    } else {
        format!("ctx:{tokens}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use super::*;
    use crate::{
        AuthState, CredentialSource, ModelCapabilities, ModelCatalog, ModelCatalogSource,
        ModelDescriptor, ProviderDescriptor,
    };

    #[test]
    fn provider_projection_and_filtering_are_frontend_neutral() {
        let rows = project_provider_picker([ProviderAvailability {
            descriptor: ProviderDescriptor {
                id: "openai".into(),
                display_name: "OpenAI".into(),
                default_model_backend: Some(crate::ModelBackend::OpenAiResponses),
                supported_model_backends: vec![crate::ModelBackend::OpenAiResponses],
                model_discovery: crate::ModelDiscoverySource::ModelsDev,
                credential_source: CredentialSource::Environment,
                credential_environment_variable: Some("OPENAI_API_KEY".into()),
                supports_managed_api_key: false,
                auth_flows: Vec::new(),
            },
            enabled: false,
            auth: AuthState::Available {
                detail: "key found".into(),
            },
            has_managed_api_key: false,
        }]);

        assert_eq!(rows[0].status, "disabled");
        assert_eq!(rows[0].setup_instructions, None);
        assert_eq!(
            rows[0].configuration_instructions,
            "Set OPENAI_API_KEY in the environment, then restart Cagent."
        );
        assert_eq!(
            rows[0].credential_environment_variable.as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(filter_provider_picker_rows(&rows, "OPEN"), [&rows[0]]);
        assert!(filter_provider_picker_rows(&rows, "missing").is_empty());
    }

    #[test]
    fn model_projection_resolves_aliases_capabilities_and_current_selection() {
        let current = ModelRef::parse("openai/gpt-next").unwrap();
        let rows = project_model_picker(
            "openai",
            Some(&current),
            &BTreeSet::new(),
            ResolvedModelCatalog {
                catalog: ModelCatalog {
                    provider: "openai".into(),
                    models: vec![
                        ModelDescriptor {
                            id: "gpt-next".into(),
                            display_name: "GPT Next".into(),
                            capabilities: ModelCapabilities {
                                supports_fast_mode: None,
                                supports_streaming: Some(true),
                                supports_tools: Some(true),
                                supports_structured_output: None,
                                supports_text_input: Some(true),
                                supports_image_input: None,
                                supports_text_output: Some(true),
                                reasoning_control: Some(ReasoningControl::Effort),
                                reasoning_efforts: Some(vec!["low".into(), "high".into()]),
                                context_window: Some(128_000),
                            },
                            backend: Some(crate::ModelBackend::OpenAiResponses),
                            raw_metadata: Value::Null,
                        },
                        ModelDescriptor {
                            id: "text-only".into(),
                            display_name: "Text Only".into(),
                            capabilities: ModelCapabilities {
                                supports_tools: Some(false),
                                ..ModelCapabilities::default()
                            },
                            backend: Some(crate::ModelBackend::OpenAiResponses),
                            raw_metadata: Value::Null,
                        },
                        ModelDescriptor {
                            id: "unknown-capabilities".into(),
                            display_name: "Unknown Capabilities".into(),
                            capabilities: ModelCapabilities::default(),
                            backend: None,
                            raw_metadata: Value::Null,
                        },
                        ModelDescriptor {
                            id: "audio-output".into(),
                            display_name: "Audio Output".into(),
                            capabilities: ModelCapabilities {
                                supports_tools: Some(true),
                                supports_text_input: Some(true),
                                supports_text_output: Some(false),
                                ..ModelCapabilities::default()
                            },
                            backend: Some(crate::ModelBackend::OpenAiResponses),
                            raw_metadata: Value::Null,
                        },
                    ],
                    version: None,
                },
                aliases: BTreeMap::from([("fast".into(), "gpt-next".into())]),
                source: ModelCatalogSource::Remote,
                age_millis: None,
                refresh_error: None,
            },
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "GPT Next · alias:fast");
        assert_eq!(rows[0].badges, ["ctx:128k"]);
        assert!(rows[0].current);
        assert_eq!(filter_model_picker_rows(&rows, "FAST"), [&rows[0]]);
        assert_eq!(
            effort_picker_flow(&rows[0].efforts),
            EffortPickerFlow::Choose
        );
    }

    #[test]
    fn prioritizes_current_model_and_preserves_other_rows() {
        let mut rows = vec![
            model_row("openai", "first"),
            model_row("mock", "current"),
            model_row("openrouter", "last"),
        ];
        rows[1].current = true;

        prioritize_current_model(&mut rows);

        assert_eq!(
            rows.iter()
                .map(|row| (row.provider.as_str(), row.id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("mock", "current"),
                ("openai", "first"),
                ("openrouter", "last"),
            ]
        );
    }

    #[test]
    fn sorts_non_current_models_by_release_date_newest_first() {
        let mut rows = vec![
            model_row_with_release("openai", "older", "2024-05-13"),
            model_row_with_release("openai", "newest", "2026-08-01"),
            model_row("openai", "undated"),
            model_row_with_release("openai", "current", "2023-01-01"),
        ];
        rows[3].current = true;

        prioritize_current_model(&mut rows);

        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["current", "newest", "older", "undated"]
        );
    }

    #[test]
    fn places_favourites_below_current_and_above_other_filtered_rows() {
        let mut rows = vec![
            model_row_with_release("openai", "newest", "2026-08-01"),
            model_row("mock", "current"),
            model_row_with_release("openai", "favourite", "2024-01-01"),
        ];
        rows[1].current = true;
        rows[2].favourite = true;

        prioritize_current_model(&mut rows);

        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["current", "favourite", "newest"]
        );
        assert_eq!(
            filter_model_picker_rows(&rows, "openai")
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["favourite", "newest"]
        );
    }

    #[test]
    fn reads_release_dates_from_models_dev_and_copilot_versions() {
        assert_eq!(
            model_release_date(
                &serde_json::json!({ "models_dev": { "release_date": "2026-07-01" } }),
                "gpt-test"
            ),
            Some("2026-07-01")
        );
        assert_eq!(
            model_release_date(
                &serde_json::json!({
                    "provider": { "version": "gpt-test-2026-08-02" }
                }),
                "gpt-test"
            ),
            Some("2026-08-02")
        );
    }

    #[test]
    fn effort_flow_handles_zero_one_and_many_options() {
        assert_eq!(effort_picker_flow(&[]), EffortPickerFlow::NoneSupported);
        assert_eq!(
            effort_picker_flow(&["high".into()]),
            EffortPickerFlow::Automatic("high".into())
        );
        assert_eq!(
            effort_picker_flow(&["low".into(), "high".into()]),
            EffortPickerFlow::Choose
        );
        assert_eq!(
            fallback_model_effort(&["low".into(), "high".into()], Some("high")),
            Some("high".into())
        );
        assert_eq!(
            fallback_model_effort(&["low".into(), "high".into()], Some("unsupported")),
            Some("low".into())
        );
        assert_eq!(fallback_model_effort(&[], Some("high")), None);
    }

    #[test]
    fn model_argument_resolution_prefers_exact_references_then_filtering() {
        let rows = [
            model_row("anthropic", "gpt-5.6-luna-preview"),
            model_row("openai", "gpt-5.6-luna"),
            model_row("openrouter", "openai/gpt-5.6-luna"),
        ];

        assert_eq!(
            resolve_model_picker_row(&rows, "openai/gpt-5.6-luna")
                .map(|row| (row.provider.as_str(), row.id.as_str())),
            Some(("openai", "gpt-5.6-luna"))
        );
        assert_eq!(
            resolve_model_picker_row(&rows, "gpt-5.6-luna")
                .map(|row| (row.provider.as_str(), row.id.as_str())),
            Some(("openai", "gpt-5.6-luna"))
        );
        assert_eq!(
            resolve_model_picker_row(&rows, "luna")
                .map(|row| (row.provider.as_str(), row.id.as_str())),
            Some(("anthropic", "gpt-5.6-luna-preview"))
        );
        assert_eq!(
            resolve_model_picker_row(&rows, "openrouter/openai/gpt-5.6-luna")
                .map(|row| (row.provider.as_str(), row.id.as_str())),
            Some(("openrouter", "openai/gpt-5.6-luna"))
        );
        assert!(resolve_model_picker_row(&rows, "missing").is_none());
        assert_eq!(
            model_selection_message("openai", "gpt-5.6-luna"),
            "Model changed to openai/gpt-5.6-luna"
        );
    }

    #[test]
    fn formats_model_picker_backend_and_id_separately() {
        let mut row = model_row("openrouter", "openai/gpt-5.6-luna");
        row.label = "OpenAI: GPT-5.6 Luna".into();
        row.badges = vec!["ctx:1m".into()];
        row.current = true;
        row.favourite = true;

        assert_eq!(
            model_picker_detail(&row),
            "openrouter (openai/gpt-5.6-luna) · ctx:1m · current ★"
        );
    }

    fn model_row(provider: &str, id: &str) -> ModelPickerRow {
        ModelPickerRow {
            provider: provider.into(),
            id: id.into(),
            label: id.into(),
            efforts: Vec::new(),
            reasoning_control: None,
            badges: Vec::new(),
            current: false,
            favourite: false,
            release_date: None,
        }
    }

    fn model_row_with_release(provider: &str, id: &str, release_date: &str) -> ModelPickerRow {
        ModelPickerRow {
            release_date: Some(release_date.into()),
            ..model_row(provider, id)
        }
    }
}
