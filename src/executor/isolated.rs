//! Linux containers are the write boundary; provider permission flags are secondary.
use super::{conversation, process, CancellationToken};
use crate::{models::Runtime, router::Decision};
use anyhow::{bail, Context, Result};
use std::{
    env,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const PROBE: &str = include_str!("../../tools/debate-container/probe.py");

#[derive(Clone)]
pub struct IsolatedExecutor {
    docker: PathBuf,
    workspace: PathBuf,
    image: String,
    verified_image: Arc<Mutex<Option<(String, String)>>>,
}

impl IsolatedExecutor {
    pub fn from_env(workspace: &Path) -> Result<Self> {
        let docker =
            super::resolve(&env::var("NECTRA_DEBATE_DOCKER").unwrap_or_else(|_| "docker".into()))?;
        let image =
            env::var("NECTRA_DEBATE_IMAGE").unwrap_or_else(|_| "nectra-debate:local".into());
        if image.is_empty() || image.starts_with('-') || image.chars().any(char::is_whitespace) {
            bail!("invalid NECTRA_DEBATE_IMAGE");
        }
        let workspace = workspace
            .canonicalize()
            .context("resolving debate workspace")?;
        mount(&workspace, "/workspace")?;
        Ok(Self {
            docker,
            workspace,
            image,
            verified_image: Arc::new(Mutex::new(None)),
        })
    }

    pub fn supports(&self, runtime: Runtime) -> bool {
        matches!(runtime, Runtime::Claude | Runtime::Codex)
    }

    /// No provider call is allowed until a disposable OS-denial probe succeeds.
    pub fn preflight(&self) -> Result<()> {
        *self
            .verified_image
            .lock()
            .map_err(|_| anyhow::anyhow!("preflight state poisoned"))? = None;
        let info = self.control(&["info", "--format", "{{.OSType}}"], Duration::from_secs(15))
            .context("debate requires a running local Linux Docker engine; no unrestricted fallback is allowed")?;
        if info.trim() != "linux" {
            bail!("debate requires Linux containers");
        }
        let endpoint = self.control(
            &[
                "context",
                "inspect",
                "--format",
                "{{.Endpoints.docker.Host}}",
            ],
            Duration::from_secs(10),
        )?;
        if env::var_os("DOCKER_HOST").is_some()
            || !(endpoint.trim().starts_with("npipe://") || endpoint.trim().starts_with("unix://"))
        {
            bail!("debate requires a local Docker endpoint, without DOCKER_HOST override");
        }
        let endpoint = endpoint.trim().to_string();
        let id = capture(pinned_command(&self.docker, &endpoint).args(["image", "inspect", "--format", "{{.Id}}", &self.image]), Duration::from_secs(15))
            .context("build the debate image first: docker build -t nectra-debate:local tools/debate-container")?;
        let id = id.trim();
        if !id.starts_with("sha256:") || id.len() != 71 {
            bail!("unexpected Docker image identity");
        }
        let fixture = tempfile::tempdir().context("creating isolation probe")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // DAC must permit mutation: denial must come from the mount boundary,
            // not merely from a directory owned by a different UID.
            fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o777))?;
        }
        for name in ["read.txt", "overwrite.txt", "delete.txt", "rename.txt"] {
            let path = fixture.path().join(name);
            fs::write(&path, "nectra-probe")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
            }
        }
        let name = unique_name();
        let mut cleanup =
            ContainerCleanup::new(self.docker.clone(), endpoint.clone(), name.clone());
        let mut command = pinned_command(&self.docker, &endpoint);
        command
            .args(container_args(fixture.path(), id, &name)?)
            .args(["--entrypoint", "python3", id, "-c", PROBE]);
        let result = capture(&mut command, Duration::from_secs(30));
        let cleaned = cleanup.finish();
        let output = result?;
        cleaned?;
        let report: serde_json::Value =
            serde_json::from_str(output.trim()).context("invalid isolation probe response")?;
        if report["denied"] != 5 || report["read"] != true {
            bail!("container write-denial probe failed: {report}");
        }
        for name in ["read.txt", "overwrite.txt", "delete.txt", "rename.txt"] {
            if fs::read_to_string(fixture.path().join(name))? != "nectra-probe" {
                bail!("isolation probe mutated a file");
            }
        }
        if fs::read_dir(fixture.path())?.count() != 4 {
            bail!("isolation probe changed directory entries");
        }
        *self
            .verified_image
            .lock()
            .map_err(|_| anyhow::anyhow!("preflight state poisoned"))? =
            Some((id.into(), endpoint));
        Ok(())
    }

    pub fn respond(
        &self,
        decision: &Decision,
        prompt: &str,
        output: &mut dyn Write,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let (id, endpoint) = self
            .verified_image
            .lock()
            .map_err(|_| anyhow::anyhow!("preflight state poisoned"))?
            .clone()
            .context("debate isolation preflight must succeed before provider execution")?;
        let args = provider_args(decision)?;
        if cancel.is_cancelled() {
            bail!("Turn cancelled");
        }
        let name = unique_name();
        let mut cleanup =
            ContainerCleanup::new(self.docker.clone(), endpoint.clone(), name.clone());
        let mut command = pinned_command(&self.docker, &endpoint);
        command.args(container_args(&self.workspace, &id, &name)?);
        let (key, auth, target) = match decision.runtime.as_str() {
            "claude" => (
                "ANTHROPIC_API_KEY",
                dirs::home_dir().map(|h| h.join(".claude/.credentials.json")),
                "/run/nectra/claude-auth.json",
            ),
            "codex" => (
                "OPENAI_API_KEY",
                env::var_os("CODEX_HOME")
                    .map(PathBuf::from)
                    .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
                    .map(|h| h.join("auth.json")),
                "/run/nectra/codex-auth.json",
            ),
            _ => unreachable!(),
        };
        // A private parent protects the temporary secret from other host users;
        // its directly mounted file is readable by the container's fixed UID.
        let credentials = tempfile::tempdir().context("creating temporary credentials")?;
        if env::var_os(key).is_some_and(|v| !v.is_empty()) {
            // Pass the name only; never put secret values in arguments or diagnostics.
            command.args(["--env", key]);
        } else if let Some(auth) = auth.filter(|p| p.is_file()) {
            let copy = credentials.path().join("auth.json");
            fs::copy(&auth, &copy).context("preparing container credentials")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&copy, fs::Permissions::from_mode(0o644))?;
            }
            command
                .arg("--mount")
                .arg(mount(&copy.canonicalize()?, target)?);
        } else {
            bail!(
                "{} debate authentication is missing; set {key} or log in locally first",
                decision.runtime
            );
        }
        command.arg(&id).args(args);
        let prompt = format!("Repository evidence is available at /workspace (read-only). Inspect it when relevant.\n{prompt}");
        let result = conversation::run_command(
            decision,
            &prompt,
            &mut command,
            output,
            cancel,
            Some(Duration::from_secs(300)),
        );
        let cleaned = cleanup.finish();
        cleaned.context("could not confirm isolated container cleanup")?;
        result
    }

    fn control(&self, args: &[&str], timeout: Duration) -> Result<String> {
        capture(Command::new(&self.docker).args(args), timeout)
    }
}

