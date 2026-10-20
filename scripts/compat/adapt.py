"""Keep Nectra in step with the provider CLIs: detect, fix what is mechanical, report the rest.

    python scripts/compat/adapt.py [--live] [--nectra target/release/nectra]

One run observes the installed CLIs (versions, help text, model lists and, with
--live, one tiny raw stream-json turn each), compares that with the registry and
the previous run, then:
  - puts mechanical fixes (flag rename, new/retired model) on a local branch
    compat/auto-<hash> made in a temporary worktree, with the tests run there;
  - applies the registry part to the installed models.toml, with a backup and diff;
  - writes an issue for everything else;
  - calls tools/laya/retrain when models, roles or task types changed, or records
    "retrain needed" in state.json when it cannot.
Never merges or pushes. Design: docs/superpowers/specs/2026-10-20-cli-auto-adapt.md
"""
import argparse, datetime, difflib, hashlib, json, os, re, shutil, subprocess, sys, tempfile, tomllib
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check  # noqa: E402  (FLAGS, CHEAP, run)

ROOT = check.ROOT
QUESTION = check.QUESTION
TODAY = datetime.date.today().isoformat()
# Flags that turn off a provider's own safety checks are never renamed automatically.
SAFETY = re.compile(r"dangerous|bypass|skip")
# The stream-json fields Nectra reads (src/executor/conversation.rs), per runtime.
STREAM_NEEDS = {
    "claude": {"result": ["result", "subtype"]},
    "codex": {"item.completed": ["item.type", "item.text"], "turn.completed": []},
    "gemini": {"message": ["role", "content"], "result": ["status"]},
}
RUNTIME = {"anthropic": "claude", "openai": "codex", "google": "gemini"}


def stream_cmd(runtime, model):
    return {
        "claude": ["claude", "-p", "--model", model, "--output-format", "stream-json", "--verbose",
                   "--no-session-persistence", QUESTION],
        "codex": ["codex", "exec", "--model", model, "--json", "--ephemeral", "--sandbox", "read-only", QUESTION],
        "gemini": ["gemini", "-m", model, "-o", "stream-json", "-p", QUESTION],
    }[runtime]


# ---- observe ------------------------------------------------------------------------
def claude_aliases(help_text):
    """The aliases `claude --help` names under --model, e.g. 'fable', 'opus'."""
    m = re.search(r"--model <model>(.*?)(?:\n\s+-|\Z)", help_text, re.S)
    return sorted(set(re.findall(r"'([a-z][a-z0-9.-]*)'", m.group(1)))) if m else []


def gemini_bundle():
    exe = shutil.which("gemini")
    if not exe:
        return None
    for start in {os.path.dirname(exe), os.path.dirname(os.path.realpath(exe))}:
        d = start
        for _ in range(6):
            for cand in (os.path.join(d, "node_modules", "@google", "gemini-cli", "bundle"), os.path.join(d, "bundle")):
                if os.path.isdir(cand) and os.path.exists(os.path.join(os.path.dirname(cand), "package.json")):
                    return cand
            d = os.path.dirname(d)
    return None


def gemini_models(bundle_text):
    """Chat model ids the Gemini CLI declares as *_MODEL constants, and every id it mentions."""
    declared = set()
    for name, value in re.findall(r'\b([A-Z_]*MODEL) = "(gemini-[^"]+)"', bundle_text):
        if not re.search(r"EMBEDDING|VISUAL|CUSTOM|LEGACY", name) and not re.search(r"customtools|image|live|computer", value):
            declared.add(value)
    return sorted(declared), set(re.findall(r'"(gemini-[0-9][\w.-]*)"', bundle_text))


def codex_catalog():
    code, out = check.run(["codex", "debug", "models"], timeout=60)
    try:
        models = json.JSONDecoder().raw_decode(out, out.index("{"))[0]["models"]
    except (ValueError, KeyError):
        return None
    return {m["slug"]: m.get("visibility", "list") for m in models}


