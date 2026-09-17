//! POSIX process groups are the supported cleanup boundary (deliberate setsid escapes are excluded).
use command_group::{CommandGroup, GroupChild};
use std::{
    io,
    process::{Child, Command},
};
pub struct ProcessTree {
    child: GroupChild,
}
impl ProcessTree {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Ok(Self {
            child: command.group().spawn()?,
        })
    }
    pub fn child(&mut self) -> &mut Child {
        self.child.inner()
    }
    pub fn is_running(&mut self) -> io::Result<bool> {
        self.child.inner().try_wait()?;
        // SAFETY: signal zero probes the owned numeric process group without dereferencing pointers.
        let result = unsafe { libc::kill(-(self.child.id() as i32), 0) };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }
    pub fn terminate(&mut self) -> io::Result<()> {
        self.child.kill()
    }
}
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.inner().try_wait();
    }
}
