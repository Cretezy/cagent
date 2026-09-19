//! Exact decimal cost calculations for provider-reported model usage.

#[derive(Clone)]
pub(super) struct PricingSnapshot {
    pub(super) prompt: Option<String>,
    pub(super) cache_read: Option<String>,
    pub(super) cache_write: Option<String>,
    pub(super) completion: Option<String>,
    pub(super) reasoning: Option<String>,
    pub(super) source: String,
    pub(super) version: String,
}

impl PricingSnapshot {
    pub(super) fn from_model(
        provider: &str,
        model: &crate::ModelDescriptor,
        version: Option<&str>,
        fast: bool,
    ) -> Option<Self> {
        let provider_pricing = model
            .raw_metadata
            .get("pricing")
            .or_else(|| model.raw_metadata.pointer("/provider/pricing"));
        let fast_cost = fast
            .then(|| {
                model
                    .raw_metadata
                    .pointer("/models_dev/experimental/modes/fast/cost")
                    .or_else(|| {
                        model
                            .raw_metadata
                            .pointer("/models_dev/models_dev/experimental/modes/fast/cost")
                    })
            })
            .flatten();
        let models_dev_cost = model
            .raw_metadata
            .pointer("/models_dev/cost")
            .or_else(|| model.raw_metadata.pointer("/models_dev/models_dev/cost"));
        let (pricing, models_dev) = match (fast_cost, provider_pricing, models_dev_cost) {
            (Some(cost), _, _) => (cost, true),
            (None, Some(pricing), _) => (pricing, false),
            (None, None, Some(cost)) => (cost, true),
            (None, None, None) => return None,
        };
        let field = |names: &[&str]| {
            names.iter().find_map(|name| {
                let value = pricing.get(*name)?;
                let value = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_number().map(ToString::to_string))?;
                if models_dev {
                    per_million_to_token_price(&value)
                } else {
                    Some(value)
                }
            })
        };
        let version = version.map_or_else(
            || {
                use sha2::{Digest as _, Sha256};
                format!(
                    "sha256:{:x}",
                    Sha256::digest(pricing.to_string().as_bytes())
                )
            },
            str::to_owned,
        );
        Some(Self {
            prompt: field(if models_dev { &["input"] } else { &["prompt"] }),
            cache_read: field(if models_dev {
                &["cache_read", "input_cache_read"]
            } else {
                &["input_cache_read", "cache_read"]
            }),
            cache_write: field(if models_dev {
                &["cache_write", "input_cache_write"]
            } else {
                &["input_cache_write", "cache_write"]
            }),
            completion: field(if models_dev {
                &["output", "completion"]
            } else {
                &["completion", "output"]
            }),
            reasoning: field(&["internal_reasoning", "reasoning"]),
            source: if models_dev {
                "models_dev".into()
            } else if provider == "openrouter" {
                "openrouter_models_api".into()
            } else {
                "provider_catalog".into()
            },
            version,
        })
    }
}

fn per_million_to_token_price(value: &str) -> Option<String> {
    parse_decimal(value)
        .and_then(|value| value.checked_div(1_000_000))
        .map(format_decimal)
}

pub(super) fn estimate_model_cost(
    usage: &crate::ModelUsage,
    pricing: Option<&PricingSnapshot>,
) -> Option<crate::ModelCost> {
    let pricing = pricing?;
    let multiply = |tokens: Option<u64>, price: Option<&String>| {
        tokens
            .zip(price)
            .and_then(|(tokens, price)| decimal_product(price, tokens))
    };
    let non_cached = usage.non_cached_input_tokens.or(usage.input_tokens);
    // Cache writes are a subset of non-cached input. Charge those tokens at
    // the write rate and only the remainder at the ordinary prompt rate.
    let ordinary_input = non_cached
        .map(|tokens| tokens.saturating_sub(usage.cache_write_input_tokens.unwrap_or_default()));
    let input_cost = multiply(ordinary_input, pricing.prompt.as_ref());
    let cache_read_cost = multiply(
        usage.cache_read_input_tokens,
        pricing.cache_read.as_ref().or(pricing.prompt.as_ref()),
    );
    let cache_write_cost = multiply(
        usage.cache_write_input_tokens,
        pricing.cache_write.as_ref().or(pricing.prompt.as_ref()),
    );
    let output_cost = multiply(usage.output_tokens, pricing.completion.as_ref());
    let reasoning_cost = multiply(
        usage.reasoning_tokens,
        pricing.reasoning.as_ref().or(pricing.completion.as_ref()),
    );
    let total_cost = decimal_sum(
        [
            input_cost.as_deref(),
            cache_read_cost.as_deref(),
            cache_write_cost.as_deref(),
            output_cost.as_deref(),
            reasoning_cost.as_deref(),
        ]
        .into_iter()
        .flatten(),
    );
    total_cost.as_ref()?;
    Some(crate::ModelCost {
        input_cost,
        cache_read_cost,
        cache_write_cost,
        output_cost,
        reasoning_cost,
        total_cost,
        currency: "USD".into(),
        pricing_source: pricing.source.clone(),
        pricing_version: pricing.version.clone(),
    })
}

