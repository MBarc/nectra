use anyhow::{Context, Result};
use std::{
    process::{Child, Command},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
static ACTIVE: Mutex<Option<Weak<AtomicBool>>> = Mutex::new(None);
static HANDLER: OnceLock<std::result::Result<(), String>> = OnceLock::new();
pub(super) struct InterruptGuard;
impl InterruptGuard {
    pub fn install(token: &CancellationToken) -> Result<Self> {
        HANDLER
            .get_or_init(|| {
                ctrlc::try_set_handler(|| {
                    let flag = ACTIVE
                        .lock()
                        .ok()
                        .and_then(|v| v.as_ref().and_then(Weak::upgrade));
                    if let Some(flag) = flag {
                        flag.store(true, Ordering::SeqCst);
                    } else {
                        std::process::exit(130);
                    }
                })
                .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!("installing interrupt handler: {e}"))?;
        *ACTIVE
            .lock()
            .map_err(|_| anyhow::anyhow!("interrupt state poisoned"))? =
            Some(Arc::downgrade(&token.0));
        let guard = Self;
        #[cfg(windows)]
        {
            // Launchers can pass down the independent "ignore Ctrl+C" attribute.
            // Registering ctrlc's handler does not clear it. Enable delivery only
            // after the handler and active token are both ready.
            // SAFETY: changes only this process's console control attribute.
            if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) } == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("enabling terminal interrupt delivery");
            }
        }
        Ok(guard)
    }
}
impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            *active = None;
        }
    }
}

pub(super) struct Scope {
    #[cfg(windows)]
    job: super::windows_job::Job,
    #[cfg(unix)]
    group: libc::pid_t,
}
impl Scope {
    pub fn prepare(command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(not(unix))]
        let _ = command;
    }
    // Only call for a child launched into a fresh group/session by us.
    pub fn attach(pid: u32) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                job: super::windows_job::Job::attach(pid)?,
            })
        }
        #[cfg(unix)]
        {
            let group = libc::pid_t::try_from(pid).context("provider PID out of range")?;
            // SAFETY: querying the fresh child, never the caller's process group.
            if group <= 1 || unsafe { libc::getpgid(group) } != group {
                anyhow::bail!("provider is not in its own process group");
            }
            Ok(Self { group })
        }
    }
    pub fn terminate(&self) {
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        {
            // SAFETY: group was verified to be the group created for this launch.
            unsafe {
                libc::kill(-self.group, libc::SIGKILL);
            }
        }
    }
}

pub(super) struct OwnedChild {
    pub child: Child,
    scope: Option<Scope>,
}
impl OwnedChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        Scope::prepare(command);
        let child = command.spawn().context("launching chat agent")?;
        let mut owned = Self { child, scope: None };
        match Scope::attach(owned.child.id()) {
            Ok(scope) => owned.scope = Some(scope),
            Err(error) => {
                if owned.child.try_wait()?.is_none() {
                    return Err(error);
                }
            }
        }
        Ok(owned)
    }
    pub fn stop(&mut self) {
        if let Some(scope) = self.scope.take() {
            scope.terminate();
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) fn join_until<T>(handle: thread::JoinHandle<T>, deadline: Instant) -> Result<T> {
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !handle.is_finished() {
        anyhow::bail!("provider pipe did not close after shutdown");
    }
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("provider pipe worker panicked"))
}
