//! Model-visible context reduction. Canonical conversation and settled tools
//! remain immutable; only a committed selection may replace a prompt prefix.
use super::context_inspection::ContextDocument;
use aworkit_capability_host::{
    ModelAssistantContentV1, ModelToolContextV1, ModelToolExchangeV1, ModelToolRequestV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod surface;
mod target;
pub(crate) use surface::*;
pub(crate) use target::freeze_summary_target;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Metadata {
    /// Frozen model capability shared with image acquisition tools.
    #[serde(default)]
    pub image_input: bool,
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_target: Option<FrozenSummaryTarget>,
    #[serde(default)]
    pub policy: Policy,
}

/// Resolved once at Chat freeze. Only opaque credential metadata is persisted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenSummaryTarget {
    pub provider: super::settings_v2::ProviderConfigurationV2,
    pub model: super::settings_v2::ModelConfigurationV2,
    #[serde(rename = "opaqueBinding")]
    pub credential: Option<super::history::FrozenCredentialBindingV1>,
}
pub(crate) const SUMMARY_BINDING: &str = "model.context-summary";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub node_id: String,
    pub child: Option<String>,
    pub outer: String,
    pub through: usize,
    pub conversation_cursor: usize,
    #[serde(default)]
    pub conversation_sequence: Option<u64>,
    pub document: ContextDocument,
    pub anchor: Option<Anchor>,
}

#[derive(Default)]
pub(crate) struct Preparation {
    pub error: Option<String>,
    /// A provider failure inside the auxiliary compaction request. It is
    /// reported to the model on the same frozen route: a rejected summary
    /// request says nothing certain about the acting request, so it never ends
    /// the Agent node or the Run.
    pub provider_error: Option<aworkit_capability_host::ProviderError>,
    pub durable: bool,
    pub changed: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub max_overflow_retries: u32,
}

/// A node's overlay on the frozen Chat compaction policy.
///
/// Only the knobs a single Agent node can answer for are overridable. The
/// summarization route, retention budget and retry counts stay Chat-wide, so one
/// Chat never carries two policies for one mechanism. An absent key inherits the
/// frozen policy, and a node with no overlay at all keeps the Chat behaviour.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Overlay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_tool_results: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_chars: Option<usize>,
}

impl Overlay {
    /// Parses one node's overlay, ignoring an absent or malformed value.
    ///
    /// The executable catalog already rejected a malformed shape when the Chat
    /// froze, so a value that cannot be parsed here cannot be trusted to change
    /// context behaviour and is treated as "inherit".
    #[must_use]
    pub(crate) fn from_node_configuration(configuration: &Value) -> Option<Self> {
        let value = configuration.get("compaction")?.clone();
        serde_json::from_value(value).ok()
    }

