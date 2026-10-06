//! Recovery for an Agent turn that arrived with neither assistant text nor a tool call.
//!
//! A provider can accept a turn and still return nothing the loop can act on: the
//! response is cut off at the model's own output ceiling, a content filter empties
//! it, or the stream ends after reasoning deltas only. The failure policy allows
//! exactly three endings for an Agent node - the model returned its final answer,
//! the user cancelled, or an authority denied the call unrecoverably - so an empty
//! turn is a condition to report and continue, never a reason to stop the Run.
//!
//! The recorded case is chat.f232dbd770679e264ca8f27562c8bc15f73f27f4, whose third
//! turn reported exactly 65,536 output tokens and produced no final text. The Run
//! was durably classified as failed and continued only because the user asked why.
//! The output-token figure is the fingerprint of that cause, so it is what the
//! notice reports beside an instruction the model can act on.

use super::*;

/// One turn that produced nothing the loop can execute or answer with.
pub(crate) struct EmptyTurnV1 {
    /// The turn number the loop was on, so the notice identifies the turn.
    pub turn: u32,
    /// Output tokens the empty turn reported. A value at the model's own ceiling
    /// is how a response cut off by its output limit shows up here.
    pub output_tokens: u64,
}

impl EmptyTurnV1 {
    /// What the model is told about the empty turn it produced.
    ///
    /// It reports the fact and offers the recovery that fits the recorded cause:
    /// a response cut off by the output ceiling finishes if the work is split.
    pub(crate) fn notice(&self) -> String {
        format!(
            "Aworkit provider notice: turn {} returned no assistant text and no tool call, so \
             nothing was executed and nothing was replayed. That turn reported {} output tokens. \
             If the response was cut off at the model's output ceiling, continue in smaller steps - \
             for example write one large file in several edits instead of a single response - and \
             say what you are doing next.",
            self.turn, self.output_tokens
        )
    }
}

/// Records what an empty turn produced and asks the model to continue.
///
/// An empty turn is never an ending. Content the turn did produce (reasoning, or a
/// partial answer) is committed so the transcript stays faithful and the model can
/// see what it already said; a turn that produced nothing at all is not committed,
/// because an empty assistant message is not a useful history entry. Either way the
/// notice carries the condition into the next request. A failed durable commit is
/// reported rather than added as a second ending, exactly as the job-completion
/// barrier does.
pub(crate) fn recover(
    authority: &dyn ModelToolInvocationPortV1,
    outer: &StableId,
    empty: EmptyTurnV1,
    content: &[aworkit_capability_host::ModelAssistantContentV1],
    exchanges: &mut Vec<ModelToolExchangeV1>,
    notice: &mut Option<String>,
) {
    if !content.is_empty() {
        let exchange = ModelToolExchangeV1 {
            assistant_content: content.to_vec(),
            results: Vec::new(),
        };
        if let Err(error) = authority.commit_exchange(outer, empty.turn, &exchange) {
            append_runtime_notices(
                notice,
                vec![format!(
                    "Aworkit commit notice: an empty turn could not be durably committed \
                     ({error}). Nothing was replayed."
                )],
            );
        }
        exchanges.push(exchange);
    }
    append_runtime_notices(notice, vec![empty.notice()]);
}
