//! A stopped pass retains graph progress; the next input resumes with fresh calls.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StoppedGraphPassV1 {
    pub schema_version: u16,
    pub node_id: String,
    pub values: BTreeMap<String, Value>,
    pub completed: Vec<String>,
    pub active_edges: BTreeSet<usize>,
}

impl PassMachine<'_> {
    /// Restore only graph progress. Model context comes from its canonical
    /// checkpoint, and the current conversation includes the new steering input.
    pub(super) fn restore_stopped(&mut self, stopped: &StoppedGraphPassV1) -> Result<(), String> {
        if stopped.schema_version != 1
            || !self
                .compiled
                .nodes
                .iter()
                .any(|node| node.id == stopped.node_id)
            || stopped
                .active_edges
                .iter()
                .any(|index| *index >= self.compiled.edges.len())
            || stopped.completed.iter().any(|id| {
                !stopped.values.contains_key(id)
                    || !self.compiled.nodes.iter().any(|node| &node.id == id)
            })
            || stopped.values.len() != stopped.completed.len()
        {
            return Err("Stopped workflow checkpoint does not match the frozen graph".into());
        }
        self.values = stopped.values.clone();
        self.completed = stopped.completed.clone();
        self.executed = stopped.completed.iter().cloned().collect();
        self.active_edges = stopped.active_edges.clone();
        self.steered_node = Some(stopped.node_id.clone());
        Ok(())
    }

    pub(super) fn stopped_outcome(
        &mut self,
        node: &CompiledGraphNodeV1,
        error: String,
        started: bool,
    ) -> GraphPassOutcomeV1 {
        // A standalone tool has no owning Agent to reconsider it. Preserve its
        // cancellation as input to successors instead of dispatching it again.
        if started && node.node_type == "tool" {
            self.values
                .insert(node.id.clone(), json!({"cancelled":true,"error":error}));
            self.completed.push(node.id.clone());
        }
        let mut outcome = self.failed_outcome(error);
        outcome.stopped_state = Some(StoppedGraphPassV1 {
            schema_version: 1,
            node_id: node.id.clone(),
            values: self.values.clone(),
            completed: self.completed.clone(),
            active_edges: self.active_edges.clone(),
        });
        outcome
    }
}
