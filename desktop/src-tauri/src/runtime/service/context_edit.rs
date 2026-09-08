//! Version-checked context revisions committed by the desktop command boundary.
use super::*;
use crate::runtime::context_inspection::{ContextDocument, select_context};

impl DesktopRuntime {
    pub(super) fn edit_context(
        &mut self,
        input: UiCommandInput,
        fingerprint: String,
    ) -> Result<UiCommandReceipt, String> {
        self.history.ensure_expected(input.expected_version)?;
        let snapshot = self.snapshot(0)?;
        if snapshot.chat.recovery_pending
            || !matches!(
                snapshot.chat.phase.as_str(),
                "waiting_input" | "completed" | "paused"
            )
        {
            return Err("Finish or stop the current turn before editing context.".into());
        }
        // A paused recovery or approval still owns a frozen request; never mutate it.
        if snapshot.events.iter().any(|e| {
            e.kind == "span.started"
                && !snapshot.events.iter().any(|end| {
                    matches!(
                        end.kind.as_str(),
                        "span.completed" | "span.failed" | "span.cancelled"
                    ) && end.span_id == e.span_id
                })
        }) {
            return Err("Finish or stop the current turn before editing context.".into());
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Edit {
            node_id: String,
            base_sequence: u64,
            document: ContextDocument,
        }
        let edit: Edit = serde_json::from_value(input.payload)
            .map_err(|e| format!("Invalid context edit: {e}"))?;
        let original = select_context(&snapshot.events, &edit.node_id)?;
        if original.sequence != edit.base_sequence {
            return Err(
                "Context changed while this panel was open. Reopen it before editing.".into(),
            );
        }
        edit.document.validate()?;
        if edit.document.tools != original.document.tools {
            return Err("Tool definitions are frozen for this Chat. Edit messages, injected context and exchanges; change tools in a new workflow.".into());
        }
        if edit.document == original.document {
            let receipt = UiCommandReceipt {
                command_id: input.command_id.clone(),
                accepted: true,
                current_version: snapshot.version,
                reason: None,
                credential_mutation: None,
            };
            // Formatting-only edits have no semantic event, but their command
            // identity must still reject reuse with different content.
            self.processed.insert(
                input.command_id,
                ProcessedCommand {
                    fingerprint,
                    receipt: receipt.clone(),
                },
            );
            return Ok(receipt);
        }
        // The before/after hashes refer to immutable event-backed documents.
        self.history.append(&input.command_id, &fingerprint, input.expected_version, vec![
            ("context.edited", json!({
                "nodeId":original.node_id, "baseSequence":original.sequence,
                "beforeHash":canonical_hash(&original.document)?, "afterHash":canonical_hash(&edit.document)?,
                "document":edit.document, "createdAt":now_label(),
                "body":"Model context edited by the user. Applies to subsequent calls of this workflow node.",
            }))
        ])
    }
}
