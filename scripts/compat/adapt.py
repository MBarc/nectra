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


# In check.py, beside FLAGS, so a generated flag rename updates the probes too.
stream_cmd = check.stream_cmd


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


def parse_codex_catalog(code, out):
    """(catalog, error). Only a successful run with a non-empty, well-formed list counts:
    the catalog decides retirements, so a broken one must never look authoritative."""
    if code != 0:
        return None, f"`codex debug models` exited {code}: {check.last_lines(out, 2)[:200]}"
    try:
        models = json.JSONDecoder().raw_decode(out, out.index("{"))[0]["models"]
        entries = [(m["slug"], m.get("visibility", "list")) for m in models]
    except (ValueError, KeyError, TypeError, AttributeError):
        return None, f"`codex debug models` printed no readable catalog: {check.last_lines(out, 2)[:200]}"
    # Every entry, before a dict could let a later duplicate hide a bad one.
    # Types first: set() would crash on an unhashable slug.
    if not entries or not all(isinstance(k, str) and k and isinstance(v, str) for k, v in entries) \
            or len({k for k, _ in entries}) != len(entries):
        return None, "`codex debug models` returned an empty or malformed catalog"
    return dict(entries), None


def codex_catalog():
    return parse_codex_catalog(*check.run(["codex", "debug", "models"], timeout=60))


def ollama_tags():
    """(names, error)."""
    host = os.environ.get("OLLAMA_HOST", "127.0.0.1:11434")
    host = host if host.startswith("http") else "http://" + host
    try:
        with urllib.request.urlopen(host.rstrip("/") + "/api/tags", timeout=3) as r:
            return sorted(m["name"] for m in json.load(r)["models"]), None
    except Exception as e:
        return None, f"Ollama at {host} did not answer: {e}"


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


def observe(providers, registry_text, state, live, work, max_probes=3):
    obs = {"versions": {}, "help": {}, "help_code": {}, "models": {}, "streams": {}, "stream_tail": {}, "probes": {},
           "ollama": None}
    for p in providers:
        cli = check.CLI[p]
        code, out = check.run([cli, "--version"])
        obs["versions"][cli] = out.strip().splitlines()[0] if code == 0 and out.strip() else None
    for cmd in check.FLAGS:
        if check.CLI_OF.get(cmd[0]) in providers:
            key = " ".join(cmd)
            obs["help_code"][key], obs["help"][key] = check.run(list(cmd))
    if "anthropic" in providers:
        listed = claude_aliases(obs["help"].get("claude --help", ""))
        error = (f"`claude --help` exited {obs['help_code'].get('claude --help')}" if obs["help_code"].get("claude --help")
                 else None if listed else "`claude --help` names no model aliases under --model")
        obs["models"]["anthropic"] = {"listed": listed, "error": error}
    if "openai" in providers:
        catalog, error = codex_catalog()
        obs["models"]["openai"] = {"catalog": catalog, "error": error}
    if "google" in providers:
        bundle, found = gemini_bundle(), {"listed": [], "mentioned": [], "error": "Gemini CLI bundle not found"}
        if bundle:
            text = "".join(open(os.path.join(bundle, f), encoding="utf-8", errors="replace").read()
                           for f in os.listdir(bundle) if f.endswith(".js"))
            declared, mentioned = gemini_models(text)
            found = {"listed": declared, "mentioned": sorted(mentioned),
                     "error": None if declared and mentioned else f"no Gemini model constants found in {bundle}"}
        obs["models"]["google"] = found
    obs["ollama"], obs["ollama_error"] = ollama_tags()
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
                if len(obs["probes"]) >= max_probes:
                    break  # the rest wait for the next run, so one burst of new ids stays cheap
                code, out = check.run(stream_cmd(RUNTIME[p], mid), timeout=300, cwd=work)
                obs["probes"][key] = {"ok": code == 0 and "42" in out, "date": TODAY,
                                      "detail": check.last_lines(out, 2)[:300]}
    return obs


# ---- files --------------------------------------------------------------------------
def acquire_lock(path):
    """Hold the lock file `path` for the run. The OS drops it when the process dies, so a
    crashed run never blocks the next one. Returns the open file, or None if held."""
    fh = open(path, "a+")
    try:
        if os.name == "nt":
            import msvcrt
            fh.seek(0)
            msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        fh.close()
        return None
    return fh


