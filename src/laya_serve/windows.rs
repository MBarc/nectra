//! The Windows transport: a named pipe whose name carries the user's SID, whose
//! DACL grants that user alone and refuses remote clients, created with
//! FILE_FLAG_FIRST_PIPE_INSTANCE so a squatter's pipe is never ours. A client
//! checks that the process serving the pipe runs as the same user.
use super::Endpoint;
use anyhow::{bail, Result};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::PathBuf,
    ptr::null_mut,
    sync::Mutex,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, GetLastError, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, TokenUser, SECURITY_ATTRIBUTES,
        TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
            PIPE_WAIT,
        },
        Threading::{
            GetCurrentProcess, OpenProcessToken,
        },
    },
};

const ERROR_PIPE_BUSY: i32 = 231;

/// A TOKEN_USER buffer, 8-byte aligned.
fn token_user(token: HANDLE) -> Option<Vec<u64>> {
    let mut len = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let ok = unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) };
    (ok != 0).then_some(buf)
}
fn sid_of(buf: &[u64]) -> *mut core::ffi::c_void {
    unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid }
}
fn own_user() -> Option<Vec<u64>> {
    let mut token: HANDLE = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let user = token_user(token);
    unsafe { CloseHandle(token) };
    user
}
fn sid_string(sid: *mut core::ffi::c_void) -> Option<String> {
    let mut wide: *mut u16 = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
        return None;
    }
    let mut len = 0;
    while unsafe { *wide.add(len) } != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) });
    unsafe { LocalFree(wide.cast()) };
    Some(s)
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

impl Endpoint {
    pub fn display(&self) -> String {
        self.0.display().to_string()
    }
}

pub fn default_endpoint(_create: bool) -> Result<Option<Endpoint>> {
    let user = own_user().ok_or_else(|| anyhow::anyhow!("cannot read this user's SID"))?;
    let sid = sid_string(sid_of(&user)).ok_or_else(|| anyhow::anyhow!("cannot format SID"))?;
    Ok(Some(Endpoint(PathBuf::from(format!(
        r"\\.\pipe\nectra-laya-{sid}"
    )))))
}

pub struct Conn(File);
impl Conn {
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }
    /// Pipe reads have no timeout; the caller's own deadline covers it.
    pub fn set_timeout(&self, _: Duration) -> io::Result<()> {
        Ok(())
    }
}
impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}
impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

pub fn connect(ep: &Endpoint) -> io::Result<Conn> {
    let denied = |m| io::Error::new(io::ErrorKind::PermissionDenied, m);
    let mut tries = 0;
    let file = loop {
        // Identification level: the server may learn who we are, not act as us.
        match OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(&ep.0)
        {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 5 => {
                tries += 1;
                unsafe { WaitNamedPipeW(wide(&ep.display()).as_ptr(), 200) };
            }
            Err(e) => return Err(e),
        }
    };
    // The pipe object must be owned by this user: a squatter cannot make it
    // so, and unlike a process id the owner cannot be recycled.
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    let rc = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    let same = rc == 0
        && matches!(own_user(), Some(me) if unsafe { EqualSid(sid_of(&me), owner) } != 0);
    if !descriptor.is_null() {
        unsafe { LocalFree(descriptor) };
    }
    if same {
        Ok(Conn(file))
    } else {
        Err(denied("the pipe is not owned by this user"))
    }
}

pub struct Listener {
    name: Vec<u16>,
    sa: SECURITY_ATTRIBUTES,
    /// The next instance, already created so a client never finds none.
    pending: Mutex<isize>,
}
// The descriptor is never freed or changed.
unsafe impl Send for Listener {}
unsafe impl Sync for Listener {}

fn create_instance(name: &[u16], sa: &SECURITY_ATTRIBUTES, first: bool) -> io::Result<isize> {
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            sa,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(h as isize)
    }
}

pub fn bind(ep: &Endpoint) -> Result<Listener> {
    let user = own_user().ok_or_else(|| anyhow::anyhow!("cannot read this user's SID"))?;
    let sid = sid_string(sid_of(&user)).ok_or_else(|| anyhow::anyhow!("cannot format SID"))?;
    // Protected DACL: full access for this user, nobody else.
    let mut descriptor = null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(&format!("D:P(A;;GA;;;{sid})")).as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if ok == 0 {
        bail!("cannot build the pipe's access list");
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let name = wide(&ep.display());
    let first = create_instance(&name, &sa, true).map_err(|e| {
        anyhow::anyhow!("another Laya server is running, or the pipe name is taken: {e}")
    })?;
    Ok(Listener {
        name,
        sa,
        pending: Mutex::new(first),
    })
}

pub fn activated() -> Option<Listener> {
    None
}

impl Listener {
    pub fn accept(&self) -> io::Result<Conn> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if *pending == 0 {
            *pending = create_instance(&self.name, &self.sa, false)?;
        }
        let h = *pending as HANDLE;
        let connected = unsafe { ConnectNamedPipe(h, null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        let failure = io::Error::last_os_error();
        // Ownership of the instance moves to the connection (or is closed).
        let file = unsafe { File::from_raw_handle(h) };
        // Replace it before anyone else looks for one; 0 means "make one next time".
        *pending = create_instance(&self.name, &self.sa, false).unwrap_or(0);
        if !connected {
            return Err(failure);
        }
        Ok(Conn(file))
    }
    pub fn cleanup(&self) {}
}