fn unique_name() -> String {
    format!(
        "nectra-debate-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn mount(path: &Path, destination: &str) -> Result<String> {
    let path = path
        .to_str()
        .context("container path must be valid UTF-8")?;
    // Docker's --mount is CSV; reject ambiguous host paths rather than misparse.
    if path.contains([',', '\n', '\r', '"']) {
        bail!("container path contains unsupported mount characters");
    }
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    Ok(format!(
        "type=bind,source={path},target={destination},readonly,bind-recursive=disabled"
    ))
}

fn container_args(workspace: &Path, _image: &str, name: &str) -> Result<Vec<OsString>> {
    Ok(vec![
        "run".into(),
        "--rm".into(),
        "--interactive".into(),
        "--name".into(),
        name.into(),
        "--read-only".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--user=10001:10001".into(),
        "--pids-limit=128".into(),
        "--memory=2g".into(),
        "--cpus=2".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=256m".into(),
        "--tmpfs=/home/agent:rw,nosuid,nodev,uid=10001,gid=10001,size=256m".into(),
        "--env=HOME=/home/agent".into(),
        "--workdir=/home/agent".into(),
        "--mount".into(),
        mount(workspace, "/workspace")?.into(),
    ])
}

fn provider_args(d: &Decision) -> Result<Vec<String>> {
    if d.model.starts_with('-') || d.model.is_empty() {
        bail!("invalid debate model");
    }
    let mut args: Vec<String> = match d.runtime.as_str() {
        "claude" => [
            "claude",
            "-p",
            "--restricted",
            "--safe-mode",
            "--tools",
            "Read,Glob,Grep",
            "--add-dir",
            "/workspace",
            "--strict-mcp-config",
            "--permission-prompts",
            "none",
            "--permission-mode",
            "manual",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
        ]
        .map(String::from)
        .to_vec(),
        "codex" => [
            "codex",
            "--ask-for-approval",
            "never",
            "exec",
            "--ignore-user-config",
            "--ignore-rules",
            "--sandbox",
            "read-only",
            "--skip-git-repo-check",
            "--json",
            "--ephemeral",
            "--model",
        ]
        .map(String::from)
        .to_vec(),
        _ => bail!("runtime is not supported by the isolated debate adapter"),
    };
    args.push(d.model.clone());
    if !d.effort.is_empty() {
        if d.runtime == "claude" {
            args.extend(["--effort".into(), d.effort.clone()]);
        } else {
            args.extend([
                "-c".into(),
                format!(
                    "model_reasoning_effort={}",
                    serde_json::to_string(&d.effort)?
                ),
            ]);
        }
    }
    if d.runtime == "codex" {
        args.extend(["--".into(), "-".into()]);
    }
    Ok(args)
}

fn pinned_command(docker: &Path, endpoint: &str) -> Command {
    let mut command = Command::new(docker);
    command
        .args(["--host", endpoint])
        .env_remove("DOCKER_CONTEXT")
        .env_remove("DOCKER_HOST");
    command
}

struct ContainerCleanup {
    docker: PathBuf,
    endpoint: String,
    name: String,
    finished: bool,
}
impl ContainerCleanup {
    fn new(docker: PathBuf, endpoint: String, name: String) -> Self {
        Self {
            docker,
            endpoint,
            name,
            finished: false,
        }
    }
    fn finish(&mut self) -> Result<()> {
        let result = capture(
            pinned_command(&self.docker, &self.endpoint).args(["rm", "--force", &self.name]),
            Duration::from_secs(10),
        );
        match result {
            Ok(_) => {
                self.finished = true;
                Ok(())
            }
            Err(e) if e.to_string().contains("No such container") => {
                self.finished = true;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}
impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish();
        }
    }
}

fn capture(command: &mut Command, timeout: Duration) -> Result<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::OwnedChild::spawn(command)?;
    let stdout = child
        .child
        .stdout
        .take()
        .context("opening command output")?;
    let stderr = child.child.stderr.take().context("opening command error")?;
    let out = thread::spawn(move || {
        let mut s = String::new();
        stdout.take(1024 * 1024).read_to_string(&mut s).map(|_| s)
    });
    let err = thread::spawn(move || {
        let mut s = String::new();
        stderr.take(1024 * 1024).read_to_string(&mut s).map(|_| s)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() >= timeout {
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    child.stop();
    let deadline = Instant::now() + Duration::from_secs(2);
    let stdout = process::join_until(out, deadline)??;
    let stderr = process::join_until(err, deadline)??;
    let status = status.context("container control command timed out")?;
    if !status.success() {
        bail!("container control command failed: {}", stderr.trim());
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decision(runtime: &str) -> Decision {
        Decision {
            task: "general".into(),
            provider: "test".into(),
            runtime: runtime.into(),
            model: "test-model".into(),
            effort: String::new(),
            mode: crate::cli::Mode::Balanced,
            score: 1.,
            score_margin: None,
            ambiguous: false,
            overridden: false,
            reason: String::new(),
            candidates: vec![],
        }
    }
    #[test]
    fn isolation_has_no_writable_host_mount_or_privileged_runtime() {
        let args = container_args(Path::new("/project"), "image", "fixture").unwrap();
        let args: Vec<_> = args.iter().map(|s| s.to_string_lossy()).collect();
        assert!(args.iter().any(|s| *s == "--read-only"));
        assert!(args.iter().any(|s| *s == "--cap-drop=ALL"));
        assert!(args
            .iter()
            .any(|s| *s == "--security-opt=no-new-privileges"));
        let mounts: Vec<_> = args.windows(2).filter(|p| p[0] == "--mount").collect();
        assert_eq!(mounts.len(), 1);
        assert!(mounts[0][1].ends_with(",readonly,bind-recursive=disabled"));
        assert!(!args.iter().any(|s| s.contains("docker.sock")
            || s.contains("privileged")
            || *s == "--network=host"));
    }
    #[test]
    fn provider_profiles_never_use_host_bypass_flags_or_project_cwd() {
        let claude = provider_args(&decision("claude")).unwrap();
        assert!(claude.contains(&"--safe-mode".into()));
        assert!(claude.contains(&"--strict-mcp-config".into()));
        assert!(claude.contains(&"Read,Glob,Grep".into()));
        let codex = provider_args(&decision("codex")).unwrap();
        assert!(codex.contains(&"read-only".into()));
        assert!(codex.contains(&"--ignore-user-config".into()));
        assert_eq!(&codex[codex.len() - 2..], &["--", "-"]);
        assert!(provider_args(&decision("gemini")).is_err());
        assert!(provider_args(&decision("ollama")).is_err());
        assert!(mount(Path::new("/a,b"), "/workspace").is_err());
    }
    #[test]
    fn failed_or_missing_preflight_cannot_launch_any_provider() {
        let executor = IsolatedExecutor {
            docker: "this-must-never-run".into(),
            workspace: "/project".into(),
            image: "fixture".into(),
            verified_image: Arc::new(Mutex::new(None)),
        };
        let err = executor
            .respond(
                &decision("codex"),
                "test",
                &mut Vec::new(),
                &CancellationToken::default(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("preflight must succeed"));
    }
}
