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
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            OpenProcess, OpenThread, ResumeThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
            THREAD_SUSPEND_RESUME,
        },
    },
};

pub(super) fn resume(pid: u32) -> Result<()> {
    // ResumeThread takes a THREAD handle, not the process handle returned by
    // OpenProcess. Enumerate the suspended process's threads and resume each.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error()).context("listing suspended provider threads");
    }
    let mut entry: THREADENTRY32 = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of::<THREADENTRY32>() as u32;
    let mut found = false;
    let mut ok = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while ok {
        if entry.th32OwnerProcessID == pid {
            found = true;
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                let error = io::Error::last_os_error();
                unsafe { windows_sys::Win32::Foundation::CloseHandle(snapshot) };
                return Err(error).context("opening suspended provider thread");
            }
            let status = unsafe { ResumeThread(thread) };
            unsafe { windows_sys::Win32::Foundation::CloseHandle(thread) };
            if status == u32::MAX {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(snapshot) };
                return Err(io::Error::last_os_error()).context("resuming provider thread");
            }
        }
        ok = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { windows_sys::Win32::Foundation::CloseHandle(snapshot) };
    if !found {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no suspended provider thread",
        ))
        .context("suspended provider has no thread");
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
