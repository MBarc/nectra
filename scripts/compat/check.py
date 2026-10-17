"""Check Nectra against the provider CLIs installed on this machine.

Provider CLIs change without notice, and Nectra relies on their flags, settings
files and terminal behaviour. Run daily against the latest releases:

    python scripts/compat/check.py --nectra target/release/nectra [--live]

Contract checks make no model calls. --live also runs one tiny turn per
provider in stream view and in terminal view (needs each CLI signed in, or API
keys in CI). Exits non-zero when anything fails.
"""
import argparse, json, os, re, shutil, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CHEAP = {"anthropic": "haiku", "openai": "gpt-5.6-luna", "google": "gemini-2.5-flash"}
CLI = {"anthropic": "claude", "openai": "codex", "google": "gemini"}
# Flags Nectra passes (src/executor, src/mcp.rs), by the help text that must list them.
FLAGS = {
    ("claude", "--help"): ["-p", "--model", "--output-format", "stream-json", "--effort", "--verbose",
                           "--include-partial-messages", "--no-session-persistence", "--settings",
                           "--session-id", "--resume", "--mcp-config", "--strict-mcp-config", "--tools",
                           "--permission-mode", "--dangerously-skip-permissions"],
    ("codex", "exec", "--help"): ["--model", "--config", "--json", "--ephemeral", "--sandbox",
                                  "--dangerously-bypass-approvals-and-sandbox", "read-only"],
    ("codex", "exec", "resume", "--help"): ["--model", "--config", "--json"],
    ("codex", "--help"): ["--ask-for-approval", "on-request", "--no-alt-screen", "--config"],
    ("gemini", "--help"): ["--model", "--output-format", "stream-json", "--approval-mode"],
}
results = []


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f": {detail}" if detail and not ok else ""), flush=True)


def run(cmd, env=None, timeout=120, stdin=subprocess.DEVNULL, cwd=None):
    # Windows npm CLIs are .cmd shims, which only a resolved path can start.
    cmd = [shutil.which(cmd[0]) or cmd[0], *cmd[1:]]
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace",
                           env={**os.environ, **(env or {})}, timeout=timeout, stdin=stdin, cwd=cwd)
        return p.returncode, p.stdout + p.stderr
    except (OSError, subprocess.TimeoutExpired) as e:
        return -1, str(e)


def last_lines(text, n=4):
    return " | ".join(l.strip() for l in text.strip().splitlines()[-n:])


def config_dir(providers):
    d = tempfile.mkdtemp(prefix="nectra-compat-cfg-")
    shutil.copy(os.path.join(ROOT, "config", "models.toml"), d)
    text = open(os.path.join(ROOT, "config", "nectra.toml"), encoding="utf-8").read()
    text = re.sub(r"(?m)^providers = .*$", "", text)
    with open(os.path.join(d, "config.toml"), "w", encoding="utf-8") as f:
        f.write(f"providers = {json.dumps(providers)}\n" + text)
    return os.path.join(d, "config.toml")


def contract(nectra, providers, cfg):
    code, out = run([nectra, "--config", cfg, "doctor"])
    check("nectra doctor finds every selected CLI", code == 0, last_lines(out))
    for cmd, flags in FLAGS.items():
        if CLI_OF.get(cmd[0]) not in providers:
            continue
        code, out = run(list(cmd))
        missing = [f for f in flags if not re.search(rf"(?<![\w-]){re.escape(f)}(?![\w-])", out)]
        check(f"`{' '.join(cmd)}` still lists Nectra's flags", code == 0 and not missing,
              f"missing {missing}" if missing else last_lines(out))
    probe = "nectra_compat_probe"
    if "openai" in providers:
        code, out = run(["codex", "mcp", "list", "-c", f'mcp_servers.{probe}={{command="nectra-probe"}}'])
        check("Codex takes an MCP server with --config at launch", probe in out, last_lines(out))


# ---- live checks --------------------------------------------------------------------
QUESTION = "What is 6 times 7? Reply with just the number."


def live_stream(nectra, cfg, work, provider):
    model = CHEAP[provider]
    # In the trusted work folder: Codex refuses to run elsewhere outside a git repo.
    code, out = run([nectra, "--config", cfg, "run", "--model", model, "--provider-view", "stream", QUESTION],
                    timeout=300, cwd=work)
    check(f"stream view answers with {model}", code == 0 and "42" in out, last_lines(out))


