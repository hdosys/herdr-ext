use std::{
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    process::Child,
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::HANDLE,
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

#[derive(Debug)]
pub(crate) struct ChildProcessJob(OwnedHandle);

impl ChildProcessJob {
    pub(crate) fn new_kill_on_close() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle.cast()) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(crate) fn assign(&self, child: &Child) -> io::Result<()> {
        self.assign_handle(child.as_raw_handle().cast())
    }

    pub(crate) fn assign_handle(&self, process: HANDLE) -> io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn active_processes(&self) -> io::Result<u32> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        if unsafe {
            QueryInformationJobObject(
                self.handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.handle(), 1) } == 0 {
            let error = io::Error::last_os_error();
            if self.active_processes()? != 0 {
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn wait_for_empty(
        &self,
        child: &mut Child,
        timeout: Duration,
        timeout_message: &str,
    ) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        let mut root_reaped = false;
        loop {
            root_reaped |= child.try_wait()?.is_some();
            if root_reaped && self.active_processes()? == 0 {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, timeout_message));
            }
            thread::sleep(Duration::from_millis(25).min(deadline - now));
        }
    }

    pub(crate) fn terminate_and_wait(
        &self,
        child: &mut Child,
        timeout: Duration,
    ) -> io::Result<()> {
        self.terminate()?;
        self.wait_for_empty(
            child,
            timeout,
            "timed out waiting for contained process-tree cleanup",
        )
    }

    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle().cast()
    }
}