def ollama_tags():
    host = os.environ.get("OLLAMA_HOST", "127.0.0.1:11434")
    host = host if host.startswith("http") else "http://" + host
    try:
        with urllib.request.urlopen(host.rstrip("/") + "/api/tags", timeout=3) as r:
            return sorted(m["name"] for m in json.load(r)["models"])
    except Exception:
        return None


def stream_events(text):
    events = []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("{"):
            try:
                events.append(json.loads(line))
            except ValueError:
                pass
    return events


def observe(providers, registry_text, state, live, work):
    obs = {"versions": {}, "help": {}, "models": {}, "streams": {}, "stream_tail": {}, "probes": {}, "ollama": None}
    for p in providers:
        cli = check.CLI[p]
        code, out = check.run([cli, "--version"])
        obs["versions"][cli] = out.strip().splitlines()[0] if code == 0 and out.strip() else None
    for cmd in check.FLAGS:
        if check.CLI_OF.get(cmd[0]) in providers:
            obs["help"][" ".join(cmd)] = check.run(list(cmd))[1]
    if "anthropic" in providers:
        obs["models"]["anthropic"] = {"listed": claude_aliases(obs["help"].get("claude --help", ""))}
    if "openai" in providers:
        obs["models"]["openai"] = {"catalog": codex_catalog()}
    if "google" in providers:
        bundle = gemini_bundle()
        if bundle:
            text = "".join(open(os.path.join(bundle, f), encoding="utf-8", errors="replace").read()
                           for f in os.listdir(bundle) if f.endswith(".js"))
            declared, mentioned = gemini_models(text)
            obs["models"]["google"] = {"listed": declared, "mentioned": sorted(mentioned)}
    obs["ollama"] = ollama_tags()
    if live:
        for p in providers:
            rt = RUNTIME[p]
            out = check.run(stream_cmd(rt, check.CHEAP[p]), timeout=300, cwd=work)[1]
            obs["streams"][rt] = stream_events(out)
            obs["stream_tail"][rt] = check.last_lines(out, 2)[:300]
        # Lists that aren't account-specific are verified once per new id, cheaply, and
        # only when the provider answered at all: a signed-out CLI proves nothing.
        known = registry_ids(registry_text)
        for p in ("anthropic", "google"):
            if p not in providers or '"42' not in json.dumps(obs["streams"].get(RUNTIME[p], [])):
                continue
            for mid in obs["models"].get(p, {}).get("listed", []):
                key, old = f"{p}/{mid}", state.get("probes", {}).get(f"{p}/{mid}")
                # A failed probe is retried after a week, in case access was granted.
                if mid in known or (old and (old["ok"] or (datetime.date.today() - datetime.date.fromisoformat(old["date"])).days < 7)):
                    continue
                code, out = check.run(stream_cmd(RUNTIME[p], mid), timeout=300, cwd=work)
                obs["probes"][key] = {"ok": code == 0 and "42" in out, "date": TODAY,
                                      "detail": check.last_lines(out, 2)[:300]}
    return obs


# ---- registry -----------------------------------------------------------------------
def registry_models(text):
    return tomllib.loads(text).get("model", [])


def registry_ids(text):
    return {m["id"] for m in registry_models(text)}


def validate_registry(text):
    models = registry_models(text)
    ids = [m["id"] for m in models]
    need = {"id", "provider", "runtime", "enabled", "capabilities"}
    if not models or len(ids) != len(set(ids)) or any(need - m.keys() for m in models):
        raise ValueError("edited registry is not valid")


def add_model(text, mid, provider, source):
    same = [m for m in registry_models(text) if m["provider"] == provider]
    if not same:
        raise ValueError(f"no {provider} model to copy scores from")
    tmpl = max(same, key=lambda m: difflib.SequenceMatcher(None, m["id"], mid).ratio())
    caps = "\n".join(f"{k}={v}" for k, v in tmpl["capabilities"].items())
    block = (f"\n# Discovered {TODAY} by scripts/compat/adapt.py ({source}). Scores copied from\n"
             f"# {tmpl['id']}: review them and the role, then enable.\n"
             f'[[model]]\nid = "{mid}"\nprovider = "{provider}"\nruntime = "{tmpl["runtime"]}"\n'
             f'enabled = false\neffort = "{tmpl.get("effort", "")}"\nprefers = []\n[model.capabilities]\n{caps}\n')
    return text.rstrip("\n") + "\n" + block


