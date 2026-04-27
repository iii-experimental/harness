//! Model capabilities knowledge base.
//!
//! Embeds a baseline `models.json` at compile time. Functions answer
//! capability queries used by router workers, harness UI selectors, and
//! provider workers needing model-shape facts.

use harness_types::{CacheRetention, ThinkingBudgets, ThinkingLevel, Transport};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

const EMBEDDED_MODELS: &str = include_str!("../data/models.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub provider: String,
    pub api: String,
    pub display_name: String,
    pub context_window: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub supports_xhigh: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_cache: bool,
    #[serde(default)]
    pub thinking_budgets: Option<ThinkingBudgets>,
    #[serde(default)]
    pub transports: Vec<Transport>,
    #[serde(default)]
    pub default_cache_retention: Option<CacheRetention>,
    #[serde(default)]
    pub pricing: Option<Pricing>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_1m: f64,
    pub output_per_1m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_1m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_1m: Option<f64>,
}

/// Capability query passed to [`supports`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Thinking,
    ThinkingLevel(ThinkingLevel),
    Tools,
    Vision,
    Cache,
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    models: Vec<Model>,
}

static CATALOG: Lazy<Vec<Model>> = Lazy::new(|| {
    let parsed: CatalogFile =
        serde_json::from_str(EMBEDDED_MODELS).expect("embedded models.json parses");
    parsed.models
});

/// Filter for [`list`].
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub provider: Option<String>,
    pub capability: Option<Capability>,
}

/// Return all models matching the filter.
pub fn list(filter: &ListFilter) -> Vec<Model> {
    CATALOG
        .iter()
        .filter(|m| filter.provider.as_ref().is_none_or(|p| p == &m.provider))
        .filter(|m| filter.capability.map_or(true, |c| supports_model(m, c)))
        .cloned()
        .collect()
}

/// Look up a single model by `(provider, model_id)`.
pub fn get(provider: &str, model_id: &str) -> Option<Model> {
    CATALOG
        .iter()
        .find(|m| m.provider == provider && m.id == model_id)
        .cloned()
}

/// True when the model supports the requested capability.
pub fn supports(provider: &str, model_id: &str, capability: Capability) -> bool {
    get(provider, model_id).is_some_and(|m| supports_model(&m, capability))
}

fn supports_model(m: &Model, capability: Capability) -> bool {
    match capability {
        Capability::Thinking => m.supports_thinking,
        Capability::ThinkingLevel(ThinkingLevel::Xhigh) => m.supports_xhigh,
        Capability::ThinkingLevel(_) => m.supports_thinking,
        Capability::Tools => m.supports_tools,
        Capability::Vision => m.supports_vision,
        Capability::Cache => m.supports_cache,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_loads() {
        assert!(!CATALOG.is_empty());
    }

    #[test]
    fn list_unfiltered_returns_all() {
        let all = list(&ListFilter::default());
        assert_eq!(all.len(), CATALOG.len());
    }

    #[test]
    fn list_by_provider() {
        let anthropic = list(&ListFilter {
            provider: Some("anthropic".into()),
            capability: None,
        });
        assert!(!anthropic.is_empty());
        assert!(anthropic.iter().all(|m| m.provider == "anthropic"));
    }

    #[test]
    fn list_by_capability_xhigh() {
        let xhigh = list(&ListFilter {
            provider: None,
            capability: Some(Capability::ThinkingLevel(ThinkingLevel::Xhigh)),
        });
        assert!(xhigh.iter().all(|m| m.supports_xhigh));
    }

    #[test]
    fn get_known_model() {
        let m = get("anthropic", "claude-opus-4-7").expect("known model");
        assert_eq!(m.context_window, 1_000_000);
        assert!(m.supports_xhigh);
    }

    #[test]
    fn get_unknown_returns_none() {
        assert!(get("anthropic", "does-not-exist").is_none());
    }

    #[test]
    fn supports_xhigh_is_subset_of_thinking() {
        for m in CATALOG.iter() {
            if m.supports_xhigh {
                assert!(
                    m.supports_thinking,
                    "model {} has xhigh but not thinking",
                    m.id
                );
            }
        }
    }

    #[test]
    fn supports_returns_true_for_known_capability() {
        assert!(supports(
            "anthropic",
            "claude-opus-4-7",
            Capability::ThinkingLevel(ThinkingLevel::Xhigh)
        ));
        assert!(supports("openai", "gpt-5", Capability::Tools));
    }

    #[test]
    fn supports_returns_false_for_unsupported() {
        assert!(!supports(
            "anthropic",
            "claude-haiku-4-5",
            Capability::Thinking
        ));
    }
}
