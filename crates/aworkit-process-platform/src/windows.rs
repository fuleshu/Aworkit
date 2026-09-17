//! Windows job objects retain descendant membership after the root exits.
use std::{
    io,
    mem::size_of,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::{Child, Command},
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::{Diagnostics::ToolHelp::*, JobObjects::*, Threading::*},
};

pub struct ProcessTree {
    child: Child,
    job: OwnedHandle,
}

fn check(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl ProcessTree {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        // SAFETY: null security/name arguments request an unnamed non-inherited job.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh valid owned handle.
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the handle and correctly sized input structure remain live for the call.
        check(unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        let mut child = command
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()?;
        // SAFETY: both handles are live; the suspended child cannot create descendants yet.
        if let Err(error) = check(unsafe { AssignProcessToJobObject(raw, child.as_raw_handle()) }) {
            let _ = child.kill();
            return Err(error);
        }
        if let Err(error) = resume(child.id()) {
            let _ = child.kill();
            return Err(error);
        }
        Ok(Self { child, job })
    }

    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn is_running(&mut self) -> io::Result<bool> {
        // Reap/cache the root status independently of descendant membership.
        self.child.try_wait()?;
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: a live job handle and an exclusive correctly sized output structure.
        check(unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut info).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        })?;
        Ok(info.ActiveProcesses != 0)
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        // SAFETY: the owned job handle remains valid, even when the root has exited.
        check(unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) })
    }
}

fn resume(process_id: u32) -> io::Result<()> {
    // SAFETY: snapshot creation has no borrowed pointer parameters.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the snapshot API returned a fresh valid owned handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: snapshot and correctly sized output remain live throughout enumeration.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while more != 0 {
        if entry.th32OwnerProcessID == process_id {
            // SAFETY: opening the enumerated thread with only resume rights.
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if raw_thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: OpenThread returned a fresh valid owned handle.
            let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread) };
            // SAFETY: thread handle is valid and belongs to our suspended root process.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: same valid snapshot and exclusive entry as above.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Err(io::Error::other("suspended process thread not found"))
}