def disable_model(text, mid):
    parts = re.split(r"(?m)^(?=\[\[model\]\])", text)
    for i, part in enumerate(parts):
        if re.search(rf'(?m)^id\s*=\s*"{re.escape(mid)}"\s*$', part):
            parts[i] = re.sub(r"(?m)^enabled\s*=\s*true\b", "enabled = false", part, count=1)
    return "".join(parts)


def edit_registry(text, fixes):
    for f in fixes:
        if f["kind"] == "model-new" and f["id"] not in registry_ids(text):
            text = add_model(text, f["id"], f["provider"], f["source"])
        elif f["kind"] == "model-retired":
            text = disable_model(text, f["id"])
    validate_registry(text)
    return text


# ---- analyse ------------------------------------------------------------------------
def flags_in(text):
    return set(re.findall(r"(?<![\w-])(--?[A-Za-z][\w-]*)", text))


def field(event, path):
    for key in path.split("."):
        if not isinstance(event, dict) or key not in event:
            return False
        event = event[key]
    return True


def analyse(obs, prev, registry_text):
    """Findings from one observation. Mechanical ones carry `fix`; the rest are issues."""
    out = []
    for cli, v in obs["versions"].items():
        old = prev.get("versions", {}).get(cli)
        if v is None:
            out.append({"kind": "missing-cli", "detail": f"{cli} is not installed or does not start"})
        elif old and old != v:
            out.append({"kind": "version", "detail": f"{cli}: {old} -> {v}"})
    for cmd, flags in check.FLAGS.items():
        key = " ".join(cmd)
        if key not in obs["help"]:
            continue
        now, before = flags_in(obs["help"][key]), set(prev.get("help_flags", {}).get(key, []))
        for flag in flags:
            if re.search(rf"(?<![\w-]){re.escape(flag)}(?![\w-])", obs["help"][key]):
                continue
            added = sorted(now - before) if before else []
            close = difflib.get_close_matches(flag, added, n=2, cutoff=0.6) if flag.startswith("-") else []
            if len(close) == 1 and not SAFETY.search(flag):
                out.append({"kind": "flag", "fix": True, "old": flag, "new": close[0],
                            "detail": f"`{key}` renamed {flag} to {close[0]}"})
            else:
                why = "it controls a provider safety check" if SAFETY.search(flag) else "no single new flag replaces it"
                out.append({"kind": "flag", "detail": f"`{key}` no longer lists {flag} ({why})"})
    for rt, events in obs["streams"].items():
        if not events:
            out.append({"kind": "stream", "detail": f"{rt} printed no stream-json events: {obs.get('stream_tail', {}).get(rt, '')}"})
            continue
        for kind, paths in STREAM_NEEDS[rt].items():
            matching = [e for e in events if e.get("type") == kind]
            if rt == "codex" and kind == "item.completed":
                matching = [e for e in matching if e.get("item", {}).get("type") == "agent_message"]
            if not matching:
                out.append({"kind": "stream", "detail": f"{rt} stream has no `{kind}` event; Nectra reads its answer from it"})
                continue
            gone = [p for p in paths if not any(field(e, p) for e in matching)]
            if gone:
                out.append({"kind": "stream", "detail": f"{rt} `{kind}` event lost {gone}; Nectra reads them"})
    models = registry_models(registry_text)
    have = {m["id"]: m for m in models}
    for provider, found in obs["models"].items():
        mine = [m["id"] for m in models if m["provider"] == provider]
        if provider == "openai":
            catalog = found.get("catalog")
            if catalog is None:
                out.append({"kind": "discovery", "detail": "`codex debug models` returned no catalog"})
                continue
            for mid, vis in catalog.items():
                if vis == "list" and mid not in have:
                    out.append({"kind": "model-new", "fix": True, "provider": provider, "id": mid,
                                "source": "codex debug models", "detail": f"Codex offers {mid} to this account"})
            for mid in mine:
                if mid not in catalog and have[mid]["enabled"]:
                    out.append({"kind": "model-retired", "fix": True, "provider": provider, "id": mid,
                                "detail": f"{mid} is no longer in this account's Codex catalog"})
            continue
        source = "claude --help" if provider == "anthropic" else "Gemini CLI bundle"
        for mid in found.get("listed", []):
            if mid in have:
                continue
            probe = obs["probes"].get(f"{provider}/{mid}") or prev.get("probes", {}).get(f"{provider}/{mid}")
            if probe and probe["ok"]:
                out.append({"kind": "model-new", "fix": True, "provider": provider, "id": mid, "source": source,
                            "detail": f"{source} names {mid}; a tiny turn on it answered"})
            elif probe:
                out.append({"kind": "model-unavailable", "detail": f"{source} names {mid}, but this account cannot use it: {probe['detail']}"})
            else:
                out.append({"kind": "model-candidate", "detail": f"{source} names {mid}; run with --live to verify it"})
        if provider == "google":
            for mid in mine:
                if mid not in found.get("mentioned", []) and have[mid]["enabled"]:
                    out.append({"kind": "model-retired", "provider": provider, "id": mid,
                                "detail": f"the Gemini CLI no longer mentions {mid}; check before disabling it"})
    if obs.get("ollama") is not None:
        configured = set(obs.get("ollama_configured", []))
        pulled = set(obs["ollama"])
        if pulled - configured:
            out.append({"kind": "ollama", "detail": f"pulled but not in config.toml [ollama]: {sorted(pulled - configured)}"})
        if configured - pulled:
            out.append({"kind": "ollama", "detail": f"configured but not pulled: {sorted(configured - pulled)}"})
    return out