const DECIMAL_SCALE: u32 = 18;

pub(super) fn parse_decimal(value: &str) -> Option<i128> {
    let (whole, fraction) = value.trim().split_once('.').unwrap_or((value.trim(), ""));
    if fraction.len() > DECIMAL_SCALE as usize || whole.starts_with('-') {
        return None;
    }
    let whole = whole.parse::<i128>().ok()?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<i128>().ok()?
    };
    let scale = 10_i128.checked_pow(DECIMAL_SCALE)?;
    let fraction_scale =
        10_i128.checked_pow(DECIMAL_SCALE.checked_sub(u32::try_from(fraction.len()).ok()?)?)?;
    whole
        .checked_mul(scale)?
        .checked_add(fraction_value.checked_mul(fraction_scale)?)
}

fn format_decimal(value: i128) -> String {
    let scale = 10_i128.pow(DECIMAL_SCALE);
    let whole = value / scale;
    let fraction = value % scale;
    if fraction == 0 {
        return whole.to_string();
    }
    let mut result = format!("{whole}.{fraction:018}");
    while result.ends_with('0') {
        result.pop();
    }
    result
}

fn decimal_product(price: &str, tokens: u64) -> Option<String> {
    parse_decimal(price)?
        .checked_mul(i128::from(tokens))
        .map(format_decimal)
}

pub(crate) fn decimal_sum<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut present = false;
    let mut total = 0_i128;
    for value in values {
        total = total.checked_add(parse_decimal(value)?)?;
        present = true;
    }
    present.then(|| format_decimal(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_dev_cost_is_normalized_from_per_million_tokens() {
        let model = crate::ModelDescriptor {
            id: "example".into(),
            display_name: "Example".into(),
            capabilities: crate::ModelCapabilities::default(),
            backend: None,
            raw_metadata: serde_json::json!({
                "models_dev": {
                    "cost": {
                        "input": 2.5,
                        "output": 15,
                        "cache_read": 0.25,
                        "cache_write": 3
                    }
                }
            }),
        };
        let pricing = PricingSnapshot::from_model("openai", &model, None, false).unwrap();
        assert_eq!(pricing.prompt.as_deref(), Some("0.0000025"));
        assert_eq!(pricing.completion.as_deref(), Some("0.000015"));
        assert_eq!(pricing.cache_read.as_deref(), Some("0.00000025"));
        assert_eq!(pricing.cache_write.as_deref(), Some("0.000003"));
        assert_eq!(pricing.source, "models_dev");
    }

    #[test]
    fn fast_mode_cost_overrides_base_and_provider_pricing() {
        let model = crate::ModelDescriptor {
            id: "example".into(),
            display_name: "Example".into(),
            capabilities: crate::ModelCapabilities::default(),
            backend: None,
            raw_metadata: serde_json::json!({
                "pricing": {"prompt": "0.000001"},
                "models_dev": {
                    "cost": {"input": 2},
                    "experimental": {"modes": {"fast": {"cost": {"input": 8}}}}
                }
            }),
        };
        let ordinary = PricingSnapshot::from_model("openai", &model, None, false).unwrap();
        let fast = PricingSnapshot::from_model("openai", &model, None, true).unwrap();
        assert_eq!(ordinary.prompt.as_deref(), Some("0.000001"));
        assert_eq!(fast.prompt.as_deref(), Some("0.000008"));
        assert_eq!(fast.source, "models_dev");
    }

    #[test]
    fn cache_writes_are_not_also_priced_as_ordinary_input() {
        let pricing = PricingSnapshot {
            prompt: Some("1".into()),
            cache_read: Some("0.1".into()),
            cache_write: Some("1.25".into()),
            completion: Some("2".into()),
            reasoning: None,
            source: "test".into(),
            version: "v1".into(),
        };
        let usage = crate::ModelUsage {
            input_tokens: Some(100),
            non_cached_input_tokens: Some(80),
            cache_read_input_tokens: Some(20),
            cache_write_input_tokens: Some(30),
            output_tokens: Some(5),
            ..crate::ModelUsage::default()
        };

        let cost = estimate_model_cost(&usage, Some(&pricing)).unwrap();

        assert_eq!(cost.input_cost.as_deref(), Some("50"));
        assert_eq!(cost.cache_read_cost.as_deref(), Some("2"));
        assert_eq!(cost.cache_write_cost.as_deref(), Some("37.5"));
        assert_eq!(cost.output_cost.as_deref(), Some("10"));
        assert_eq!(cost.total_cost.as_deref(), Some("99.5"));
    }
}
