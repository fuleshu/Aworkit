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
    /// Repeated refusal is a visible failure with cleanup, never an infinite retry.
    #[allow(clippy::too_many_arguments)] // Preserve the parent loop's explicit checkpoint boundary.
    pub fn defer(
        &mut self,
        authority: &dyn ModelToolInvocationPortV1,
        outer: &StableId,
        turn: u32,
        content: &[aworkit_capability_host::ModelAssistantContentV1],
        exchanges: &mut Vec<ModelToolExchangeV1>,
        notice: &mut Option<String>,
    ) -> Result<bool, ModelToolLoopErrorV1> {
        let Some(outstanding) = authority
            .outstanding_jobs()
            .map_err(ModelToolLoopErrorV1::ToolAuthority)?
        else {
            return Ok(false);
        };
        self.ignored += 1;
        if self.ignored > 4 {
            authority.stop_unkept_jobs();
            return Err(ModelToolLoopErrorV1::ToolAuthority("The model repeatedly tried to finish with unresolved shell jobs. Cleanup was requested; inspect job_list/job_output before continuing.".into()));
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: content.to_vec(),
            results: Vec::new(),
        };
        authority
            .commit_exchange(outer, turn, &exchange)
            .map_err(ModelToolLoopErrorV1::ToolAuthority)?;
        exchanges.push(exchange);
        *notice = Some(outstanding);
        Ok(true)
    }
}