def new_file(base, ext):
    """Create `<base><stamp><ext>` exclusively, never reusing a name. Returns its path."""
    stamp = datetime.datetime.now().strftime("%Y-%m-%dT%H%M%S.%f")  # sorts by time
    for n in range(1, 1000):
        path = f"{base}{stamp}{'' if n == 1 else f'-{n}'}{ext}"
        try:
            os.close(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY))
            return path
        except FileExistsError:
            continue
    raise RuntimeError(f"no free name for {base}{stamp}{ext}")


def atomic_write(path, text):
    """Write via a uniquely named temporary file beside `path`, then replace it."""
    fd, tmp = tempfile.mkstemp(prefix=os.path.basename(path) + ".", suffix=".tmp", dir=os.path.dirname(path) or ".")
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as fh:
            fh.write(text)
        os.replace(tmp, path)
    except BaseException:
        if os.path.exists(tmp):
            os.remove(tmp)
        raise


def rename_noreplace(src, dst):
    """Move `src` to `dst`, failing if `dst` exists. os.rename already refuses on
    Windows; elsewhere a hard link does, then the source name goes."""
    if os.name == "nt":
        os.rename(src, dst)
    else:
        os.link(src, dst)
        os.unlink(src)


def rename_back(backup, path, what):
    """Move `backup` back to `path` unless a file has appeared there. Text for the report."""
    try:
        rename_noreplace(backup, path)
        return ""
    except OSError as e:
        # The journal stays while the registry is absent, so the next run tries again.
        return f"; putting it back failed too ({e}), so {what} is in {backup}" + (
            "" if os.path.exists(path) else f" and the next run will try to put it back (journal {path}{SWAP})")


SWAP = ".swap"  # beside the registry: names its backup while the registry is moved aside


def recover_swap(path):
    """Finish what a run killed mid-swap left: the registry missing, its original in the
    backup the journal names. Returns a line for the report, or None."""
    journal = path + SWAP if path else None
    if not journal or not os.path.exists(journal):
        return None
    backup = open(journal, encoding="utf-8").read()
    note = None
    if not os.path.exists(path) and os.path.exists(backup):
        rename_noreplace(backup, path)
        note = f"an earlier run was stopped while replacing {path}; the original was put back from {backup}"
    elif os.path.exists(backup) and not os.path.getsize(backup):
        os.remove(backup)  # reserved, never filled: the earlier run stopped before moving anything
    if os.path.exists(path):
        os.remove(journal)
    return note


# Cargo and training jobs start children; the timeout must hold for all of them.
run_bounded = check.run_bounded
CHECK_TIMEOUT = 3600  # check.py bounds each CLI call itself; this bounds the whole of it


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


MODEL_HEADER = re.compile(r"(?m)^[ \t]*\[\[[ \t]*model[ \t]*\]\][ \t]*(?:#.*)?$")


def disable_model(text, mid):
    """Set `enabled = false` for `mid`, editing only that line, so comments and layout
    stay. The parser locates the model; the result is re-parsed and must differ from the
    original in exactly that one field, or this raises."""
    models = registry_models(text)
    index = next((i for i, m in enumerate(models) if m["id"] == mid), None)
    if index is None:
        raise ValueError(f"cannot disable {mid}: it is not in the registry")
    if not models[index]["enabled"]:
        return text
    heads = list(MODEL_HEADER.finditer(text))
    new = text
    if len(heads) == len(models):
        start = heads[index].end()
        end = heads[index + 1].start() if index + 1 < len(heads) else len(text)
        block = text[start:end]
        sub = re.search(r"(?m)^[ \t]*\[", block)  # the model's own keys end at its first sub-table
        cut = sub.start() if sub else len(block)
        own = re.sub(r"(?m)^([ \t]*enabled[ \t]*=[ \t]*)true\b", r"\1false", block[:cut], count=1)
        new = text[:start] + own + block[cut:] + text[end:]
    expected = [dict(m) for m in models]
    expected[index]["enabled"] = False
    if new == text or registry_models(new) != expected:
        raise ValueError(f"cannot disable {mid}: its `enabled` line is not in a form this script edits safely")
    return new


def emptied(text, fixes):
    """Providers with enabled models in `text` that `fixes` would retire them all from."""
    retired = {f["id"] for f in fixes if f["kind"] == "model-retired" and f.get("fix")}
    enabled = {}
    for m in registry_models(text):
        if m["enabled"]:
            enabled.setdefault(m["provider"], set()).add(m["id"])
    return sorted(p for p, ids in enabled.items() if ids <= retired)


