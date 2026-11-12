//! The unix transport: a socket in a private directory, mode 0600, with the
//! peer's uid checked on both ends, and a lock file so only one server binds it.
use super::Endpoint;
use anyhow::{bail, Context, Result};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::{AsRawFd, FromRawFd},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

fn euid() -> u32 {
    unsafe { libc::geteuid() }
}

/// The uid of the process at the other end of a connected socket.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        Ok(cred.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `dir` exists (made, if `create`), is a real directory and is this user's
/// alone. `Ok(false)` when it is absent and not to be made.
fn private_dir(dir: &Path, create: bool) -> Result<bool> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
        }
    }
    let meta = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound && !create => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    if !meta.is_dir() || meta.uid() != euid() || meta.mode() & 0o077 != 0 {
        bail!(
            "{} is not a directory private to this user (mode 0700); refusing to use it",
            dir.display()
        );
    }
    Ok(true)
}

/// `$XDG_RUNTIME_DIR/nectra/laya.sock`; without a runtime directory (macOS,
/// containers) `$TMPDIR/nectra-<uid>/laya.sock`. None when the directory
/// does not exist and `create` is false: no server can be there.
pub fn default_endpoint(create: bool) -> Result<Option<Endpoint>> {
    let dir = match dirs::runtime_dir() {
        Some(runtime) => runtime.join("nectra"),
        None => std::env::temp_dir().join(format!("nectra-{}", euid())),
    };
    if !private_dir(&dir, create)? {
        return Ok(None);
    }
    Ok(Some(Endpoint::new(dir.join("laya.sock"))?))
}

impl Endpoint {
    pub fn new(sock: PathBuf) -> Result<Self> {
        // sun_path is 108 bytes on Linux, 104 on the BSDs.
        if sock.as_os_str().len() > 100 {
            bail!("socket path {} is too long", sock.display());
        }
        Ok(Self(sock))
    }
    pub fn display(&self) -> String {
        self.0.display().to_string()
    }
}

pub struct Conn(UnixStream);
impl Conn {
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }
    pub fn set_timeout(&self, d: Duration) -> io::Result<()> {
        self.0.set_read_timeout(Some(d))?;
        self.0.set_write_timeout(Some(d))
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

/// Connects to the server of `ep`, but only if the socket is this user's own.
pub fn connect(ep: &Endpoint) -> io::Result<Conn> {
    let meta = fs::symlink_metadata(&ep.0)?;
    if !meta.file_type().is_socket() || meta.uid() != euid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not this user's socket",
        ));
    }
    let s = UnixStream::connect(&ep.0)?;
    if peer_uid(&s)? != euid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the server is another user's",
        ));
    }
    Ok(Conn(s))
}

pub struct Listener {
    inner: UnixListener,
    /// The socket file to remove on the way out (not one systemd owns).
    path: Option<PathBuf>,
    _lock: Option<fs::File>,
}

/// Binds `ep`. The lock file makes this exclusive between servers, so while
/// it is held any socket already there is stale.
pub fn bind(ep: &Endpoint) -> Result<Listener> {
    let dir = ep.0.parent().context("socket has no directory")?;
    private_dir(dir, false)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(ep.0.with_extension("lock"))
        .context("opening the lock file")?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("another Laya server is running ({})", ep.display());
    }
    match fs::symlink_metadata(&ep.0) {
        Ok(m) if m.file_type().is_socket() => fs::remove_file(&ep.0)?,
        Ok(_) => bail!("{} exists and is not a socket", ep.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let inner = UnixListener::bind(&ep.0).with_context(|| format!("binding {}", ep.display()))?;
    fs::set_permissions(&ep.0, fs::Permissions::from_mode(0o600))?;
    Ok(Listener {
        inner,
        path: Some(ep.0.clone()),
        _lock: Some(lock),
    })
}

/// The socket systemd passes to a socket-activated service (fd 3).
pub fn activated() -> Option<Listener> {
    let mine = std::env::var("LISTEN_PID").ok()?.parse::<u32>().ok()? == std::process::id();
    let one = std::env::var("LISTEN_FDS").ok()?.parse::<u32>().ok()? == 1;
    if !(mine && one) {
        return None;
    }
    // SAFETY: systemd says fd 3 is ours and a listening socket; nothing else owns it.
    let inner = unsafe { UnixListener::from_raw_fd(3) };
    Some(Listener {
        inner,
        path: None,
        _lock: None,
    })
}

impl Listener {
    /// The next connection from this user; another user's is closed and an
    /// error returned.
    pub fn accept(&self) -> io::Result<Conn> {
        let (s, _) = self.inner.accept()?;
        if peer_uid(&s)? != euid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "connection from another user",
            ));
        }
        Ok(Conn(s))
    }
    pub fn cleanup(&self) {
        if let Some(p) = &self.path {
            let _ = fs::remove_file(p);
        }
    }
}
