//! Model-visible context reduction. Canonical conversation and settled tools
//! remain immutable; only a committed selection may replace a prompt prefix.
use super::context_inspection::ContextDocument;
use aworkit_capability_host::{
    model_images::ImageDispatchV1, ModelAssistantContentV1, ModelToolContextV1,
    ModelToolExchangeV1, ModelToolRequestV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
    /// The model's configured maximum output tokens. Compaction budgets are
    /// computed on the window the provider leaves for input, so this
    /// reservation is subtracted before any ratio applies. Absent on contexts
    /// frozen before the key existed, which reserves nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_target: Option<FrozenSummaryTarget>,
    #[serde(default)]
    pub policy: Policy,
}

/// How this Run's dispatches represent images, from the frozen model capability.
///
/// A model that cannot take images still receives an explicit reference for
/// every image, so the run keeps its evidence without uploading bytes the
/// provider would discard.
///
/// Only an explicit `imageInput: false` withholds the bytes. A context that
/// predates the key, or was frozen without an answer, keeps the previous
/// behaviour and attaches them: an unknown capability is not a denial, and
/// silently dropping image input from an existing Chat is the worse error.
pub(crate) fn image_dispatch(model_context: &Value) -> Result<ImageDispatchV1, String> {
    // Parsed for validation: a frozen context that no longer decodes is a
    // pipeline input error rather than a silently different dispatch.
    serde_json::from_value::<Metadata>(model_context.clone())
        .map_err(|error| format!("invalid frozen model context: {error}"))?;
    Ok(
        match model_context.get("imageInput").and_then(Value::as_bool) {
            Some(false) => ImageDispatchV1::Reference,
            _ => ImageDispatchV1::Attach,
        },
    )
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
    /// Share of the compacted replacement the summary takes; the retained tail
    /// keeps the rest. The one number in this model with no derivation, and so
    /// the one the Settings panel offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_share: Option<f64>,
    /// Legacy sink for the removed proportional retention. The retained tail is
    /// derived from the window now, so a frozen Chat that still carries
    /// `retainRatio` deserializes and the value means nothing.
    #[serde(default, rename = "retainRatio", skip_serializing)]
    pub _legacy_retain_ratio: Option<f64>,
    /// Verbatim tail for a Chat whose model declares no context window: the
    /// legacy byte-pressure path, where there is no ratio to derive from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_tokens: Option<u64>,
    /// Legacy sink for the removed absolute summary budget. A frozen Chat may
    /// still carry `maxTokens`; it is read into nothing and never serialized.
    #[serde(default, rename = "maxTokens", skip_serializing)]
    pub _legacy_max_tokens: Option<u64>,
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
fn one() -> u32 {
    1
}
// Compaction-side pruning has to actually fire before it can save a summary.
// Measured durable outcomes put the median tool result at 600-900 bytes, shell
// output at ~1.4 KB p90 and ~2.7 KB max, and the largest recorded job output at
// ~28 KB, so the previous 80 KiB gate sat about 100x above the median and never
// qualified. The default is now the same 8,000-character floor Hermes uses for
// proactive pruning: a result large enough to matter is reduced to its head and
// tail (the original stays durably retrievable) before a summary is paid for.
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
        let share = self.summary_share();
        if !self.threshold_ratio.is_finite()
            || !(0.0 < self.threshold_ratio && self.threshold_ratio <= 1.0)
            // A trigger at or below the occupancy target would leave the context
            // above it after every compaction, so the run would compact on every
            // request. This replaces the old retention-below-trigger check, which
            // guarded a ratio that no longer exists.
            || self.threshold_ratio <= COMPACTION_TARGET_RATIO
            || !share.is_finite()
            || !(0.0 < share && share < 1.0)
            || self.compaction_retries > 32
            || self.max_overflow_retries > 32
            || self.threshold_chars == 0
            || self.threshold_chars > 4 * 1024 * 1024
            || self
                .head_chars
                .saturating_add(self.tail_chars)
                .saturating_add(prune_marker_bound_chars())
                > self.threshold_chars
        {
            return Err("Invalid compaction policy: require a trigger above the quarter-window occupancy target, a summary share strictly between 0 and 1, and valid pruning budgets.".into());
        }
        if capacity == Some(0) {
            return Err("Compaction requires a positive context window.".into());
        }
        Ok(())
    }
    pub(crate) fn threshold(&self, capacity: u64) -> u64 {
        (capacity as f64 * self.threshold_ratio).floor() as u64
    }
    pub(crate) fn retention(&self) -> u64 {
        self.retain_tokens.unwrap_or(0)
    }

    /// Share of the replacement budget the summary takes. The retained tail
    /// keeps the remainder, so the setting is honoured exactly at every window.
    pub(crate) fn summary_share(&self) -> f64 {
        self.summary_share.unwrap_or(SUMMARY_BUDGET_SHARE)
    }

    /// The provider output reservation, out of the declared window.
    ///
    /// This is the one bound that is not a share of the context: it is the output
    /// the model itself declares it can emit, clamped to the target share so a
    /// model claiming more output than its whole window (393,216 against a
    /// 32,000-token window) cannot collapse the window to zero - a zero window
    /// has no threshold to cross, so it would compact on every turn. A model that
    /// declares nothing reserves nothing.
    pub(crate) fn output_reservation(&self, capacity: u64, max_output_tokens: Option<u64>) -> u64 {
        max_output_tokens
            .unwrap_or(0)
            .min((capacity as f64 * COMPACTION_TARGET_RATIO).floor() as u64)
    }

    /// The window compaction budgets may spend: the declared context window
    /// minus the provider's own output reservation, which comes out of the same
    /// window.
    pub(crate) fn effective_window(&self, capacity: u64, max_output_tokens: Option<u64>) -> u64 {
        capacity.saturating_sub(self.output_reservation(capacity, max_output_tokens))
    }

    /// Hard occupancy goal for one compaction, as a share of the window. Every
    /// compaction must reach it, or the run would compact endlessly instead of
    /// working.
    pub(crate) fn target(&self, window: u64) -> u64 {
        (window as f64 * COMPACTION_TARGET_RATIO).floor() as u64
    }

    /// Smallest replacement one compaction produces, as a share of the window.
    /// This is the floor for a window whose target is out of reach, and it is a
    /// share rather than a token count so it scales with the model.
    pub(crate) fn minimum_replacement(&self, window: u64) -> u64 {
        (window as f64 * COMPACTION_FLOOR_RATIO).floor() as u64
    }

    /// What one compaction replaces: the verbatim tail and the summary it pays
    /// for.
    ///
    /// `budget = target − fixed`, floored at the replacement share, and the
    /// summary takes its share of that budget while the tail keeps the rest. Both
    /// parts are therefore shares of the window, and a large context keeps
    /// proportionally more of both: the tail grows with the window instead of the
    /// summary being clipped to a constant, so a big window never behaves like a
    /// small one. The only limit that is not a context share is the model's own
    /// declared output, which bounds how long a summary may be asked to be.
    ///
    /// Below the floor the target is simply missed and the plan is the floor: a
    /// run that ignores its own declared window is worse than one that compacts
    /// badly.
    pub(crate) fn replacement_plan(
        &self,
        window: u64,
        fixed_tokens: u64,
        output_reservation: Option<u64>,
    ) -> ReplacementPlan {
        let budget = self
            .target(window)
            .saturating_sub(fixed_tokens)
            .max(self.minimum_replacement(window));
        // The summary takes its share first, bounded by what the model can emit;
        // the tail keeps the remainder, so a model that declares little output
        // shifts the whole budget into verbatim history rather than truncating
        // the plan.
        let summary = ((budget as f64 * self.summary_share()).floor() as u64)
            .min(output_reservation.unwrap_or(u64::MAX));
        ReplacementPlan {
            retain: budget.saturating_sub(summary),
            summary,
        }
    }

    /// Whether one compaction can leave the context below its own trigger. When
    /// the fixed context already reaches the trigger there is nothing to gain,
    /// and compacting would only oscillate.
    pub(crate) fn can_reduce(&self, window: u64, fixed_tokens: u64) -> bool {
        fixed_tokens.saturating_add(self.minimum_replacement(window)) < self.threshold(window)
    }

    /// Smallest window whose target reaches a given fixed context. Derived, not
    /// chosen: the target must cover the fixed context plus one replacement, so
    /// `W * (target - floor) >= fixed`. It moves with the tool selection the
    /// model actually sees.
    pub(crate) fn minimum_window(&self, fixed_tokens: u64) -> u64 {
        (fixed_tokens as f64 / (COMPACTION_TARGET_RATIO - COMPACTION_FLOOR_RATIO)).ceil() as u64
    }

    /// Why automatic compaction will miss its target here, if it will.
    ///
    /// Advisory, never a refusal: the caller compacts anyway with the floor
    /// replacement and surfaces this once per condition.
    pub(crate) fn compaction_advisory(&self, window: u64, fixed_tokens: u64) -> Option<String> {
        let minimum = self.minimum_replacement(window);
        if self.target(window) < fixed_tokens.saturating_add(minimum) {
            return Some(format!(
                "Automatic compaction cannot reach its {percent:.0}% target in a {window}-token window: the target is {target} tokens, the fixed context is {fixed_tokens} and the smallest replacement is {floor_percent:.0}% of the window, {minimum} tokens. About {required} tokens is the smallest window that reaches the target with this tool selection; compaction still runs, and will run often.",
                percent = COMPACTION_TARGET_RATIO * 100.0,
                floor_percent = COMPACTION_FLOOR_RATIO * 100.0,
                target = self.target(window),
                required = self.minimum_window(fixed_tokens),
            ));
        }
        None
    }
}