def edit_registry(text, fixes):
    for f in fixes:
        if f["kind"] == "model-new" and f["id"] not in registry_ids(text):
            text = add_model(text, f["id"], f["provider"], f["source"])
        elif f["kind"] == "model-retired" and f["id"] in registry_ids(text):
            # A registry may leave a model out on purpose; one it has must be disabled for real.
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
            # Another subcommand of the same CLI that still lists it shares the literal.
            still = [k for k, fl in check.FLAGS.items() if k != cmd and k[0] == cmd[0] and flag in fl
                     and re.search(rf"(?<![\w-]){re.escape(flag)}(?![\w-])", obs["help"].get(" ".join(k), ""))]
            # A rename changes every occurrence in Nectra, shared code included, so it is only
            # mechanical for a flag no other CLI is known to use: now, last run, or in FLAGS.
            others = sorted({k[0] for k, fl in check.FLAGS.items() if k[0] != cmd[0] and flag in fl}
                            | {k.split()[0] for k, text in obs["help"].items() if k.split()[0] != cmd[0] and flag in flags_in(text)}
                            | {k.split()[0] for k, fl in prev.get("help_flags", {}).items() if k.split()[0] != cmd[0] and flag in fl})
            if len(close) == 1 and not SAFETY.search(flag) and not SAFETY.search(close[0]) and not still and not others:
                out.append({"kind": "flag", "fix": True, "runtime": cmd[0], "old": flag, "new": close[0],
                            "detail": f"`{key}` renamed {flag} to {close[0]}"})
                continue
            if SAFETY.search(flag) or (len(close) == 1 and SAFETY.search(close[0])):
                why = ("it controls a provider safety check" if SAFETY.search(flag)
                       else f"its likely replacement {close[0]} controls a provider safety check")
            elif len(close) == 1 and others:
                why = f"probably now {close[0]}, but {', '.join(others)} also uses {flag}, and Nectra's code may share it"
            elif len(close) == 1:
                why = f"probably now {close[0]}, but `{' '.join(still[0])}` still lists it"
            else:
                why = "no single new flag replaces it"
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
        # Failed discovery proves nothing: never retire (or add) from it.
        if found.get("error") or (provider == "openai" and not found.get("catalog")):
            out.append({"kind": "discovery", "detail": found.get("error") or f"{provider} discovery returned nothing"})
            continue
        if provider == "openai":
            catalog = found["catalog"]
            gone = [mid for mid in mine if mid not in catalog and have[mid]["enabled"]]
            if gone and len(gone) == len([mid for mid in mine if have[mid]["enabled"]]):
                out.append({"kind": "discovery", "detail": f"the Codex catalog lists none of the enabled models {gone}; "
                            "not trusting it to retire them all"})
                continue
            for mid, vis in catalog.items():
                if vis == "list" and mid not in have:
                    out.append({"kind": "model-new", "fix": True, "provider": provider, "id": mid,
                                "source": "codex debug models", "detail": f"Codex offers {mid} to this account"})
            for mid in gone:
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
                out.append({"kind": "model-candidate", "detail": f"{source} names {mid}; not verified (needs --live and a signed-in CLI)"})
        if provider == "google":
            for mid in mine:
                if mid not in found.get("mentioned", []) and have[mid]["enabled"]:
                    out.append({"kind": "model-retired", "provider": provider, "id": mid,
                                "detail": f"the Gemini CLI no longer mentions {mid}; check before disabling it"})
    if obs.get("ollama") is None and obs.get("ollama_error") and obs.get("ollama_configured"):
        out.append({"kind": "discovery", "detail": obs["ollama_error"]})
    if obs.get("ollama") is not None:
        configured = set(obs.get("ollama_configured", []))
        pulled = set(obs["ollama"])
        if pulled - configured:
            out.append({"kind": "ollama", "detail": f"pulled but not in config.toml [ollama]: {sorted(pulled - configured)}"})
        if configured - pulled:
            out.append({"kind": "ollama", "detail": f"configured but not pulled: {sorted(configured - pulled)}"})
    return out


# ---- fixes --------------------------------------------------------------------------
GIT_TIMEOUT = 600


