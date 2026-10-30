//! Scope cleanup to this newly launched provider and its descendants.
use anyhow::{Context, Result};
use std::{
    io, mem,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE},
    },
};

pub(super) fn resume(pid: u32) -> Result<()> {
    let process = unsafe {
        OpenProcess(
            PROCESS_SET_QUOTA | PROCESS_SUSPEND_RESUME | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if process.is_null() {
        return Err(io::Error::last_os_error()).context("opening suspended provider process");
    }
    // SAFETY: the handle refers to the suspended child created by Nectra.
    let status = unsafe { windows_sys::Win32::System::Threading::ResumeThread(process) };
    unsafe { windows_sys::Win32::Foundation::CloseHandle(process) };
    if status == u32::MAX {
        return Err(io::Error::last_os_error()).context("resuming provider process");
    }
    Ok(())
}

pub(super) struct Job(OwnedHandle);
impl Job {
    pub fn attach(pid: u32) -> Result<Self> {
        // SAFETY: null names/security attributes request an unnamed, non-inherited job.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error()).context("creating provider job");
        }
        // SAFETY: CreateJobObjectW returned a unique owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw as _) });
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the structure and byte size match the selected information class.
        if unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("configuring provider job");
        }
        // SAFETY: only the PID returned by our own spawn is passed here.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error()).context("opening provider process");
        }
        let process = unsafe { OwnedHandle::from_raw_handle(process as _) };
        if unsafe { AssignProcessToJobObject(raw, process.as_raw_handle() as HANDLE) } == 0 {
            return Err(io::Error::last_os_error())
                .context("assigning provider process to its cleanup job");
        }
        Ok(job)
    }
    pub fn terminate(&self) {
        // SAFETY: this handle owns only the provider process group created above.
        unsafe {
            TerminateJobObject(self.0.as_raw_handle() as HANDLE, 1);
        }
    }
}
