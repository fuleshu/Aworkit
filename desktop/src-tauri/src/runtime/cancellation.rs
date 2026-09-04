//! Out-of-band cancellation for the one workflow currently owned by the
//! desktop runtime. The controller is cloneable outside the runtime mutex so a
//! Stop command never waits behind the work it is meant to interrupt.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use aworkit_capability_host::CancellationToken;

#[derive(Clone, Default)]
pub struct WorkflowCancellationController {
    state: Arc<Mutex<CancellationState>>,
}

#[derive(Default)]
struct CancellationState {
    active: Option<ActiveWorkflow>,
    pending: Option<PendingStopRequestV1>,
    requested: BTreeMap<String, StopRequestV1>,
}

struct PendingStopRequestV1 {
    command_id: String,
    chat_id: String,
}

struct ActiveWorkflow {
    chat_id: String,
    run_id: String,
    cancellation: CancellationToken,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StopRequestV1 {
    pub(crate) command_id: String,
    pub(crate) chat_id: String,
    pub(crate) run_id: String,
}

pub(crate) struct ActiveWorkflowGuard {
    controller: WorkflowCancellationController,
    chat_id: String,
    run_id: String,
}

impl WorkflowCancellationController {
    pub(crate) fn register(
        &self,
        chat_id: &str,
        run_id: &str,
        cancellation: CancellationToken,
    ) -> Result<ActiveWorkflowGuard, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "workflow cancellation state is unavailable".to_owned())?;
        if state.active.is_some() {
            return Err("another workflow is already registered for cancellation".into());
        }
        state.active = Some(ActiveWorkflow {
            chat_id: chat_id.to_owned(),
            run_id: run_id.to_owned(),
            cancellation: cancellation.clone(),
        });
        if state
            .pending
            .as_ref()
            .is_some_and(|request| request.chat_id == chat_id)
        {
            let request = state.pending.take().expect("pending request exists");
            cancellation.cancel();
            state.requested.insert(
                run_id.to_owned(),
                StopRequestV1 {
                    command_id: request.command_id,
                    chat_id: request.chat_id,
                    run_id: run_id.to_owned(),
                },
            );
        }
        Ok(ActiveWorkflowGuard {
            controller: self.clone(),
            chat_id: chat_id.to_owned(),
            run_id: run_id.to_owned(),
        })
    }

    /// Requests cancellation only when the UI targets the exact active Chat.
    /// `false` means no workflow was active; suspended approvals are settled by
    /// the normal runtime command after it acquires the mutex.
    pub fn request_stop(&self, chat_id: &str, command_id: &str) -> Result<bool, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "workflow cancellation state is unavailable".to_owned())?;
        let Some(active) = state.active.as_ref() else {
            if let Some(pending) = state.pending.as_ref() {
                return if pending.command_id == command_id && pending.chat_id == chat_id {
                    Ok(false)
                } else {
                    Err("another Stop request is already pending".into())
                };
            }
            // The run span is committed immediately before pipeline dispatch.
            // Queue this exact target across that narrow registration window;
            // the Tauri command removes it again if normal command validation
            // later proves that no workflow was running.
            state.pending = Some(PendingStopRequestV1 {
                command_id: command_id.to_owned(),
                chat_id: chat_id.to_owned(),
            });
            return Ok(false);
        };
        if active.chat_id != chat_id {
            return Err("Stop targeted a Chat other than the active workflow".into());
        }
        let request = StopRequestV1 {
            command_id: command_id.to_owned(),
            chat_id: active.chat_id.clone(),
            run_id: active.run_id.clone(),
        };
        active.cancellation.cancel();
        let run_id = active.run_id.clone();
        state.requested.insert(run_id, request);
        Ok(true)
    }

    /// Removes an unclaimed early request after the ordinary Stop command has
    /// settled or failed validation. Requests already claimed by a run are
    /// consumed by `take_request` and are unaffected.
    pub fn discard_request(&self, command_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            if state
                .pending
                .as_ref()
                .is_some_and(|request| request.command_id == command_id)
            {
                state.pending = None;
            }
            state
                .requested
                .retain(|_, request| request.command_id != command_id);
        }
    }

    pub(crate) fn take_request(&self, chat_id: &str, run_id: &str) -> Option<StopRequestV1> {
        self.state
            .lock()
            .ok()
            .and_then(|mut state| state.requested.remove(run_id))
            .filter(|request| request.chat_id == chat_id)
    }

    fn unregister(&self, chat_id: &str, run_id: &str) {
        if let Ok(mut state) = self.state.lock()
            && state.active.as_ref().is_some_and(|active| {
                active.chat_id == chat_id && active.run_id == run_id
            })
        {
            state.active = None;
        }
    }
}

impl Drop for ActiveWorkflowGuard {
    fn drop(&mut self) {
        self.controller.unregister(&self.chat_id, &self.run_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_is_scoped_to_the_exact_active_chat_and_consumed_once() {
        let controller = WorkflowCancellationController::default();
        let cancellation = CancellationToken::default();
        let guard = controller
            .register("chat.active", "run.active", cancellation.clone())
            .expect("register");

        assert!(controller.request_stop("chat.other", "chat.stop").is_err());
        assert!(!cancellation.is_cancelled());
        assert!(
            controller
                .request_stop("chat.active", "chat.stop")
                .expect("stop")
        );
        assert!(cancellation.is_cancelled());
        assert_eq!(
            controller
                .take_request("chat.active", "run.active")
                .expect("request")
                .command_id,
            "chat.stop"
        );
        assert!(
            controller
                .take_request("chat.active", "run.active")
                .is_none()
        );
        drop(guard);
        assert!(
            !controller
                .request_stop("chat.active", "chat.stop-again")
                .expect("idle")
        );
        controller.discard_request("chat.stop-again");
    }

    #[test]
    fn stop_queued_just_before_registration_cancels_that_exact_chat() {
        let controller = WorkflowCancellationController::default();
        assert!(
            !controller
                .request_stop("chat.active", "chat.stop-early")
                .expect("queue early stop")
        );
        let cancellation = CancellationToken::default();
        let _guard = controller
            .register("chat.active", "run.active", cancellation.clone())
            .expect("register");

        assert!(cancellation.is_cancelled());
        assert_eq!(
            controller
                .take_request("chat.active", "run.active")
                .expect("claimed request")
                .command_id,
            "chat.stop-early"
        );
    }
}
