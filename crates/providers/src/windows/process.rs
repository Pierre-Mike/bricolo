//! Child processes on Windows. There are no signals: a polite stop is
//! Ctrl+Break sent to the child's console process group, and the whole
//! tree is held in a job object that kills what is left when it closes.

use std::os::windows::io::RawHandle;

use ::windows::Win32::Foundation::{CloseHandle, HANDLE};
use ::windows::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use ::windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use ::windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
use ::windows::core::PCWSTR;

/// The creation flag that makes a child lead its own console process
/// group, so Ctrl+Break reaches it and what it starts, and not the node.
pub const NEW_PROCESS_GROUP: u32 = CREATE_NEW_PROCESS_GROUP.0;

/// Sends Ctrl+Break to the process group `pid` leads. Fails when the child
/// has no console in common with the node.
pub fn ctrl_break(pid: u32) -> bool {
    // SAFETY: no pointers; an unknown group only makes the call fail.
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) }.is_ok()
}

/// A job object holding one child and everything it starts. Dropping it
/// kills them all (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), and so does the
/// node exiting, since Windows then closes the handle.
pub struct Job(HANDLE);

// SAFETY: a job handle may be used and closed from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// Puts the process `handle` into a new kill-on-close job. Processes it
    /// started before this call stay outside.
    pub fn hold(handle: RawHandle) -> ::windows::core::Result<Job> {
        // SAFETY: plain Win32 calls on handles this function owns, and on
        // the child's process handle, which the caller keeps open.
        unsafe {
            let job = Job(CreateJobObjectW(None, PCWSTR::null())?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            AssignProcessToJobObject(job.0, HANDLE(handle))?;
            Ok(job)
        }
    }

    /// Kills every process in the job.
    pub fn kill(&self) {
        // SAFETY: a live job handle.
        let _ = unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: closes the handle `hold` created, once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