/// Occupancy one compaction must reach, as a share of the window.
pub(crate) const COMPACTION_TARGET_RATIO: f64 = 0.25;
/// Replacement share used when the target is out of reach, so even a window too
/// small for its own fixed context still reduces by a predictable fraction
/// instead of by an absolute token count.
///
/// Bounded above by the arithmetic itself: a floor at or above the target share
/// would clamp every window, so it must stay below `target - fixed/window` for
/// the windows the fixed context is meant to fit. At the recorded Chat's fixed
/// context of about 12,300 tokens a 100,000-token window is not clamped
/// (12,600 > 8,000) while a 32,000-token one is.
pub(crate) const COMPACTION_FLOOR_RATIO: f64 = 0.08;
/// Default share of the replacement budget spent on the summary; the retained
/// tail takes the rest. The one quality ratio in the model - prose against
/// verbatim history - so a larger window keeps more of both. A Chat may set its
/// own `summaryShare`, and this is what it gets when it does not.
pub(crate) const SUMMARY_BUDGET_SHARE: f64 = 0.382;

/// What one compaction replaces: the verbatim tail and the summary it pays for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReplacementPlan {
    pub retain: u64,
    pub summary: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Trigger {
    Pressure,
    ContextOverflow,
    Manual,
    BytePressure,
}