def live_terminal(nectra, cfg, work, provider):
    model = CHEAP[provider]
    try:
        import pyte
    except ImportError:
        check(f"terminal view answers with {model}", False, "pip install pyte (and pywinpty on Windows)")
        return
    screen = pyte.Screen(120, 40)
    stream = pyte.ByteStream(screen)
    child = Pty([nectra, "--config", cfg, "--provider-view", "terminal", "chat"], work)

    def text():
        return "\n".join(line.rstrip() for line in screen.display)

    def wait(pattern, seconds, answer_prompts=False):
        end = time.time() + seconds
        while time.time() < end:
            stream.feed(child.read())
            shown = text()
            if re.search(pattern, shown, re.M):
                return True
            # First runs in a new folder ask about trust; Nectra shows the prompt.
            if answer_prompts and "WAITING FOR YOU" in shown:
                # Claude's trust prompt defaults to "No, exit"; trusting is below it.
                if "❯ No, exit" in shown:
                    child.write("\x1b[B")
                    time.sleep(0.5)
                child.write("\r")
                time.sleep(3)
            time.sleep(0.3)
        return False

    ok = wait(r"you>", 30)
    if ok:
        child.write(f"/model {model}\r")
        time.sleep(1)
        child.write(QUESTION + "\r")
        # The answer arrives below the turn header, then a fresh prompt.
        ok = wait(rf"{re.escape(model)} \(.*\n(?:.*\n)*?\s*42\b", 300, answer_prompts=True)
    detail = last_lines(text(), 6)
    child.close()
    check(f"terminal view answers with {model}", ok, detail)


class Pty:
    """A child in a pseudo-terminal, read by a thread: reads block until output arrives."""

    def __init__(self, cmd, cwd):
        import threading
        self.buf, self.lock = bytearray(), threading.Lock()
        if os.name == "nt":
            from winpty import PtyProcess
            self.p = PtyProcess.spawn(cmd, cwd=cwd, dimensions=(40, 120))
        else:
            import pty
            self.pid, self.fd = pty.fork()
            if self.pid == 0:
                os.chdir(cwd)
                os.environ["TERM"] = "xterm-256color"
                os.execvp(cmd[0], cmd)
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        while True:
            try:
                if os.name == "nt":
                    data = self.p.read(4096)
                    data = data.encode("utf-8", "surrogatepass") if isinstance(data, str) else data
                else:
                    data = os.read(self.fd, 4096)
            except (EOFError, OSError):
                return
            if not data and os.name != "nt":
                return
            with self.lock:
                self.buf += data

    def read(self):
        with self.lock:
            out = bytes(self.buf)
            self.buf.clear()
        return out

    def write(self, s):
        if os.name == "nt":
            self.p.write(s)
        else:
            os.write(self.fd, s.encode())

    def close(self):
        try:
            if os.name == "nt":
                self.p.terminate(force=True)
            else:
                os.kill(self.pid, 9)
        except Exception:
            pass


CLI_OF = {v: k for k, v in CLI.items()}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nectra", default=os.path.join(ROOT, "target", "release", "nectra"))
    ap.add_argument("--providers", default="anthropic,openai,google")
    ap.add_argument("--live", action="store_true", help="also run one tiny turn per provider")
    args = ap.parse_args()
    args.nectra = os.path.abspath(args.nectra)
    providers = [p for p in args.providers.split(",") if p in CLI]
    cfg = config_dir(providers)
    for provider in providers:
        code, out = run([CLI[provider], "--version"])
        print(f"{CLI[provider]}: {out.strip().splitlines()[0] if out.strip() else 'not found'}")
    contract(args.nectra, providers, cfg)
    if args.live:
        # One stable folder, so first-run trust is asked (and remembered) once.
        work = os.path.join(tempfile.gettempdir(), "nectra-compat-work")
        os.makedirs(work, exist_ok=True)
        if not os.path.isdir(os.path.join(work, ".git")):
            subprocess.run(["git", "init", "-q", work], check=False)
        for provider in providers:
            live_stream(args.nectra, cfg, work, provider)
            # Gemini has no terminal view (src/executor/native.rs `enabled`).
            if provider != "google":
                live_terminal(args.nectra, cfg, work, provider)
    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)} passed, {len(failed)} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