# ---- fixes --------------------------------------------------------------------------
def git(*args, cwd=ROOT):
    p = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    return p.returncode, (p.stdout + p.stderr).strip()


def rename_flag(root, old, new):
    """Replace the flag literal in Nectra's sources and in check.py's flag list."""
    touched = []
    paths = [os.path.join(d, f) for d, _, fs in os.walk(os.path.join(root, "src")) for f in fs if f.endswith(".rs")]
    for path in paths + [os.path.join(root, "scripts", "compat", "check.py")]:
        text = open(path, encoding="utf-8").read()
        if f'"{old}"' in text:
            with open(path, "w", encoding="utf-8", newline="") as fh:
                fh.write(text.replace(f'"{old}"', f'"{new}"'))
            touched.append(os.path.relpath(path, root).replace("\\", "/"))
    return touched


def sources_with(root, literal):
    paths = [os.path.join(d, f) for d, _, fs in os.walk(os.path.join(root, "src")) for f in fs if f.endswith(".rs")]
    return [p for p in paths if f'"{literal}"' in open(p, encoding="utf-8").read()]


def make_branch(repo, base, fixes, test_cmd, target_dir):
    """Apply the fixes on compat/auto-<hash> in a throwaway worktree. Returns a summary dict."""
    digest = hashlib.sha256(json.dumps([f["detail"] for f in fixes], sort_keys=True).encode()).hexdigest()[:8]
    branch = f"compat/auto-{digest}"
    if git("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=repo)[0] == 0:
        return {"branch": branch, "skipped": "branch already exists"}
    wt = tempfile.mkdtemp(prefix="nectra-compat-fix-")
    os.rmdir(wt)
    code, out = git("worktree", "add", "-q", "-b", branch, wt, base, cwd=repo)
    if code:
        return {"branch": branch, "error": out}
    try:
        touched = []
        for f in fixes:
            if f["kind"] == "flag":
                touched += rename_flag(wt, f["old"], f["new"])
        reg = os.path.join(wt, "config", "models.toml")
        text = open(reg, encoding="utf-8").read()
        new = edit_registry(text, [f for f in fixes if f["kind"].startswith("model-")])
        if new != text:
            with open(reg, "w", encoding="utf-8", newline="") as fh:
                fh.write(new)
            touched.append("config/models.toml")
        if not touched:
            git("worktree", "remove", "--force", wt, cwd=repo)
            git("branch", "-D", branch, cwd=repo)
            return {"branch": branch, "skipped": "nothing to change on " + base}
        env = {**os.environ, "CARGO_TARGET_DIR": target_dir} if target_dir else None
        t = subprocess.run(test_cmd, shell=True, cwd=wt, capture_output=True, text=True, encoding="utf-8",
                           errors="replace", env=env)
        passed = t.returncode == 0
        tail = check.last_lines(t.stdout + t.stderr, 6)
        msg = ("Compat: adapt to provider CLI changes\n\n" + "\n".join(f"- {f['detail']}" for f in fixes)
               + f"\n\nTests ({test_cmd}): {'passed' if passed else 'FAILED'}\nMade by scripts/compat/adapt.py; review before merging.\n")
        git("add", "-A", cwd=wt)
        code, out = git("commit", "-q", "-m", msg, cwd=wt)
        return {"branch": branch, "files": sorted(set(touched)), "tests_passed": passed, "test_tail": tail,
                **({"error": out} if code else {})}
    finally:
        git("worktree", "remove", "--force", wt, cwd=repo)


def update_installed(path, fixes, report):
    """Apply registry fixes to the installed models.toml; backup and diff first."""
    reg = [f for f in fixes if f["kind"].startswith("model-")]
    if not reg or not path or not os.path.exists(path):
        return None
    text = open(path, encoding="utf-8").read()
    new = edit_registry(text, reg)
    if new == text:
        return None
    backup = f"{path}.bak-{TODAY}"
    if not os.path.exists(backup):
        shutil.copy2(path, backup)
    diff = "".join(difflib.unified_diff(text.splitlines(True), new.splitlines(True), path, path + " (new)"))
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8", newline="") as fh:
        fh.write(new)
    os.replace(tmp, path)
    report["installed"] = {"path": path, "backup": backup, "diff": diff}
    return diff


# ---- Laya retrain -------------------------------------------------------------------
def fingerprint(registry_text, repo):
    """What Laya learns from: enabled models and their roles, the role table, the task types."""
    roster = sorted((m["id"], tuple(m.get("prefers", []))) for m in registry_models(registry_text) if m["enabled"])
    parts = [json.dumps(roster)]
    for rel, pattern in (("src/policy.rs", r"pub const ROLES.*?\];"), ("src/classifier.rs", r"pub enum TaskType \{.*?\}")):
        try:
            m = re.search(pattern, open(os.path.join(repo, rel), encoding="utf-8").read(), re.S)
            parts.append(m.group(0) if m else "")
        except OSError:
            parts.append("")
    return hashlib.sha256("\n".join(parts).encode()).hexdigest()[:16]


def retrain_tool(repo):
    base = os.path.join(repo, "tools", "laya", "retrain")
    for ext in ("", ".py", ".ps1", ".sh", ".cmd"):
        if os.path.isfile(base + ext):
            return base + ext
    return None


def maybe_retrain(state, fp, reason, repo):
    pending = state.get("retrain", {})
    if state.get("fingerprint") == fp and not pending.get("needed"):
        return {"status": "not needed"}
    reason = reason if state.get("fingerprint") != fp else pending.get("reason", reason)
    tool = retrain_tool(repo)
    result = {"status": "retrain needed", "reason": reason}
    if tool:
        cmd = {".py": [sys.executable, tool], ".ps1": ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", tool],
               ".sh": ["sh", tool]}.get(os.path.splitext(tool)[1], [tool])
        p = subprocess.run(cmd, cwd=repo, capture_output=True, text=True, encoding="utf-8", errors="replace",
                           env={**os.environ, "NECTRA_RETRAIN_REASON": reason, "NECTRA_RETRAIN_FINGERPRINT": fp})
        result.update(tool=os.path.relpath(tool, repo), exit=p.returncode, output=check.last_lines(p.stdout + p.stderr))
        if p.returncode == 0:
            result["status"] = "retrain triggered"
            state["retrain"] = {"needed": False, "last": TODAY, "reason": reason}
            state["fingerprint"] = fp
            return result
    else:
        result["tool"] = "tools/laya/retrain is absent"
    state["retrain"] = {"needed": True, "reason": reason, "since": pending.get("since", TODAY)}
    state["fingerprint"] = fp
    return result


# ---- report & main ------------------------------------------------------------------
def write_report(path, findings, rep):
    fixes = [f for f in findings if f.get("fix")]
    quiet = ("version", "ollama", "model-candidate", "model-unavailable")
    issues = [f for f in findings if not f.get("fix") and f["kind"] not in quiet]
    notes = [f for f in findings if f["kind"] in quiet]
    lines = [f"# Nectra CLI compatibility: {TODAY}", "", "## Observed", ""]
    lines += [f"- {cli}: {v or 'not found'}" for cli, v in rep["versions"].items()]
    lines += [f"- {p} models: {', '.join(ids) or '(none)'}" for p, ids in rep["models"].items()]
    lines += ["", "## Mechanical fixes", ""] + ([f"- {f['detail']}" for f in fixes] or ["None."])
    if rep.get("branch"):
        b = rep["branch"]
        lines += ["", f"Branch `{b['branch']}`: " + (b.get("skipped") or b.get("error") or
                  f"tests {'passed' if b['tests_passed'] else 'FAILED'}; files {', '.join(b['files'])}")]
        if b.get("test_tail"):
            lines += ["", "```", b["test_tail"], "```"]
    if rep.get("installed"):
        i = rep["installed"]
        lines += ["", f"Installed registry `{i['path']}` updated; backup `{i['backup']}`.", "", "```diff", i["diff"].rstrip(), "```"]
    lines += ["", "## Issues (need a person)", ""] + ([f"- **{f['kind']}**: {f['detail']}" for f in issues] or ["None."])
    lines += ["", "## Notes", ""] + ([f"- {f['detail']}" for f in notes] or ["None."])
    if rep.get("check"):
        lines += ["", "## check.py", "", "```", rep["check"].rstrip(), "```"]
    r = rep["retrain"]
    lines += ["", "## Laya retrain", "", f"{r['status']}" + (f": {r['reason']}" if r.get("reason") else "")
              + (f" ({r['tool']}" + (f", exit {r['exit']}" if "exit" in r else "") + ")" if r.get("tool") else "")]
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")
    return issues


def default_dirs():
    if os.name == "nt":
        return (os.path.join(os.environ["LOCALAPPDATA"], "nectra", "compat"),
                os.path.join(os.environ["APPDATA"], "nectra", "config.toml"))
    state = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
    conf = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return os.path.join(state, "nectra", "compat"), os.path.join(conf, "nectra", "config.toml")


def main(argv=None):
    state_dir, user_config = default_dirs()
    ap = argparse.ArgumentParser()
    ap.add_argument("--providers", default="anthropic,openai,google")
    ap.add_argument("--live", action="store_true", help="raw stream turns and one probe per new model")
    ap.add_argument("--nectra", help="also run check.py against this binary")
    ap.add_argument("--repo", default=ROOT)
    ap.add_argument("--base", default="master")
    # One thread: the ConPTY terminal tests flake when run alongside each other.
    ap.add_argument("--test-cmd", default="cargo test --all-targets --quiet -- --test-threads=1")
    ap.add_argument("--target-dir", help="CARGO_TARGET_DIR for the fix branch (default: <state>/target)")
    ap.add_argument("--state-dir", default=state_dir)
    ap.add_argument("--config", default=user_config, help="installed config.toml; 'none' skips the installed registry")
    ap.add_argument("--fixture", help="observation JSON to use instead of the real CLIs (tests, drills)")
    args = ap.parse_args(argv)
    os.makedirs(args.state_dir, exist_ok=True)
    state_path = os.path.join(args.state_dir, "state.json")
    state = json.load(open(state_path, encoding="utf-8")) if os.path.exists(state_path) else {}
    providers = [p for p in args.providers.split(",") if p in check.CLI]

    installed, configured_ollama = None, []
    if args.config != "none" and os.path.exists(args.config):
        cfg = tomllib.loads(open(args.config, encoding="utf-8").read())
        installed = os.path.join(os.path.dirname(args.config), cfg.get("registry", "models.toml"))
        configured_ollama = [m["name"] for m in cfg.get("ollama", {}).get("models", [])]
    bundled = open(os.path.join(args.repo, "config", "models.toml"), encoding="utf-8").read()
    routing = open(installed, encoding="utf-8").read() if installed and os.path.exists(installed) else bundled

    if args.fixture:
        obs = json.load(open(args.fixture, encoding="utf-8"))
        prev = obs.pop("previous_state", state)
    else:
        work = os.path.join(tempfile.gettempdir(), "nectra-compat-work")
        os.makedirs(work, exist_ok=True)
        if not os.path.isdir(os.path.join(work, ".git")):
            subprocess.run(["git", "init", "-q", work], check=False)
        obs, prev = observe(providers, bundled, state, args.live, work), state
    obs.setdefault("ollama_configured", configured_ollama)

    findings = analyse(obs, prev, bundled)
    # The installed copy can lag the bundled one; its own drift counts too.
    if routing != bundled:
        seen = {f["detail"] for f in findings}
        findings += [f for f in analyse({**obs, "streams": {}, "help": {}, "versions": {}}, prev, routing)
                     if f["detail"] not in seen and f["kind"].startswith("model-")]
    fixes = [f for f in findings if f.get("fix")]
    rep = {"versions": obs["versions"],
           "models": {p: sorted(m.get("catalog") or m.get("listed") or []) for p, m in obs["models"].items()}}
    if obs.get("ollama") is not None:
        rep["models"]["ollama"] = obs["ollama"]
    if args.nectra:
        p = subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "check.py"), "--nectra", args.nectra,
                            "--providers", ",".join(providers)] + (["--live"] if args.live else []),
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
        rep["check"] = p.stdout + p.stderr
        findings += [{"kind": "check", "detail": l[7:]} for l in rep["check"].splitlines() if l.startswith("[FAIL]")]
    # A rename is only mechanical if Nectra passes the old flag as a literal.
    for f in fixes:
        if f["kind"] == "flag" and not sources_with(args.repo, f["old"]):
            f.pop("fix")
            f["detail"] += ", but Nectra's sources don't pass it literally"
    fixes = [f for f in fixes if f.get("fix")]
    bundled_fixes = [f for f in fixes if f["kind"] == "flag" or edit_registry(bundled, [f]) != bundled]
    if bundled_fixes:
        rep["branch"] = make_branch(args.repo, args.base, bundled_fixes, args.test_cmd,
                                    args.target_dir or os.path.join(args.state_dir, "target"))
    update_installed(installed, fixes, rep)

    # The first run has nothing to compare with, so it compares with the registry before its own edits.
    state.setdefault("fingerprint", fingerprint(routing, args.repo))
    after = open(installed, encoding="utf-8").read() if installed and os.path.exists(installed) else bundled
    changed = [f["detail"] for f in fixes if f["kind"].startswith("model-")]
    rep["retrain"] = maybe_retrain(state, fingerprint(after, args.repo),
                                   "; ".join(changed) or "models, roles or task types changed", args.repo)

    state.update(versions={k: v for k, v in obs["versions"].items() if v} or prev.get("versions", {}),
                 help_flags={k: sorted(flags_in(v)) for k, v in obs["help"].items()} or prev.get("help_flags", {}),
                 probes={**state.get("probes", {}), **obs["probes"]}, last_run=TODAY)
    tmp = state_path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(state, fh, indent=1)
    os.replace(tmp, state_path)
    report = os.path.join(args.state_dir, f"report-{TODAY}.md")
    issues = write_report(report, findings, rep)
    print(open(report, encoding="utf-8").read())
    print(f"Report: {report}")
    return 1 if issues or (rep.get("branch") or {}).get("tests_passed") is False else 0


if __name__ == "__main__":
    sys.exit(main())