/// The elision notice is deliberately explicit: a model that only sees a terse
/// marker tends to keep reasoning from the truncated result instead of
/// recovering the omitted content.
pub(crate) const PRUNE_MARKER_PREFIX: &str = "\n\n[... middle of this tool result pruned: ";
pub(crate) const PRUNE_MARKER_SUFFIX: &str = " characters removed, so this result is incomplete. Re-read or re-run the request (with read_file offset and limit for a file) before relying on anything omitted here ...]\n\n";
pub(crate) fn prune_marker(removed: usize) -> String {
    format!("{PRUNE_MARKER_PREFIX}{removed}{PRUNE_MARKER_SUFFIX}")
}
/// Longest marker any removal count can produce.
pub(crate) fn prune_marker_bound_chars() -> usize {
    prune_marker(usize::MAX).chars().count()
}
pub(crate) const INSTRUCTION: &str = include_str!("instruction.txt");
/// Notice a replacement carries when a span left the selection without being
/// summarised, because the context no longer fitted the model's window and no
/// summary could be delivered.
///
/// Deliberately free of counts: it is built before the oldest units that fit are
/// chosen, so its own size is part of that choice. The counts belong in the
/// `context.compacted` evidence, not in the prompt.
pub(crate) const DROPPED_SPAN_MARKER: &str = "\n\n[... earlier history was dropped without a summary: this context exceeded the model's declared window, so the oldest exchanges were removed. The original conversation and tool results remain available in Run details - re-read or re-run anything you still need before relying on it ...]\n\n";
/// Opening sentence of every generated checkpoint. It frames the summary for the
/// model, and it is also the marker that tells a later compaction that a
/// user-role unit is a prior checkpoint to be consolidated rather than a real
/// user turn to be pinned.
pub(crate) const CHECKPOINT_PREAMBLE: &str = "This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. Treat the captured context as established background and build on it without restating it. Continue the task directly from the messages that follow, without acknowledging this checkpoint.";
pub(crate) fn frame_summary(text: &str) -> String {
    format!("{CHECKPOINT_PREAMBLE}\n\n<compacted-summary>\n{text}\n</compacted-summary>")
}
/// Whether one unit's content is a compaction checkpoint rather than a real turn.
pub(crate) fn is_checkpoint(content: &str) -> bool {
    content.starts_with(CHECKPOINT_PREAMBLE)
}

/// How many times one compaction attempt shrinks its summary prompt and tries
/// again before a failed summary becomes terminal.
///
/// Every recovery is a paid auxiliary call, so the bound is deliberately small.
/// The budget is per attempt: a committed reduction moves on to the next
/// attempt with a fresh one, so progress resets it.
pub(crate) const SUMMARY_SHRINK_RETRIES: u32 = 2;

/// Opening words of the generated messages that re-emit durable Run state
/// after a compaction. They are labels, not instructions: the state itself is
/// restored from records, so a compaction never carries an older copy of it
/// forward as if the user had written it.
pub(crate) const GOAL_STATE_LABEL: &str = "Current Chat goal (";
pub(crate) const TASK_STATE_LABEL: &str = "Current Run task list (";
pub(crate) const FILES_STATE_LABEL: &str = "Files this Run has already read or changed (";
/// Whether one unit's content is generated durable state rather than a real
/// user turn. Generated state is re-derived from records after every compaction
/// and when a restored checkpoint carries it, so pinning an older copy would
/// duplicate it and could contradict it.
pub(crate) fn is_generated_state(content: &str) -> bool {
    [GOAL_STATE_LABEL, TASK_STATE_LABEL, FILES_STATE_LABEL]
        .iter()
        .any(|label| content.starts_with(label))
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
        assert_eq!(disabled.threshold_chars, Policy::default().threshold_chars);
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
