//! Console-free background launches, without changing pipes or process ownership.
use std::{io, process::Command};

use command_group::{CommandGroup, GroupChild};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Suppress the implicit Windows console for a directly spawned background
/// command (including Tokio's underlying std command). GUI windows are unaffected.
/// Grouped commands must use `spawn_background_group`: command-group replaces
/// the command's creation flags with its own builder flags before spawning.
pub fn configure_background_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Preserve command-group containment and pipe behavior while suppressing the
/// implicit Windows console at the actual group spawn boundary.
pub fn spawn_background_group(command: &mut Command) -> io::Result<GroupChild> {
    let mut group = command.group();
    #[cfg(windows)]
    group.creation_flags(CREATE_NO_WINDOW);
    group.spawn()
}
