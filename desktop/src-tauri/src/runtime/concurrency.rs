//! Chat command ownership is independent of the desktop coordinator lock.
use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex},
};

use super::{DesktopRuntime, UiCommandInput, UiCommandReceipt};

#[derive(Clone, Default)]
pub(crate) struct ChatCommands(Arc<(Mutex<BTreeMap<String, String>>, Condvar)>);

pub(crate) struct ChatCommandLease {
    commands: ChatCommands,
    chat_id: String,
}

impl ChatCommands {
    pub(crate) fn active_ids(&self) -> Vec<String> {
        self.0
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn reserve(
        &self,
        chat_id: &str,
        command_id: &str,
    ) -> Result<ChatCommandLease, String> {
        let mut active = self
            .0
            .0
            .lock()
            .map_err(|_| "Chat command registry unavailable")?;
        if active.contains_key(chat_id) {
            return Err("This Chat already has an active command.".into());
        }
        active.insert(chat_id.into(), command_id.into());
        Ok(ChatCommandLease {
            commands: self.clone(),
            chat_id: chat_id.into(),
        })
    }

    fn wait_for_predecessor(
        &self,
        chat_id: &str,
        command_id: &str,
        cancel: bool,
    ) -> Result<(), String> {
        let mut active = self
            .0
            .0
            .lock()
            .map_err(|_| "Chat command registry unavailable")?;
        while active
            .get(chat_id)
            .is_some_and(|id| cancel || id == command_id)
        {
            active = self
                .0
                .1
                .wait(active)
                .map_err(|_| "Chat command registry unavailable")?;
        }
        Ok(())
    }
}

impl Drop for ChatCommandLease {
    fn drop(&mut self) {
        let mut active = self.commands.0.0.lock().unwrap_or_else(|e| e.into_inner());
        active.remove(&self.chat_id);
        self.commands.0.1.notify_all();
    }
}

/// Called on a blocking IPC worker. Only admission and feedback take the
/// coordinator lock. Provider/tool execution owns its immutable Chat worker.
pub fn dispatch_chat_command(
    runtime: Arc<Mutex<DesktopRuntime>>,
    mut command: UiCommandInput,
) -> Result<UiCommandReceipt, String> {
    let (commands, target) = {
        let core = runtime
            .lock()
            .map_err(|_| "desktop runtime lock unavailable")?;
        let target = core.command_chat_target(&command)?;
        (core.chat_commands(), target)
    };
    if !is_navigation(&command.action) {
        command.target_id = Some(target.clone());
        commands.wait_for_predecessor(&target, &command.command_id, command.action == "cancel")?;
    }
    let mut worker = {
        let mut core = runtime
            .lock()
            .map_err(|_| "desktop runtime lock unavailable")?;
        if is_navigation(&command.action) {
            return core.command(command);
        }
        core.prepare_chat_worker(&target, &command.command_id)?
    };
    let result = worker.execute(command);
    // Merge only explicit feedback, never a worker's old Settings snapshot.
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock unavailable")?
        .finish_chat_worker(&mut worker);
    result
}

pub(crate) fn is_navigation(action: &str) -> bool {
    matches!(
        action,
        "new_chat" | "select_chat" | "set_chat_pinned" | "delete_chat" | "fork"
    )
}
