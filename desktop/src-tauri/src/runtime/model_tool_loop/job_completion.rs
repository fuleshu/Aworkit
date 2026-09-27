//! Runtime-owned completion barrier for asynchronous jobs, shared by all loop entry paths.
use super::*;

#[derive(Default)]
pub(super) struct CompletionGuard {
    ignored: usize,
}
impl CompletionGuard {
    pub fn progressed(&mut self) {
        self.ignored = 0;
    }
    /// Commit the attempted response as history and ask the model to resolve jobs.
    ///
    /// The barrier asks; it never ends the node. After repeated refusals the
    /// unkept jobs are stopped so background work cannot outlive the answer, and
    /// the model is told that cleanup happened.
    #[allow(clippy::too_many_arguments)] // Preserve the parent loop's explicit checkpoint boundary.
    pub fn defer(
        &mut self,
        authority: &dyn ModelToolInvocationPortV1,
        outer: &StableId,
        turn: u32,
        content: &[aworkit_capability_host::ModelAssistantContentV1],
        exchanges: &mut Vec<ModelToolExchangeV1>,
        notice: &mut Option<String>,
    ) -> bool {
        let outstanding = match authority.outstanding_jobs() {
            Ok(outstanding) => outstanding,
            Err(error) => {
                // The inventory could not be read: report it and let the answer
                // stand rather than ending the node.
                append_runtime_notices(
                    notice,
                    vec![format!(
                        "Aworkit job notice: the background-job inventory could not be read ({error}). \
                         Do not assume there is no background work; call job_list if it matters."
                    )],
                );
                return false;
            }
        };
        let Some(outstanding) = outstanding else {
            return false;
        };
        self.ignored += 1;
        if self.ignored > 4 {
            authority.stop_unkept_jobs();
            append_runtime_notices(
                notice,
                vec![
                    "Aworkit job notice: this answer repeatedly arrived with unresolved background \
                     jobs. They were stopped so no work outlives the response. Continue if the answer \
                     is still incomplete."
                        .to_owned(),
                ],
            );
            return false;
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: content.to_vec(),
            results: Vec::new(),
        };
        // A failed durable commit is reported here rather than adding a second
        // termination path to the barrier.
        if let Err(error) = authority.commit_exchange(outer, turn, &exchange) {
            append_runtime_notices(
                notice,
                vec![format!(
                    "Aworkit commit notice: the attempted response could not be durably committed \
                     ({error}). Nothing was replayed."
                )],
            );
        }
        exchanges.push(exchange);
        *notice = Some(outstanding);
        true
    }
}