    /// Applies this overlay to the frozen policy for one node's context.
    pub(crate) fn apply(&self, policy: &mut Policy) {
        if let Some(auto) = self.auto {
            policy.auto = auto;
        }
        if let Some(prune) = self.prune_tool_results {
            policy.prune_tool_results = prune;
        }
        if let Some(threshold) = self.threshold_chars {
            policy.threshold_chars = threshold;
        }
        if let Some(head) = self.head_chars {
            policy.head_chars = head;
        }
        if let Some(tail) = self.tail_chars {
            policy.tail_chars = tail;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Policy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<aworkit_capability_host::context_compression::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarization_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarization_model: Option<String>,
    #[serde(default = "yes")]
    pub auto: bool,
    #[serde(default = "threshold")]
    pub threshold_ratio: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_tokens: Option<u64>,
    #[serde(default = "summary_tokens")]
    pub max_tokens: u64,
    #[serde(default = "one")]
    pub compaction_retries: u32,
    #[serde(default = "one")]
    pub max_overflow_retries: u32,
    #[serde(default = "yes")]
    pub prune_tool_results: bool,
    #[serde(default = "prune_threshold")]
    pub threshold_chars: usize,
    #[serde(default = "prune_head")]
    pub head_chars: usize,
    #[serde(default = "prune_tail")]
    pub tail_chars: usize,
}
fn yes() -> bool {
    true
}
fn threshold() -> f64 {
    0.8
}
fn summary_tokens() -> u64 {
    8192
}
fn one() -> u32 {
    1
}
fn prune_threshold() -> usize {
    8192
}
fn prune_head() -> usize {
    4096
}
fn prune_tail() -> usize {
    1024
}
impl Default for Policy {
    fn default() -> Self {
        serde_json::from_value(json!({})).expect("compaction defaults")
    }
}
impl Policy {
    pub(crate) fn validate(&self, capacity: Option<u64>) -> Result<(), String> {
        if let Some(policy) = &self.compression {
            policy.validate()?;
        }
        if self.summarization_provider.is_some() != self.summarization_model.is_some()
            || self
                .summarization_provider
                .as_deref()
                .is_some_and(str::is_empty)
                != self
                    .summarization_model
                    .as_deref()
                    .is_some_and(str::is_empty)
        {
            return Err(
                "Summarization provider and model must be omitted, cleared or configured together."
                    .into(),
            );
        }
        let ratio = self.retain_ratio.unwrap_or(0.16);
        if !self.threshold_ratio.is_finite()
            || !(0.0 < self.threshold_ratio && self.threshold_ratio <= 1.0)
            || !ratio.is_finite()
            || !(0.0 < ratio && ratio <= 1.0)
            || (self.retain_ratio.is_some() && self.retain_tokens.is_some())
            || (self.retain_tokens.is_none() && ratio >= self.threshold_ratio)
            || self.max_tokens == 0
            || self.max_tokens > 1_048_576
            || self.compaction_retries > 32
            || self.max_overflow_retries > 32
            || self.threshold_chars == 0
            || self.threshold_chars > 4 * 1024 * 1024
            || self
                .head_chars
                .saturating_add(self.tail_chars)
                .saturating_add(PRUNE_MARKER.chars().count())
                > self.threshold_chars
        {
            return Err("Invalid compaction policy: require a positive threshold at most 1, smaller exclusive tail ratio/token budget, positive summary cap and valid pruning budgets.".into());
        }
        if let Some(capacity) = capacity {
            if capacity == 0 || self.retention(capacity) >= self.threshold(capacity) {
                return Err(
                    "Compaction retention must be smaller than the model pressure threshold."
                        .into(),
                );
            }
        }
        Ok(())
    }
    pub(crate) fn threshold(&self, capacity: u64) -> u64 {
        (capacity as f64 * self.threshold_ratio).floor() as u64
    }
    pub(crate) fn retention(&self, capacity: u64) -> u64 {
        self.retain_tokens
            .unwrap_or_else(|| (capacity as f64 * self.retain_ratio.unwrap_or(0.16)).floor() as u64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Trigger {
    Pressure,
    ContextOverflow,
    Manual,
    BytePressure,
}

pub(crate) const PRUNE_MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";
pub(crate) const INSTRUCTION: &str = include_str!("instruction.txt");
pub(crate) fn frame_summary(text: &str) -> String {
    format!(
        "This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. Treat the captured context as established background and build on it without restating it. Continue the task directly from the messages that follow, without acknowledging this checkpoint.\n\n<compacted-summary>\n{text}\n</compacted-summary>"
    )
}

/// Pricing is UTF-16 compatible with the Harness estimator, not a tokenizer.
pub(crate) fn text_tokens(text: &str) -> u64 {
    (text.encode_utf16().count() as u64).div_ceil(4)
}
pub(crate) fn json_tokens(value: &impl Serialize) -> u64 {
    text_tokens(&serde_json::to_string(value).unwrap_or_default())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Anchor {
    pub header_hash: String,
    pub estimated: u64,
    pub reported: u64,
}
pub(crate) fn hash(value: &impl Serialize) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable context"))
    )
}
pub(crate) fn header_hash(request: &ModelToolRequestV1) -> String {
    hash(
        &json!({"system":request.input["messages"].as_array().into_iter().flatten().filter(|m|m["role"]=="system").collect::<Vec<_>>(),"tools":request.tools,"parameters":request.parameters}),
    )
}
pub(crate) fn pressure(
    request: &ModelToolRequestV1,
    anchor: Option<&Anchor>,
) -> Result<u64, String> {
    let estimated = estimate(request)?;
    Ok(
        match anchor.filter(|a| a.header_hash == header_hash(request) && a.reported >= a.estimated)
        {
            Some(a) => a
                .reported
                .saturating_add(estimated)
                .saturating_sub(a.estimated),
            None => estimated,
        },
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn an_overlay_replaces_only_the_keys_a_node_declares() {
        let frozen = Policy {
            auto: true,
            prune_tool_results: true,
            threshold_chars: 8192,
            head_chars: 4096,
            tail_chars: 1024,
            ..Policy::default()
        };
        let mut disabled = frozen.clone();
        Overlay {
            auto: Some(false),
            ..Overlay::default()
        }
        .apply(&mut disabled);
        assert!(!disabled.auto);
        // Every other knob keeps the Chat value.
        assert_eq!(disabled.threshold_chars, 8192);
        assert_eq!(disabled.prune_tool_results, true);

        let mut tightened = frozen.clone();
        Overlay {
            threshold_chars: Some(2048),
            head_chars: Some(1024),
            tail_chars: Some(256),
            ..Overlay::default()
        }
        .apply(&mut tightened);
        assert_eq!(tightened.threshold_chars, 2048);
        assert_eq!(tightened.head_chars, 1024);
        assert_eq!(tightened.tail_chars, 256);
        assert!(tightened.auto);
    }

    #[test]
    fn an_absent_or_malformed_node_overlay_inherits_the_chat_policy() {
        assert!(Overlay::from_node_configuration(&json!({})).is_none());
        assert!(Overlay::from_node_configuration(&json!({"compaction": null})).is_none());
        // An unknown key or wrong type cannot be trusted to change context
        // behaviour, so it is treated as inherit rather than applied.
        assert!(
            Overlay::from_node_configuration(&json!({"compaction": {"thresholdRatio": 0.5}}))
                .is_none()
        );
        assert!(Overlay::from_node_configuration(&json!({"compaction": {"auto": "no"}})).is_none());
        let parsed = Overlay::from_node_configuration(&json!({"compaction": {"auto": false}}))
            .expect("a well-formed overlay parses");
        assert_eq!(parsed.auto, Some(false));
    }
}