def git(*args, cwd=ROOT, strip=True):
    """(exit code, stdout + stderr), bounded like every other command the run starts."""
    try:
        code, out = run_bounded(["git", *args], GIT_TIMEOUT, cwd=cwd, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return -1, f"git {' '.join(args)} timed out after {GIT_TIMEOUT}s"
    except OSError as e:
        return -1, f"git {' '.join(args)} could not start: {e}"
    return code, out.strip() if strip else out


def git_blob(repo, spec):
    """The exact text of a file in git (`rev:path`), or None. Nothing stripped or translated."""
    code, out = git("cat-file", "blob", spec, cwd=repo, strip=False)
    return out if code == 0 else None


def rename_plan(root, old, new):
    """New text per file, with every occurrence of the flag `old` renamed to `new`: in the
    Rust sources and tests, shared code such as the permissions filter included, and in
    check.py. analyse() proposes a rename only for a flag no other CLI uses, so every
    occurrence is that CLI's."""
    word = re.compile(rf"(?<![\w-]){re.escape(old)}(?![\w-])")
    paths = [os.path.join(d, f) for top in ("src", "tests") for d, _, fs in os.walk(os.path.join(root, top))
             for f in fs if f.endswith(".rs")]
    plan = {}
    for path in paths + [os.path.join(root, "scripts", "compat", "check.py")]:
        text = open(path, encoding="utf-8").read()
        renamed = word.sub(lambda m: new, text)
        if renamed != text:
            plan[path] = renamed
    return plan


def run_tests(wt, test_cmd, target_dir, timeout):
    env = {**os.environ, "CARGO_TARGET_DIR": target_dir} if target_dir else None
    try:
        code, out = run_bounded(test_cmd, timeout, shell=True, cwd=wt, env=env)
        return code == 0, check.last_lines(out, 6)
    except subprocess.TimeoutExpired:
        return False, f"tests timed out after {timeout:g}s"
    except OSError as e:
        return False, f"tests could not start: {e}"


def cleanup(repo, wt, branch=None):
    """Remove the temporary worktree `wt`, and `branch` if given. Returns what could not be."""
    problems = []
    if os.path.exists(wt):
        code, out = git("worktree", "remove", "--force", wt, cwd=repo)
        if code:
            shutil.rmtree(wt, ignore_errors=True)  # a half-made checkout git doesn't know
        if os.path.exists(wt):
            problems.append(f"worktree {wt}: {out}")
    code, out = git("worktree", "prune", cwd=repo)
    if code:
        problems.append(f"git worktree prune: {out}")
    if branch and git("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=repo)[0] == 0:
        code, out = git("branch", "-D", branch, cwd=repo)
        if code:
            problems.append(f"branch {branch}: {out}")
    return problems


def test_registry(repo, base, text, test_cmd, target_dir, timeout):
    """Run the tests on `base` with `text` as its registry, in a throwaway worktree: the
    check an installed change gets when no fix branch tested exactly that text."""
    wt = tempfile.mkdtemp(prefix="nectra-compat-check-")
    os.rmdir(wt)
    result = {}
    try:
        code, out = git("worktree", "add", "-q", "--detach", wt, base, cwd=repo)
        if code:
            result["error"] = f"git worktree add: {out}"
            return result
        with open(os.path.join(wt, "config", "models.toml"), "w", encoding="utf-8", newline="") as fh:
            fh.write(text)
        result["tests_passed"], result["test_tail"] = run_tests(wt, test_cmd, target_dir, timeout)
        return result
    finally:
        if problems := cleanup(repo, wt):
            result["cleanup"] = problems


def make_branch(repo, base, fixes, test_cmd, target_dir, timeout=3600):
    """Apply the fixes on compat/auto-<hash> in a throwaway worktree. Returns a summary dict."""
    code, base_sha = git("rev-parse", "--verify", "--quiet", base + "^{commit}", cwd=repo)
    if code:
        return {"branch": "(not made)", "error": f"base {base} is not a commit"}
    # The name covers the fixes, the exact base commit and the test command, so an earlier
    # result is reused only for the same change tested the same way.
    key = json.dumps({"fixes": [f["detail"] for f in fixes], "base": base_sha, "test": test_cmd}, sort_keys=True)
    branch = f"compat/auto-{hashlib.sha256(key.encode()).hexdigest()[:8]}"
    if git("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=repo)[0] == 0:
        # An earlier run made it; its commit, made on the base, records how its tests went.
        m = re.search(r"^Tests \(.*\): (passed|FAILED)$", git("log", "-1", "--format=%B", branch, cwd=repo)[1], re.M)
        on_base = git("rev-parse", f"{branch}^", cwd=repo) == (0, base_sha)
        return {"branch": branch, "existing": True, "tests_passed": m.group(1) == "passed" if m and on_base else None}
    wt = tempfile.mkdtemp(prefix="nectra-compat-fix-")
    os.rmdir(wt)
    committed, result = False, {"branch": branch}
    try:
        code, out = git("worktree", "add", "-q", "-b", branch, wt, base_sha, cwd=repo)
        if code:  # the branch may exist already, pointing at the base
            result["error"] = f"git worktree add: {out}"
            return result
        touched = []
        for f in fixes:
            if f["kind"] == "flag":
                for path, text in rename_plan(wt, f["old"], f["new"]).items():
                    with open(path, "w", encoding="utf-8", newline="") as fh:
                        fh.write(text)
                    touched.append(os.path.relpath(path, wt).replace("\\", "/"))
        reg = os.path.join(wt, "config", "models.toml")
        text = open(reg, encoding="utf-8").read()
        new = edit_registry(text, [f for f in fixes if f["kind"].startswith("model-")])
        if new != text:
            with open(reg, "w", encoding="utf-8", newline="") as fh:
                fh.write(new)
            touched.append("config/models.toml")
        if not touched:
            result.update(skipped="nothing to change on " + base, unchanged=True)
            return result
        passed, tail = run_tests(wt, test_cmd, target_dir, timeout)
        msg = ("Compat: adapt to provider CLI changes\n\n" + "\n".join(f"- {f['detail']}" for f in fixes)
               + f"\n\nTests ({test_cmd}): {'passed' if passed else 'FAILED'}\nMade by scripts/compat/adapt.py; review before merging.\n")
        code, out = git("add", "-A", cwd=wt)
        if not code:
            code, out = git("commit", "-q", "-m", msg, cwd=wt)
        committed = not code
        result.update(files=sorted(set(touched)), tests_passed=passed, test_tail=tail,
                      **({"error": f"git commit: {out}"} if code else {}))
        return result
    except Exception as e:  # a half-made fix must not crash the run or look like a result
        result["error"] = f"{type(e).__name__}: {e}"
        return result
    finally:
        # Without its commit the branch would read as "made earlier" on the next run.
        if problems := cleanup(repo, wt, None if committed else branch):
            result["cleanup"] = problems


def update_installed(path, fixes, report, expected):
    """Apply registry fixes to the installed models.toml; backup and diff first.

    `expected` is the text this run read at its start. The write is a compare-and-swap:
    the file is moved aside to its backup name and the moved text compared, then the
    new text is moved in only if no file has appeared at `path` meanwhile. An edit at any
    point before that last move is kept and the write abandoned."""
    reg = [f for f in fixes if f["kind"].startswith("model-")]
    if not reg or not path or not os.path.exists(path):
        return None
    new = edit_registry(expected, reg)
    if new == expected:
        return None
    conflict = f"{path} changed while this run was working; it was left as it is"
    if open(path, encoding="utf-8").read() != expected:
        report["installed_conflict"] = conflict
        return None
    # The new text is written in full before the original moves, and a journal names the
    # backup while the registry is moved aside, so a killed run is repaired by the next.
    tmp = journal = None
    try:
        fd, tmp = tempfile.mkstemp(prefix=os.path.basename(path) + ".", suffix=".tmp", dir=os.path.dirname(path))
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as fh:
            fh.write(new)
        backup = new_file(f"{path}.bak-", "")  # one per change, holding exactly what this run replaced
        journal = path + SWAP
        atomic_write(journal, backup)
    except OSError as e:
        for f in (tmp, journal):
            if f and os.path.exists(f):
                os.remove(f)
        report["installed_conflict"] = f"the new {path} could not be written ({e}); it was left as it is"
        return None
    try:
        try:
            os.replace(path, backup)
        except OSError as e:  # e.g. an editor holding it open without sharing deletes
            os.remove(backup)
            report["installed_conflict"] = f"{path} could not be moved aside ({e}); it was left as it is"
            return None
        # From here the registry is absent: every failure tries to put the original back.
        try:
            if open(backup, encoding="utf-8").read() != expected:
                report["installed_conflict"] = conflict + rename_back(backup, path, "the edited version")
                return None
            rename_noreplace(tmp, path)
        except Exception as e:
            if os.path.exists(path):
                report["installed_conflict"] = f"{conflict}; the version it would have replaced is in {backup}"
            else:
                report["installed_conflict"] = f"{path} could not be replaced ({e})" + rename_back(backup, path, "the original")
            return None
    finally:
        if os.path.exists(tmp):
            os.remove(tmp)
        if os.path.exists(path):
            os.remove(journal)
    diff = "".join(difflib.unified_diff(expected.splitlines(True), new.splitlines(True), path, path + " (new)"))
    report["installed"] = {"path": path, "backup": backup, "diff": diff}
    return diff


# ---- Laya retrain -------------------------------------------------------------------
FINGERPRINT_VERSION = 2


def fingerprint(registry_text, repo, providers, ollama):
    """What Laya learns from: the models Nectra would actually route to and their roles,
    the role table and the task types.

    The roster follows src/models.rs: `providers` (None when the config has no list)
    disables unselected providers, Gemini needs an explicit "google", and configured
    Ollama models join as ollama/<name> with no role."""
    def routable(m):
        if providers is not None and m["provider"] not in providers:
            return False
        return m["provider"] != "google" or bool(providers and "google" in providers)
    roster = sorted((m["id"], sorted(m.get("prefers", []))) for m in registry_models(registry_text)
                    if m["enabled"] and routable(m))
    if providers is None or "ollama" in providers:
        roster += sorted((f"ollama/{name}", []) for name in ollama)
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


def mark_retrain(state, fp, reason, save):
    """Record that Laya needs retraining for `fp`, and save it, before anything acts on it."""
    if fp == state.get("fingerprint"):
        return
    pending = state.get("retrain", {})
    state["retrain"] = {"needed": True, "reason": reason, "fingerprint": fp,
                        "since": pending.get("since", TODAY) if pending.get("needed") else TODAY}
    state["fingerprint"] = fp
    save()


def maybe_retrain(state, repo, timeout, save):
    """Run tools/laya/retrain for a pending retrain. Only exit 0 clears it."""
    pending = state.get("retrain", {})
    if not pending.get("needed"):
        return {"status": "not needed"}
    reason, fp = pending["reason"], pending.get("fingerprint", state.get("fingerprint", ""))
    result = {"status": "retrain needed", "reason": reason}
    tool = retrain_tool(repo)
    if not tool:
        result["tool"] = "tools/laya/retrain is absent"
        return result
    cmd = {".py": [sys.executable, tool], ".ps1": ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", tool],
           ".sh": ["sh", tool]}.get(os.path.splitext(tool)[1], [tool])
    result["tool"] = os.path.relpath(tool, repo)
    try:
        code, out = run_bounded(cmd, timeout, cwd=repo,
                                env={**os.environ, "NECTRA_RETRAIN_REASON": reason, "NECTRA_RETRAIN_FINGERPRINT": fp})
    except subprocess.TimeoutExpired:
        result["tool"] += f", timed out after {timeout:g}s"
        return result
    except OSError as e:
        result["tool"] += f", could not start: {e}"
        return result
    result.update(exit=code, output=check.last_lines(out))
    if code == 0:
        result["status"] = "retrain triggered"
        state["retrain"] = {"needed": False, "last": TODAY, "reason": reason, "fingerprint": fp}
        save()
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
        result = {True: "tests passed", False: "tests FAILED", None: "no test result"}[b.get("tests_passed")]
        lines += ["", f"Branch `{b['branch']}`: " + (
            f"error: {b['error']}" if b.get("error") else b["skipped"] if b.get("skipped")
            else f"made by an earlier run; {result}" if b.get("existing")
            else f"{result}; files {', '.join(b['files'])}")]
        if b.get("test_tail"):
            lines += ["", "```", b["test_tail"], "```"]
    for what in ("branch", "installed_tests"):
        if (rep.get(what) or {}).get("cleanup"):
            lines += ["", "Cleanup failed; remove these by hand: " + "; ".join(rep[what]["cleanup"])]
    if rep.get("installed_tests"):
        t = rep["installed_tests"]
        lines += ["", f"Tests {'passed' if t.get('tests_passed') else 'FAILED'} with the new installed registry on "
                  f"`{t['base']}`" + (f": {t['error']}" if t.get("error") else ".")]
        if t.get("test_tail"):
            lines += ["", "```", t["test_tail"], "```"]
    if rep.get("recovered"):
        lines += ["", f"Installed registry repaired: {rep['recovered']}."]
    if rep.get("installed_blocked"):
        lines += ["", f"Installed registry not updated: {rep['installed_blocked']}."]
    if rep.get("installed_conflict"):
        lines += ["", f"Installed registry not updated: {rep['installed_conflict']}."]
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
    atomic_write(path, "\n".join(lines) + "\n")
    return issues


def choose_providers(arg, selected):
    """The CLIs to check: --providers if given ("" means none), else the config's list
    (an empty list means none), else all three."""
    chosen = (arg.split(",") if arg else []) if arg is not None else selected if selected is not None else list(check.CLI)
    return [p for p in chosen if p in check.CLI]


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
    ap.add_argument("--providers", help="default: the providers in the installed config, else all three")
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
    # Training is slow; the retrain tool may train in place or just queue a job.
    ap.add_argument("--retrain-timeout", type=float, default=12 * 3600, help="seconds (default 12 h)")
    ap.add_argument("--test-timeout", type=float, default=3600, help="seconds for the fix branch's tests (default 1 h)")
    ap.add_argument("--max-probes", type=int, default=3, help="new-model probe turns per run (default 3)")
    args = ap.parse_args(argv)
    os.makedirs(args.state_dir, exist_ok=True)
    installed, configured_ollama, selected = None, [], None
    if args.config != "none" and os.path.exists(args.config):
        cfg = tomllib.loads(open(args.config, encoding="utf-8").read())
        installed = os.path.join(os.path.dirname(args.config), cfg.get("registry", "models.toml"))
        configured_ollama = [m["name"] for m in cfg.get("ollama", {}).get("models", [])]
        selected = cfg.get("providers")
    # The state lock keeps runs sharing a state dir apart; the lock beside the installed
    # registry keeps apart runs with different state dirs that would write the same file.
    locks = [os.path.join(args.state_dir, "adapt.lock")] + ([installed + ".lock"] if installed else [])
    held = []
    try:
        for path in locks:
            lock = acquire_lock(path)
            if lock is None:
                print(f"Another adapt.py run holds {path}; it releases when that run ends.", file=sys.stderr)
                return 2
            held.append(lock)
        return run(args, installed, configured_ollama, selected)
    finally:
        for lock in held:
            lock.close()


def run(args, installed, configured_ollama, selected):
    state_path = os.path.join(args.state_dir, "state.json")
    state = json.load(open(state_path, encoding="utf-8")) if os.path.exists(state_path) else {}

    def save():
        atomic_write(state_path, json.dumps(state, indent=1))
    providers = choose_providers(args.providers, selected)
    bundled = open(os.path.join(args.repo, "config", "models.toml"), encoding="utf-8").read()
    recovered = recover_swap(installed)
    routing = open(installed, encoding="utf-8").read() if installed and os.path.exists(installed) else bundled
    def fp(text):
        return fingerprint(text, args.repo, selected, configured_ollama)

    # The first run compares with the registry as found, saved before anything can change it.
    # So does the first run after the fingerprint's definition changes.
    if "fingerprint" not in state or state.get("fingerprint_version") != FINGERPRINT_VERSION:
        state["fingerprint_version"] = FINGERPRINT_VERSION
        state["fingerprint"] = fp(routing)
        save()

    if args.fixture:
        obs = json.load(open(args.fixture, encoding="utf-8"))
        prev = obs.pop("previous_state", state)
    else:
        work = os.path.join(tempfile.gettempdir(), "nectra-compat-work")
        os.makedirs(work, exist_ok=True)
        if not os.path.isdir(os.path.join(work, ".git")):
            git("init", "-q", work)
        obs, prev = observe(providers, bundled, state, args.live, work, args.max_probes), state
    obs.setdefault("ollama_configured", configured_ollama)

    findings = analyse(obs, prev, bundled)
    # The installed copy can lag the bundled one; its own drift counts too.
    if routing != bundled:
        seen = {f["detail"] for f in findings}
        findings += [f for f in analyse({**obs, "streams": {}, "help": {}, "versions": {}}, prev, routing)
                     if f["detail"] not in seen and (f["kind"].startswith("model-") or f["kind"] == "discovery")]
    # Retiring every enabled model of a provider means the discovery is suspect, whichever
    # registry it would empty, so none of that provider's retirements are applied.
    for p in sorted({p for text in {bundled, routing} for p in emptied(text, findings)}):
        for f in findings:
            if f["kind"] == "model-retired" and f.get("provider") == p:
                f.pop("fix", None)
        findings.append({"kind": "discovery", "detail": f"retiring {p}'s missing models would leave {p} with no "
                         "enabled model in a registry; not trusting the discovery to do that"})
    fixes = [f for f in findings if f.get("fix")]
    rep = {"versions": obs["versions"],
           "models": {p: sorted(m.get("catalog") or m.get("listed") or []) for p, m in obs["models"].items()}}
    if obs.get("ollama") is not None:
        rep["models"]["ollama"] = obs["ollama"]
    if recovered:
        rep["recovered"] = recovered
    if args.nectra:
        cmd = [sys.executable, os.path.join(os.path.dirname(__file__), "check.py"), "--nectra", args.nectra,
               "--providers", ",".join(providers)] + (["--live"] if args.live else [])
        try:
            code, rep["check"] = run_bounded(cmd, CHECK_TIMEOUT, stdin=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            code, rep["check"] = None, f"[FAIL] check.py timed out after {CHECK_TIMEOUT}s"
        except OSError as e:
            code, rep["check"] = None, f"[FAIL] check.py could not start: {e}"
        findings += [{"kind": "check", "detail": l[7:]} for l in rep["check"].splitlines() if l.startswith("[FAIL]")]
        if code:  # a crash prints no [FAIL] line; its exit code is the only sign
            findings.append({"kind": "check", "detail": f"check.py exited {code}: {check.last_lines(rep['check'], 3)}"})
    # A rename is only mechanical where Nectra builds that provider's command with the literal.
    for f in fixes:
        if f["kind"] != "flag":
            continue
        literal = re.compile(rf'"{re.escape(f["old"])}(?![\w-])')
        if not [p for p in rename_plan(args.repo, f["old"], f["new"])
                if p.endswith(".rs") and literal.search(open(p, encoding="utf-8").read())]:
            f.pop("fix")
            f["detail"] += ", but Nectra's code doesn't pass it literally"
    fixes = [f for f in fixes if f.get("fix")]
    def changes_bundled(f):
        try:
            return f["kind"] == "flag" or edit_registry(bundled, [f]) != bundled
        except ValueError:
            return True  # make_branch hits the same error and reports it
    bundled_fixes = [f for f in fixes if changes_bundled(f)]
    target = args.target_dir or os.path.join(args.state_dir, "target")
    if bundled_fixes:
        rep["branch"] = make_branch(args.repo, args.base, bundled_fixes, args.test_cmd, target, args.test_timeout)
    # The installed registry routes for real, so it only takes a change whose exact text passed
    # the tests: on the fix branch, or else on its own on the base.
    b = rep.get("branch")
    reg = [f for f in fixes if f["kind"].startswith("model-")]
    try:
        proposed = edit_registry(routing, reg) if reg and installed and os.path.exists(installed) else routing
    except ValueError as e:
        proposed, rep["installed_blocked"] = routing, f"{installed} could not be edited safely ({e})"
    if b and (b.get("error") or (not b.get("unchanged") and not b.get("tests_passed"))):
        why = b.get("error") or ("its tests failed" if b.get("tests_passed") is False else "no test result")
        rep["installed_blocked"] = f"the fix branch `{b['branch']}` did not pass ({why})"
    elif proposed != routing:
        tested = b and b.get("tests_passed") and git_blob(args.repo, f"{b['branch']}:config/models.toml") == proposed
        if not tested:
            t = test_registry(args.repo, args.base, proposed, args.test_cmd, target, args.test_timeout)
            rep["installed_tests"] = {**t, "base": args.base}
            if not t.get("tests_passed"):
                rep["installed_blocked"] = "the new installed registry did not pass the tests" + (
                    f" ({t['error']})" if t.get("error") else "")
        if not rep.get("installed_blocked"):
            before = {k: state.get(k) for k in ("fingerprint", "retrain")}
            mark_retrain(state, fp(proposed), "; ".join(f["detail"] for f in reg), save)
            update_installed(installed, fixes, rep, routing)
            if "installed" not in rep:  # refused: the change it announced never happened
                for k, v in before.items():
                    state.pop(k, None) if v is None else state.update({k: v})
                save()

    after = open(installed, encoding="utf-8").read() if installed and os.path.exists(installed) else bundled
    mark_retrain(state, fp(after), "models, roles, task types or provider selection changed", save)
    rep["retrain"] = maybe_retrain(state, args.repo, args.retrain_timeout, save)

    state.update(versions={k: v for k, v in obs["versions"].items() if v} or prev.get("versions", {}),
                 help_flags={k: sorted(flags_in(v)) for k, v in obs["help"].items()} or prev.get("help_flags", {}),
                 probes={**state.get("probes", {}), **obs["probes"]}, last_run=TODAY)
    save()
    report = new_file(os.path.join(args.state_dir, "report-"), ".md")
    issues = write_report(report, findings, rep)
    shutil.copyfile(report, os.path.join(args.state_dir, "report-latest.md"))
    print(open(report, encoding="utf-8").read())
    print(f"Report: {report}")
    b = rep.get("branch") or {}
    branch_failed = b and not b.get("unchanged") and (b.get("error") or not b.get("tests_passed"))
    leftovers = b.get("cleanup") or (rep.get("installed_tests") or {}).get("cleanup")
    return 1 if issues or rep.get("installed_conflict") or rep.get("installed_blocked") or branch_failed or leftovers else 0


if __name__ == "__main__":
    sys.exit(main())
