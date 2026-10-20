"""Tests for adapt.py: python scripts/compat/test_adapt.py"""
import json, os, shutil, subprocess, sys, tempfile, time, tomllib, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import adapt  # noqa: E402

FIXTURE = os.path.join(HERE, "fixtures", "breaking-release.json")
REGISTRY = open(os.path.join(adapt.ROOT, "config", "models.toml"), encoding="utf-8").read()
OK = f'"{sys.executable}" -c "print(1)"'


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, encoding="utf-8", check=True).stdout


class Parsing(unittest.TestCase):
    def test_claude_aliases_come_from_the_model_option(self):
        text = ("  --fallback-model <model>  e.g. 'nope'\n  --model <model>   Provide an alias (e.g.\n"
                "                    'fable', 'opus', or 'sonnet') or a full name.\n  -n, --name <name>  'x'\n")
        self.assertEqual(adapt.claude_aliases(text), ["fable", "opus", "sonnet"])

    def test_gemini_constants_skip_non_chat_models(self):
        text = ('DEFAULT_GEMINI_MODEL = "gemini-2.5-pro"; LATEST_GEMINI_FLASH_MODEL = "gemini-3.8-flash";'
                'DEFAULT_GEMINI_EMBEDDING_MODEL = "gemini-embedding-001"; X_MODEL = "gemini-3.1-pro-preview-customtools";'
                'LEGACY_CCPA_FLASH_MODEL = "gemini-3-flash"; y = "gemini-2.5-flash"')
        declared, mentioned = adapt.gemini_models(text)
        self.assertEqual(declared, ["gemini-2.5-pro", "gemini-3.8-flash"])
        self.assertIn("gemini-2.5-flash", mentioned)

    def test_registry_edits_keep_comments_and_stay_valid(self):
        new = adapt.edit_registry(REGISTRY, [
            {"kind": "model-new", "id": "gpt-6-luna", "provider": "openai", "source": "test"},
            {"kind": "model-retired", "id": "gpt-5.6-luna"}])
        self.assertIn("# Bootstrap heuristic scores", new)
        models = {m["id"]: m for m in tomllib.loads(new)["model"]}
        self.assertFalse(models["gpt-6-luna"]["enabled"])
        self.assertEqual(models["gpt-6-luna"]["prefers"], [])
        self.assertEqual(models["gpt-6-luna"]["capabilities"], models["gpt-5.6-luna"]["capabilities"])
        self.assertFalse(models["gpt-5.6-luna"]["enabled"])
        self.assertTrue(models["gpt-5.6-sol"]["enabled"])
        # Applying the same edit twice changes nothing more.
        self.assertEqual(adapt.edit_registry(new, [{"kind": "model-new", "id": "gpt-6-luna", "provider": "openai",
                                                    "source": "test"}]), new)


class BreakingRelease(unittest.TestCase):
    def setUp(self):
        self.fx = json.load(open(FIXTURE, encoding="utf-8"))
        self.findings = adapt.analyse(self.fx, self.fx.pop("previous_state"), REGISTRY)

    def detail(self, kind):
        return [f for f in self.findings if f["kind"] == kind]

    def test_flag_rename_is_mechanical(self):
        rename = [f for f in self.detail("flag") if f.get("fix")]
        self.assertEqual([(f["old"], f["new"]) for f in rename], [("--ephemeral", "--ephemeral-session")])

    def test_removed_safety_flag_is_an_issue_not_a_fix(self):
        gone = [f for f in self.detail("flag") if "--dangerously-skip-permissions" in f["detail"]]
        self.assertEqual(len(gone), 1)
        self.assertNotIn("fix", gone[0])
        self.assertIn("safety", gone[0]["detail"])

    def test_stream_field_loss_is_reported(self):
        self.assertEqual(len(self.detail("stream")), 1)
        self.assertIn("claude `result` event lost ['result']", self.detail("stream")[0]["detail"])

    def test_models(self):
        new = {f["id"] for f in self.detail("model-new")}
        self.assertEqual(new, {"gpt-7-nova", "fable"})  # gpt-reserve is hidden
        self.assertEqual({f["id"] for f in self.detail("model-retired")}, {"gpt-5.6-luna"})

    def test_versions(self):
        self.assertEqual(len(self.detail("version")), 2)


class Sandbox(unittest.TestCase):
    """A throwaway repo, user config dir and state dir."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.repo = os.path.join(self.tmp, "repo")
        for rel in ("config/models.toml", "scripts/compat/check.py", "src/policy.rs", "src/classifier.rs"):
            os.makedirs(os.path.dirname(os.path.join(self.repo, rel)), exist_ok=True)
            shutil.copy(os.path.join(adapt.ROOT, rel), os.path.join(self.repo, rel))
        os.makedirs(os.path.join(self.repo, "src", "executor"))
        with open(os.path.join(self.repo, "src", "executor", "process.rs"), "w") as f:
            f.write('let args: &[&str] = match rt { "codex" => &["exec", "--ephemeral"], _ => &[] };\n')
        git(self.repo, "init", "-q", "-b", "master")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@t", "add", "-A")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base")
        self.user = os.path.join(self.tmp, "user")
        os.makedirs(self.user)
        with open(os.path.join(self.user, "config.toml"), "w") as f:
            f.write('providers = ["anthropic", "openai"]\nregistry = "models.toml"\n')
        shutil.copy(os.path.join(adapt.ROOT, "config", "models.toml"), self.user)
        self.state = os.path.join(self.tmp, "state")

    def tearDown(self):
        subprocess.run(["git", "worktree", "prune"], cwd=self.repo)
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run_main(self, test_cmd=OK, fixture=FIXTURE, extra=()):
        env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
        os.environ.update(env)
        return adapt.main(["--fixture", fixture, "--repo", self.repo, "--state-dir", self.state,
                           "--config", os.path.join(self.user, "config.toml"), "--test-cmd", test_cmd,
                           "--providers", "anthropic,openai", *extra])

    def installed(self):
        return open(os.path.join(self.user, "models.toml"), encoding="utf-8").read()

    def fixture(self, **changes):
        """A copy of the breaking-release fixture with top-level keys replaced."""
        fx = json.load(open(FIXTURE, encoding="utf-8"))
        fx.update(changes)
        path = os.path.join(self.tmp, f"fixture-{len(os.listdir(self.tmp))}.json")
        json.dump(fx, open(path, "w", encoding="utf-8"))
        return path

    def script(self, code):
        """A shell command running this Python code, for --test-cmd."""
        path = os.path.join(self.tmp, f"script-{len(os.listdir(self.tmp))}.py")
        with open(path, "w", encoding="utf-8") as f:
            f.write(code + "\n")
        return f'"{sys.executable}" "{path}"'

    def latest_report(self):
        newest = max((os.path.join(self.state, f) for f in os.listdir(self.state) if f.startswith("report-")),
                     key=os.path.getmtime)
        return open(newest, encoding="utf-8").read()

    def reports(self):
        return sorted(f for f in os.listdir(self.state) if f.startswith("report-") and f != "report-latest.md")

    def state_json(self):
        return json.load(open(os.path.join(self.state, "state.json"), encoding="utf-8"))

    def clean_fixture(self):
        """Only a new Codex model: no other issue that would set the exit status anyway."""
        fx = json.load(open(FIXTURE, encoding="utf-8"))
        catalog = {**fx["models"]["openai"]["catalog"], "gpt-5.6-luna": "list"}
        return self.fixture(help={}, streams={}, versions={}, models={"openai": {"catalog": catalog}})

    def branches(self):
        return [b.strip(" *") for b in git(self.repo, "branch", "--format=%(refname:short)").split()]



class EndToEnd(Sandbox):
    """The fixture through main(): branch with tests, installed registry, retrain."""

    def test_breaking_release_end_to_end_without_retrain_tool(self):
        code = self.run_main()
        self.assertEqual(code, 1)  # issues remain: removed safety flag, stream change
        auto = [b for b in self.branches() if b.startswith("compat/auto-")]
        self.assertEqual(len(auto), 1)
        self.assertEqual(git(self.repo, "rev-parse", "--abbrev-ref", "HEAD").strip(), "master")
        src = git(self.repo, "show", f"{auto[0]}:src/executor/process.rs")
        self.assertIn('"--ephemeral-session"', src)
        self.assertIn('"--ephemeral-session"', git(self.repo, "show", f"{auto[0]}:scripts/compat/check.py"))
        reg = {m["id"]: m for m in tomllib.loads(git(self.repo, "show", f"{auto[0]}:config/models.toml"))["model"]}
        self.assertFalse(reg["gpt-7-nova"]["enabled"])
        self.assertFalse(reg["gpt-5.6-luna"]["enabled"])
        self.assertIn("Tests", git(self.repo, "log", "-1", "--format=%B", auto[0]))
        # Master itself is untouched.
        self.assertIn('"--ephemeral"', open(os.path.join(self.repo, "src", "executor", "process.rs")).read())
        # Installed registry: edited, backed up, still valid.
        installed = os.path.join(self.user, "models.toml")
        ids = {m["id"]: m for m in tomllib.loads(open(installed, encoding="utf-8").read())["model"]}
        self.assertIn("fable", ids)
        self.assertFalse(ids["gpt-5.6-luna"]["enabled"])
        backups = [f for f in os.listdir(self.user) if f.startswith("models.toml.bak-")]
        self.assertEqual(len(backups), 1)
        self.assertEqual(open(os.path.join(self.user, backups[0]), encoding="utf-8").read(), REGISTRY)
        # luna had the simple-coding role, so Laya needs retraining; the tool is absent.
        st = self.state_json()
        self.assertTrue(st["retrain"]["needed"])
        self.assertIn("gpt-5.6-luna", st["retrain"]["reason"])
        report = self.latest_report()
        self.assertIn("tools/laya/retrain is absent", report)
        self.assertIn("--dangerously-skip-permissions", report)
        # A second run makes no duplicate branch and no duplicate registry entries.
        self.run_main()
        self.assertEqual(len([b for b in self.branches() if b.startswith("compat/auto-")]), 1)
        adapt.validate_registry(open(installed, encoding="utf-8").read())

    def test_retrain_tool_is_called_once_it_exists(self):
        self.run_main()
        self.assertTrue(self.state_json()["retrain"]["needed"])
        tool = os.path.join(self.repo, "tools", "laya")
        os.makedirs(tool)
        seen = os.path.join(self.tmp, "seen.txt")
        with open(os.path.join(tool, "retrain.py"), "w") as f:
            f.write(f"import os\nopen({seen!r}, 'w').write(os.environ['NECTRA_RETRAIN_REASON'])\n")
        self.run_main()
        st = self.state_json()
        self.assertFalse(st["retrain"]["needed"])
        self.assertIn("gpt-5.6-luna", open(seen).read())
        # Nothing changed since: no further retrain.
        os.remove(seen)
        self.run_main()
        self.assertFalse(os.path.exists(seen))

    def test_failing_retrain_tool_keeps_it_pending(self):
        os.makedirs(os.path.join(self.repo, "tools", "laya"))
        with open(os.path.join(self.repo, "tools", "laya", "retrain.py"), "w") as f:
            f.write("raise SystemExit(3)\n")
        self.run_main()
        st = self.state_json()
        self.assertTrue(st["retrain"]["needed"])

    def test_failing_tests_are_reported(self):
        code = self.run_main(test_cmd=f'"{sys.executable}" -c "raise SystemExit(1)"')
        self.assertEqual(code, 1)
        self.assertIn("tests FAILED", self.latest_report())


class AstraReview(Sandbox):
    """The risky paths from the Astra review (astra-compat-review-1.md)."""

    # 1: concurrency
    def test_a_second_run_fails_fast_while_the_lock_is_held(self):
        os.makedirs(self.state)
        holder = subprocess.Popen([sys.executable, "-c", f"import os, sys; sys.path.insert(0, {HERE!r}); import adapt; "
                                   f"l = adapt.acquire_lock(os.path.join({self.state!r}, 'adapt.lock')); print('held' if l else 'no', flush=True); sys.stdin.read()"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), "held")
            self.assertEqual(self.run_main(), 2)
            self.assertFalse(os.path.exists(os.path.join(self.state, "state.json")))
        finally:
            holder.stdin.close()
            holder.wait()
        self.assertEqual(self.run_main(), 1)  # lock released with the process

    def test_a_registry_edited_during_the_run_is_not_overwritten(self):
        installed = os.path.join(self.user, "models.toml")
        # The "tests" stand in for Michael editing the registry while the branch builds.
        self.assertEqual(self.run_main(test_cmd=self.script(f"open({installed!r}, 'a').write('# my edit\\n')")), 1)
        text = self.installed()
        self.assertIn("# my edit", text)
        self.assertNotIn('id = "fable"', text)
        self.assertIn("changed while this run was working", self.latest_report())
        self.assertEqual([f for f in os.listdir(self.user) if f.endswith(".tmp")], [])

    # 2: discovery trust
    def test_codex_catalog_needs_success_and_a_real_list(self):
        good = '{"models": [{"slug": "gpt-6-luna", "visibility": "list"}]}'
        self.assertEqual(adapt.parse_codex_catalog(0, good), ({"gpt-6-luna": "list"}, None))
        for code, out in ((1, '{"models": []}'), (1, good), (0, '{"models": []}'), (0, '{"models": {}}'),
                          (0, '{"models": [{"name": "x"}]}'), (0, '{"models": ["x"]}'), (0, "error: not signed in"),
                          (0, '{"models": [{"slug": 7}]}')):
            catalog, error = adapt.parse_codex_catalog(code, out)
            self.assertIsNone(catalog, out)
            self.assertTrue(error)

    def analyse_models(self, models, **extra):
        obs = {"versions": {}, "help": {}, "streams": {}, "probes": {}, "models": models, "ollama": None, **extra}
        return adapt.analyse(obs, {}, REGISTRY)

    def test_failed_discovery_never_retires_and_is_an_issue(self):
        for models in ({"openai": {"catalog": None, "error": "exit 1"}},
                       {"anthropic": {"listed": [], "error": "claude --help exited 1"}},
                       {"google": {"listed": [], "mentioned": [], "error": "Gemini CLI bundle not found"}}):
            found = self.analyse_models(models)
            self.assertFalse([f for f in found if f["kind"].startswith("model-")], models)
            self.assertEqual([f["kind"] for f in found], ["discovery"], models)

    def test_a_catalog_that_would_retire_every_model_is_not_trusted(self):
        found = self.analyse_models({"openai": {"catalog": {"gpt-7-nova": "list"}, "error": None}})
        self.assertFalse([f for f in found if f["kind"] == "model-retired"])
        self.assertIn("discovery", [f["kind"] for f in found])

    def test_unreachable_ollama_is_an_issue_when_models_are_configured(self):
        found = self.analyse_models({}, ollama_error="connection refused", ollama_configured=["llama3.1:8b"])
        self.assertEqual([f["kind"] for f in found], ["discovery"])
        self.assertEqual(self.analyse_models({}, ollama_error="connection refused", ollama_configured=[]), [])

    # 3: a failing fix branch protects the installed registry
    def test_failing_branch_tests_leave_the_installed_registry_alone(self):
        self.assertEqual(self.run_main(test_cmd=self.script("raise SystemExit(1)")), 1)
        self.assertEqual(self.installed(), REGISTRY)
        self.assertEqual([f for f in os.listdir(self.user) if ".bak-" in f], [])
        self.assertIn("Installed registry not updated: the fix branch", self.latest_report())

    def test_a_branch_that_cannot_be_made_leaves_the_installed_registry_alone(self):
        self.assertEqual(self.run_main(extra=["--base", "no-such-branch"]), 1)
        self.assertEqual(self.installed(), REGISTRY)
        self.assertIn("Installed registry not updated: the fix branch", self.latest_report())

    # 4: retrain intent survives interruption, launch errors and hangs
    def retrain_tool(self, name, body):
        os.makedirs(os.path.join(self.repo, "tools", "laya"), exist_ok=True)
        with open(os.path.join(self.repo, "tools", "laya", name), "w") as f:
            f.write(body)

    def test_retrain_intent_is_saved_before_the_registry_changes(self):
        from unittest import mock
        with mock.patch.object(adapt, "update_installed", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.run_main()
        st = self.state_json()
        self.assertTrue(st["retrain"]["needed"])
        self.assertIn("gpt-5.6-luna", st["retrain"]["reason"])
        self.assertEqual(st["retrain"]["fingerprint"], st["fingerprint"])

    def test_an_interrupted_first_run_still_retrains_next_time(self):
        from unittest import mock
        with mock.patch.object(adapt, "maybe_retrain", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.run_main()
        self.assertFalse(adapt.registry_models(self.installed())[3]["enabled"])  # luna already disabled
        seen = os.path.join(self.tmp, "seen.txt")
        self.retrain_tool("retrain.py", f"open({seen!r}, 'w').write('ran')\n")
        self.run_main()
        self.assertTrue(os.path.exists(seen))
        self.assertFalse(self.state_json()["retrain"]["needed"])

    def test_a_retrain_tool_that_cannot_start_keeps_it_pending(self):
        self.retrain_tool("retrain", "this is not a program\n")
        self.run_main()
        st = self.state_json()
        self.assertTrue(st["retrain"]["needed"])
        self.assertIn("could not start", self.latest_report())

    def test_a_hung_retrain_tool_times_out_and_stays_pending(self):
        self.retrain_tool("retrain.py", "import time\ntime.sleep(60)\n")
        start = time.time()
        self.run_main(extra=["--retrain-timeout", "2"])
        self.assertLess(time.time() - start, 40)
        self.assertTrue(self.state_json()["retrain"]["needed"])
        self.assertIn("timed out", self.latest_report())

    # 5: renames are safe on both ends and scoped to one provider
    def analyse_help(self, key, old, new):
        fx = json.load(open(FIXTURE, encoding="utf-8"))
        prev = fx.pop("previous_state")
        fx["help"][key] = fx["help"][key].replace(f"  {old}  something", f"  {new}  something")
        return [f for f in adapt.analyse(fx, prev, REGISTRY) if f["kind"] == "flag" and old in f["detail"]]

    def test_a_rename_into_a_safety_flag_is_an_issue(self):
        found = self.analyse_help("codex exec --help", "--sandbox", "--sandbox-bypass")
        self.assertEqual(len(found), 1)
        self.assertNotIn("fix", found[0])
        self.assertIn("safety", found[0]["detail"])

    def test_a_flag_another_subcommand_still_lists_is_not_renamed(self):
        found = self.analyse_help("codex exec --help", "--json", "--jsonl")
        self.assertEqual(len(found), 1)
        self.assertNotIn("fix", found[0])
        self.assertIn("codex exec resume --help", found[0]["detail"])

    def test_a_rename_updates_shared_code_deliberately(self):
        # Astra's case: a permissions filter shared by every runtime, nearest to a "gemini"
        # literal, and Codex's own launch code. All of it must move together.
        src = os.path.join(self.repo, "src", "executor", "two.rs")
        with open(src, "w") as f:
            f.write('let rt = match d.runtime { "gemini" => &cfg.gemini, _ => &cfg.codex };\n'
                    'if ["--ephemeral", "--permission-mode"].contains(&flag) { continue; }\n'
                    'match rt { "codex" => args.push("--ephemeral"), _ => {} }\n'
                    '// "--ephemeral=1" is the same flag\n')
        plan = adapt.rename_plan(self.repo, "--ephemeral", "--ephemeral-session")
        self.assertNotIn('"--ephemeral"', plan[src])
        self.assertEqual(plan[src].count("--ephemeral-session"), 3)
        self.assertIn(os.path.join(self.repo, "scripts", "compat", "check.py"), plan)

    def test_a_flag_another_cli_uses_is_not_renamed(self):
        fx = json.load(open(FIXTURE, encoding="utf-8"))
        prev = fx.pop("previous_state")
        fx["help"]["claude --help"] += "  --ephemeral  something\n"
        found = [f for f in adapt.analyse(fx, prev, REGISTRY) if f["kind"] == "flag" and "--ephemeral" in f["detail"]]
        self.assertEqual(len(found), 1)
        self.assertNotIn("fix", found[0])
        self.assertIn("claude", found[0]["detail"])
        # What the previous run saw counts too, for a CLI not checked this time.
        fx = json.load(open(FIXTURE, encoding="utf-8"))
        prev = fx.pop("previous_state")
        del fx["help"]["claude --help"]
        prev["help_flags"]["claude --help"].append("--ephemeral")
        found = [f for f in adapt.analyse(fx, prev, REGISTRY) if f["kind"] == "flag" and "--ephemeral" in f["detail"]]
        self.assertNotIn("fix", found[0])

    # 6: the loop's own probe commands follow a generated rename
    def test_the_fix_branch_also_renames_the_probe_commands(self):
        self.run_main()
        branch = [b for b in self.branches() if b.startswith("compat/auto-")][0]
        ns = {"__file__": os.path.join(self.repo, "scripts", "compat", "check.py"), "__name__": "branch_check"}
        exec(git(self.repo, "show", f"{branch}:scripts/compat/check.py"), ns)
        codex = ns["stream_cmd"]("codex", "gpt-6-luna")
        self.assertIn("--ephemeral-session", codex)
        self.assertNotIn("--ephemeral", codex)
        self.assertIn("--no-session-persistence", ns["stream_cmd"]("claude", "haiku"))
        self.assertIs(adapt.check.stream_cmd, adapt.stream_cmd)

    # 7: every run keeps its own backup and report
    def test_each_run_keeps_its_own_backup_and_report(self):
        fx = json.load(open(FIXTURE, encoding="utf-8"))
        one = self.fixture(models={"openai": {"catalog": {**fx["models"]["openai"]["catalog"], "gpt-5.6-luna": "list"}}})
        two = self.fixture(models={"openai": {"catalog": {**fx["models"]["openai"]["catalog"], "gpt-5.6-luna": "list",
                                                          "gpt-7-terra": "list"}}})
        self.run_main(fixture=one)
        after_one = self.installed()
        self.run_main(fixture=two)
        self.run_main(fixture=two)  # nothing new: still its own report
        backups = sorted(f for f in os.listdir(self.user) if f.startswith("models.toml.bak-"))
        self.assertEqual(len(backups), 2)
        texts = {open(os.path.join(self.user, b), encoding="utf-8").read() for b in backups}
        self.assertEqual(texts, {REGISTRY, after_one})
        self.assertEqual(len(self.reports()), 3)
        latest = open(os.path.join(self.state, "report-latest.md"), encoding="utf-8").read()
        self.assertEqual(latest, open(os.path.join(self.state, self.reports()[-1]), encoding="utf-8").read())

    # 8: branch outcomes always reach the exit status
    def test_an_existing_branch_keeps_its_failed_test_result(self):
        clean = self.clean_fixture()
        once = os.path.join(self.tmp, "failed-once").replace("\\", "/")
        # Fails the first time only: the second run must answer from the branch, not rerun.
        flaky = self.script(f"import os, sys; sys.exit(0 if os.path.exists('{once}') else open('{once}', 'w') and 1)")
        self.assertEqual(self.run_main(test_cmd=flaky, fixture=clean), 1)
        self.assertEqual(self.run_main(test_cmd=flaky, fixture=clean), 1)
        self.assertIn("tests FAILED", self.latest_report())
        self.assertEqual(self.installed(), REGISTRY)

    def test_an_existing_branch_keeps_its_passed_test_result(self):
        clean = self.clean_fixture()
        self.assertEqual(self.run_main(fixture=clean), 0)
        self.assertEqual(self.run_main(fixture=clean), 0)
        self.assertIn("tests passed", self.latest_report())

    def test_a_branch_error_fails_the_run(self):
        self.assertEqual(self.run_main(fixture=self.clean_fixture(), extra=["--config", "none", "--base", "nope"]), 1)

    # 9: disable_model handles real TOML and proves it worked
    def test_disable_model_understands_valid_toml_spellings(self):
        luna = 'id = "gpt-5.6-luna"\nprovider = "openai"\nruntime = "codex"\nenabled = true\n'
        self.assertIn(luna, REGISTRY)
        for spelling in ('id = "gpt-5.6-luna" # my model\nprovider = "openai"\nruntime = "codex"\nenabled = true # on\n',
                         "  id = 'gpt-5.6-luna'\n  provider = 'openai'\n  runtime = 'codex'\n  enabled=true\n",
                         'id="gpt-5.6-luna"\nprovider = "openai"\nruntime = "codex"\nenabled = true\n'):
            text = REGISTRY.replace(luna, spelling).replace("[[model]]\nid = \"haiku\"", "[[ model ]]  # small\nid = \"haiku\"")
            new = adapt.disable_model(text, "gpt-5.6-luna")
            before = {m["id"]: m for m in adapt.registry_models(text)}
            after = {m["id"]: m for m in adapt.registry_models(new)}
            self.assertFalse(after["gpt-5.6-luna"]["enabled"], spelling)
            before["gpt-5.6-luna"]["enabled"] = False
            self.assertEqual(after, before, spelling)
            self.assertEqual(new.count("#"), text.count("#"), spelling)

    def test_disable_model_fails_loudly_when_it_cannot(self):
        inline = 'model = [{id = "gpt-5.6-luna", provider = "openai", runtime = "codex", enabled = true, capabilities = {}}]\n'
        quoted = REGISTRY.replace('id = "gpt-5.6-luna"\nprovider = "openai"\nruntime = "codex"\nenabled = true',
                                  'id = "gpt-5.6-luna"\nprovider = "openai"\nruntime = "codex"\n"enabled" = true')
        for text in (inline, quoted):
            with self.assertRaises(ValueError):
                adapt.disable_model(text, "gpt-5.6-luna")
        with self.assertRaises(ValueError):
            adapt.disable_model(REGISTRY, "no-such-model")
        self.assertEqual(adapt.disable_model(adapt.disable_model(REGISTRY, "gpt-5.6-luna"), "gpt-5.6-luna"),
                         adapt.disable_model(REGISTRY, "gpt-5.6-luna"))

    def test_a_registry_it_cannot_edit_is_reported_not_crashed_on(self):
        with open(os.path.join(self.user, "models.toml"), "w", encoding="utf-8") as f:
            f.write(REGISTRY.replace('runtime = "codex"\nenabled = true\neffort = "medium"',
                                     'runtime = "codex"\n"enabled" = true\neffort = "medium"'))
        self.assertEqual(self.run_main(), 1)
        self.assertIn("Installed registry not updated", self.latest_report())

    # 10: provider selection is honoured and part of the fingerprint
    def test_an_empty_provider_list_means_none(self):
        self.assertEqual(adapt.choose_providers("", None), [])
        self.assertEqual(adapt.choose_providers(None, []), [])
        self.assertEqual(adapt.choose_providers(None, None), ["anthropic", "openai", "google"])
        self.assertEqual(adapt.choose_providers(None, ["openai", "ollama"]), ["openai"])
        self.assertEqual(adapt.choose_providers("anthropic", ["openai"]), ["anthropic"])

    def test_fingerprint_follows_what_nectra_would_route_to(self):
        fp = lambda providers, ollama=(): adapt.fingerprint(REGISTRY, adapt.ROOT, providers, list(ollama))
        both = fp(["anthropic", "openai"])
        self.assertNotEqual(both, fp(["anthropic"]))
        self.assertNotEqual(both, fp(["anthropic", "openai", "google"]))
        self.assertNotEqual(both, fp(["anthropic", "openai", "ollama"], ["llama3.1:8b"]))
        self.assertEqual(both, fp(["anthropic", "openai"], ["llama3.1:8b"]))  # ollama not selected
        self.assertEqual(both, fp(["openai", "anthropic"]))
        swapped = REGISTRY.replace('prefers = ["planning"]', 'prefers = ["planning", "coding"]')
        self.assertEqual(adapt.fingerprint(swapped, adapt.ROOT, None, []),
                         adapt.fingerprint(swapped.replace('["planning", "coding"]', '["coding", "planning"]'), adapt.ROOT, None, []))

    def test_changing_the_selected_providers_asks_for_a_retrain(self):
        quiet = self.fixture(help={}, streams={}, versions={}, models={})
        self.assertEqual(self.run_main(fixture=quiet), 0)
        self.assertFalse(self.state_json().get("retrain", {}).get("needed"))
        with open(os.path.join(self.user, "config.toml"), "w") as f:
            f.write('providers = ["anthropic"]\nregistry = "models.toml"\n')
        self.run_main(fixture=quiet)
        self.assertTrue(self.state_json()["retrain"]["needed"])

    # Budgets: probes per run, and the fix branch's test step
    def test_new_model_probes_are_capped_per_run(self):
        from unittest import mock
        aliases = ", ".join(f"'new{i}'" for i in range(6))
        calls = []

        def fake_run(cmd, timeout=120, cwd=None, **kw):
            calls.append(cmd)
            if cmd[1:] == ["--help"]:
                return 0, f"  --model <model>  an alias (e.g. {aliases})\n  -n, --name\n"
            return 0, '{"type": "result", "subtype": "success", "result": "42"}'
        with mock.patch.object(adapt.check, "run", side_effect=fake_run):
            obs = adapt.observe(["anthropic"], REGISTRY, {}, True, self.tmp, max_probes=2)
        self.assertEqual(len(obs["probes"]), 2)
        self.assertEqual(len([c for c in calls if c[0] == "claude" and "-p" in c and "haiku" not in c]), 2)
        found = adapt.analyse(obs, {}, REGISTRY)
        self.assertEqual(len([f for f in found if f["kind"] == "model-candidate"]), 4)

    def test_branch_tests_that_hang_time_out_and_count_as_failed(self):
        start = time.time()
        code = self.run_main(test_cmd=self.script("import time; time.sleep(60)"), extra=["--test-timeout", "3"])
        self.assertLess(time.time() - start, 45)
        self.assertEqual(code, 1)
        self.assertIn("timed out after 3s", self.latest_report())
        self.assertEqual(self.installed(), REGISTRY)


class AstraReReview(Sandbox):
    """The gaps from Astra's re-review (astra-compat-review-2.md)."""

    def hold_lock(self, path):
        holder = subprocess.Popen([sys.executable, "-c", f"import sys; sys.path.insert(0, {HERE!r}); import adapt; "
                                   f"l = adapt.acquire_lock({path!r}); print('held' if l else 'no', flush=True); sys.stdin.read()"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.assertEqual(holder.stdout.readline().strip(), "held")
        return holder

    def no_leftovers(self):
        self.assertEqual([f for f in os.listdir(self.user) if f.endswith(".tmp")], [])

    # 1: the registry write is a compare-and-swap, and its lock is the registry's own
    def test_an_edit_just_before_the_swap_is_kept(self):
        from unittest import mock
        installed, real = os.path.join(self.user, "models.toml"), adapt.new_file

        def edit_then_reserve(base, ext):
            if ".bak-" in base:  # after the comparison, before the file is moved aside
                open(installed, "a", encoding="utf-8").write("# my late edit\n")
            return real(base, ext)
        with mock.patch.object(adapt, "new_file", side_effect=edit_then_reserve):
            self.assertEqual(self.run_main(), 1)
        self.assertIn("# my late edit", self.installed())
        self.assertNotIn('id = "fable"', self.installed())
        self.assertIn("changed while this run was working", self.latest_report())
        self.assertEqual([f for f in os.listdir(self.user) if ".bak-" in f], [])
        self.no_leftovers()

    def test_a_file_rewritten_during_the_swap_is_kept(self):
        from unittest import mock
        installed, real = os.path.join(self.user, "models.toml"), adapt.rename_noreplace

        def rewrite_then_move(src, dst):
            if dst == installed and src.endswith(".tmp"):
                open(installed, "w", encoding="utf-8").write("# theirs\n")
            return real(src, dst)
        with mock.patch.object(adapt, "rename_noreplace", side_effect=rewrite_then_move):
            self.assertEqual(self.run_main(), 1)
        self.assertEqual(self.installed(), "# theirs\n")
        self.assertIn("changed while this run was working", self.latest_report())
        backups = [f for f in os.listdir(self.user) if ".bak-" in f]
        self.assertEqual(len(backups), 1)
        self.assertEqual(open(os.path.join(self.user, backups[0]), encoding="utf-8").read(), REGISTRY)
        self.no_leftovers()

    def test_runs_with_different_state_dirs_share_the_registry_lock(self):
        holder = self.hold_lock(os.path.join(self.user, "models.toml.lock"))
        try:
            self.assertEqual(self.run_main(), 2)
            self.assertEqual(self.installed(), REGISTRY)
        finally:
            holder.stdin.close()
            holder.wait()


    # 2: the whole catalog is validated, and the all-retirement guard sees every registry
    def test_a_catalog_with_malformed_visibility_is_rejected(self):
        for vis in ("{}", "7", "null", "[\"list\"]"):
            out = '{"models": [{"slug": "gpt-5.6-sol", "visibility": %s}]}' % vis
            catalog, error = adapt.parse_codex_catalog(0, out)
            self.assertIsNone(catalog, vis)
            self.assertTrue(error)
        self.assertEqual(adapt.parse_codex_catalog(0, '{"models": [{"slug": "a"}, {"slug": "b", "visibility": "hide"}]}'),
                         ({"a": "list", "b": "hide"}, None))

    def test_retiring_every_enabled_model_of_the_installed_registry_is_refused(self):
        only_luna = REGISTRY
        for mid in ("gpt-5.6-sol", "gpt-6-astra"):
            only_luna = adapt.disable_model(only_luna, mid)
        with open(os.path.join(self.user, "models.toml"), "w", encoding="utf-8") as f:
            f.write(only_luna)
        self.assertEqual(self.run_main(), 1)
        luna = {m["id"]: m for m in adapt.registry_models(self.installed())}["gpt-5.6-luna"]
        self.assertTrue(luna["enabled"])
        self.assertIn("would leave openai with no enabled model", self.latest_report())


    # 3: the installed change itself must pass the tests
    def luna_disabled_on_master(self):
        """master already has luna off; the installed copy still has it on. No branch to make."""
        with open(os.path.join(self.repo, "config", "models.toml"), "w", encoding="utf-8") as f:
            f.write(adapt.disable_model(REGISTRY, "gpt-5.6-luna"))
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "luna off")
        return self.fixture(help={}, streams={}, versions={},
                            models={"openai": {"catalog": {"gpt-6-astra": "list", "gpt-5.6-sol": "list"}}})

    def luna(self):
        return {m["id"]: m for m in adapt.registry_models(self.installed())}["gpt-5.6-luna"]

    def test_an_installed_change_without_a_branch_needs_passing_tests(self):
        fx = self.luna_disabled_on_master()
        self.assertEqual(self.run_main(test_cmd=self.script("raise SystemExit(1)"), fixture=fx), 1)
        self.assertTrue(self.luna()["enabled"])
        self.assertIn("Installed registry not updated", self.latest_report())
        self.assertEqual(self.run_main(fixture=fx), 0)
        self.assertFalse(self.luna()["enabled"])
        self.assertIn("Tests passed with the new installed registry", self.latest_report())

    def test_the_installed_text_is_tested_not_only_the_branch(self):
        with open(os.path.join(self.user, "models.toml"), "a", encoding="utf-8") as f:
            f.write("# mine\n")
        fails_on_mine = self.script("import sys; sys.exit('# mine' in open('config/models.toml').read())")
        self.assertEqual(self.run_main(test_cmd=fails_on_mine), 1)
        self.assertIn("tests passed", self.latest_report())  # the branch
        self.assertIn("Installed registry not updated", self.latest_report())
        self.assertTrue(self.luna()["enabled"])


    # 4: a timeout is a bound, even when children outlive their launcher
    def launcher(self, then=""):
        """A command that starts a child holding its output, which writes a marker after 3 s."""
        mark = os.path.join(self.tmp, f"survived-{len(os.listdir(self.tmp))}").replace("\\", "/")
        path = os.path.join(self.tmp, f"launch-{len(os.listdir(self.tmp))}.py")
        with open(path, "w", encoding="utf-8") as f:
            f.write("import subprocess, sys, time\n"
                    f"subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(3); open(\"{mark}\", \"w\")'], stdout=sys.stdout)\n"
                    f"print('started', flush=True)\n{then}\n")
        return [sys.executable, path], mark

    def assert_killed(self, mark):
        time.sleep(4.5)
        self.assertFalse(os.path.exists(mark), "a child outlived the run")

    def test_children_left_running_by_a_finished_command_are_killed(self):
        cmd, mark = self.launcher()
        start = time.time()
        code, out = adapt.run_bounded(cmd, 30)
        self.assertLess(time.time() - start, 2.5)
        self.assertEqual((code, out.strip()), (0, "started"))
        self.assert_killed(mark)

    def test_a_timeout_kills_the_children_and_returns_on_time(self):
        cmd, mark = self.launcher("time.sleep(60)")
        start = time.time()
        with self.assertRaises(subprocess.TimeoutExpired):
            adapt.run_bounded(cmd, 0.5)
        self.assertLess(time.time() - start, 2.5)
        self.assert_killed(mark)

    def test_cli_calls_are_bounded_too(self):
        cmd, mark = self.launcher("time.sleep(60)")
        start = time.time()
        code, _ = adapt.check.run(cmd, timeout=0.5)
        self.assertLess(time.time() - start, 2.5)
        self.assertEqual(code, -1)
        self.assert_killed(mark)


    # 8: a failed checkout leaves no branch behind, and failed cleanup is reported
    def failing_checkout(self, fail_branch_delete=False):
        """adapt.git where `worktree add -b` creates the branch, then fails to check it out."""
        real = adapt.git

        def fake(*args, cwd=adapt.ROOT):
            if args[:2] == ("worktree", "add") and "-b" in args:
                i = args.index("-b")
                real("branch", args[i + 1], args[i + 3], cwd=cwd)
                return 128, "fatal: simulated checkout failure"
            if fail_branch_delete and args[:2] == ("branch", "-D"):
                return 1, "error: simulated delete failure"
            return real(*args, cwd=cwd)
        return fake

    def test_a_failed_checkout_leaves_no_branch(self):
        from unittest import mock
        with mock.patch.object(adapt, "git", side_effect=self.failing_checkout()):
            self.assertEqual(self.run_main(fixture=self.clean_fixture()), 1)
        self.assertEqual([b for b in self.branches() if b.startswith("compat/auto-")], [])
        self.assertIn("simulated checkout failure", self.latest_report())
        # The next run makes the branch for real instead of trusting a leftover.
        self.assertEqual(self.run_main(fixture=self.clean_fixture()), 0)

    def test_a_failed_cleanup_is_reported_and_fails_the_run(self):
        from unittest import mock
        with mock.patch.object(adapt, "git", side_effect=self.failing_checkout(fail_branch_delete=True)):
            self.assertEqual(self.run_main(fixture=self.clean_fixture()), 1)
        self.assertIn("Cleanup failed", self.latest_report())
        self.assertIn("simulated delete failure", self.latest_report())


    # New 2: a cached branch result counts only for the same base commit and test command
    def auto_branches(self):
        return [b for b in self.branches() if b.startswith("compat/auto-")]

    def test_a_stricter_test_command_is_not_answered_from_the_cache(self):
        clean = self.clean_fixture()
        self.assertEqual(self.run_main(fixture=clean), 0)
        self.assertEqual(self.run_main(test_cmd=self.script("raise SystemExit(1)"), fixture=clean), 1)
        self.assertIn("tests FAILED", self.latest_report())
        self.assertEqual(len(self.auto_branches()), 2)

    def test_a_new_base_commit_is_tested_again(self):
        clean = self.clean_fixture()
        self.assertEqual(self.run_main(fixture=clean), 0)
        with open(os.path.join(self.repo, "README"), "w") as f:
            f.write("moved on\n")
        git(self.repo, "add", "README")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "later")
        self.assertEqual(self.run_main(test_cmd=self.script("raise SystemExit(1)"), fixture=clean), 1)
        self.assertIn("tests FAILED", self.latest_report())
        self.assertEqual(len(self.auto_branches()), 2)


    # New 3: a model the installed registry leaves out does not block its other changes
    def test_a_retirement_of_an_omitted_model_does_not_block_other_changes(self):
        start = REGISTRY.index('[[model]]\nid = "gpt-5.6-luna"')
        without_luna = REGISTRY[:start] + REGISTRY[REGISTRY.index("[[model]]", start + 1):]
        with open(os.path.join(self.user, "models.toml"), "w", encoding="utf-8") as f:
            f.write(without_luna)
        self.run_main()
        self.assertNotIn("could not be edited safely", self.latest_report())
        ids = {m["id"] for m in adapt.registry_models(self.installed())}
        self.assertIn("gpt-7-nova", ids)
        self.assertNotIn("gpt-5.6-luna", ids)
        with self.assertRaises(ValueError):  # a direct call stays strict
            adapt.disable_model(without_luna, "gpt-5.6-luna")


    # New 4: a write that was refused does not leave a retrain behind
    def test_a_refused_write_does_not_ask_for_a_retrain(self):
        installed = os.path.join(self.user, "models.toml")
        comment = self.script(f"open({installed!r}, 'a').write('# just a comment\\n')")
        self.assertEqual(self.run_main(test_cmd=comment), 1)
        self.assertIn("changed while this run was working", self.latest_report())
        self.assertFalse(self.state_json().get("retrain", {}).get("needed"))
        self.assertIn("not needed", self.latest_report())



class AstraRound3(Sandbox):
    """The gaps from Astra's third review (astra-compat-review-3.md)."""

    def path(self):
        return os.path.join(self.user, "models.toml")

    def backups(self):
        return [f for f in os.listdir(self.user) if ".bak-" in f]

    # New 1: a swap that fails or is interrupted never leaves the registry missing
    def test_a_replacement_that_cannot_be_written_leaves_the_registry_in_place(self):
        from unittest import mock
        real = tempfile.mkstemp

        def refuse_beside_registry(*a, **kw):
            if os.path.samefile(kw.get("dir") or ".", self.user):
                raise OSError(28, "No space left on device")
            return real(*a, **kw)
        with mock.patch.object(adapt.tempfile, "mkstemp", side_effect=refuse_beside_registry):
            self.assertEqual(self.run_main(), 1)
        self.assertEqual(self.installed(), REGISTRY)
        self.assertIn("No space left on device", self.latest_report())
        self.assertEqual(self.backups(), [])

    def test_a_failed_final_move_puts_the_original_back(self):
        from unittest import mock
        real = adapt.rename_noreplace

        def refuse(src, dst):
            if src.endswith(".tmp"):
                raise PermissionError(13, "Access is denied")
            return real(src, dst)
        with mock.patch.object(adapt, "rename_noreplace", side_effect=refuse):
            self.assertEqual(self.run_main(), 1)
        self.assertEqual(self.installed(), REGISTRY)
        self.assertIn("Access is denied", self.latest_report())
        self.assertEqual(self.backups(), [])
        self.assertEqual([f for f in os.listdir(self.user) if f.endswith((".tmp", ".swap"))], [])

    def test_an_interrupted_swap_is_repaired_by_the_next_run(self):
        # What a run killed mid-swap leaves: the original moved to its backup, the
        # journal naming it, and no registry.
        backup = adapt.new_file(self.path() + ".bak-", "")
        os.replace(self.path(), backup)
        with open(self.path() + ".swap", "w", encoding="utf-8") as f:
            f.write(backup)
        self.run_main(fixture=self.clean_fixture())
        self.assertIn("put back", self.latest_report())
        self.assertIn('id = "gpt-5.6-luna"', self.installed())
        self.assertFalse(os.path.exists(self.path() + ".swap"))
        self.assertFalse(os.path.exists(backup))


    # 2: every entry is validated, before duplicates can collapse
    def test_a_catalog_with_a_duplicate_or_bad_entry_is_rejected(self):
        for models in ('[{"slug": "gpt-5.6-sol", "visibility": {}}, {"slug": "gpt-5.6-sol", "visibility": "list"}]',
                       '[{"slug": "gpt-5.6-sol", "visibility": "list"}, {"slug": "gpt-5.6-sol", "visibility": "list"}]',
                       '[{"slug": "a"}, "b"]'):
            catalog, error = adapt.parse_codex_catalog(0, '{"models": %s}' % models)
            self.assertIsNone(catalog, models)
            self.assertTrue(error)


    # 3: the installed text is compared with the branch's byte for byte
    def test_an_installed_text_differing_only_in_whitespace_is_tested_itself(self):
        with open(self.path(), "w", encoding="utf-8", newline="") as f:
            f.write("\n" + REGISTRY)
        fails_on_it = self.script("import sys; sys.exit(open('config/models.toml', newline='').read().startswith(chr(10)))")
        self.assertEqual(self.run_main(test_cmd=fails_on_it), 1)
        self.assertIn("Tests FAILED with the new installed registry", self.latest_report())
        self.assertNotIn('id = "gpt-7-nova"', self.installed())


    # 4: a command that cannot be contained is not run
    @unittest.skipUnless(os.name == "nt", "Windows job objects")
    def test_a_command_windows_will_not_contain_is_not_run(self):
        import ctypes, types
        from unittest import mock
        real = ctypes.WinDLL
        mark = os.path.join(self.tmp, "ran").replace("\\", "/")
        cmd = [sys.executable, "-c", f"open('{mark}', 'w')"]
        for create, assign in ((0, 1), (1234, 0)):
            fake = types.SimpleNamespace(CreateJobObjectW=lambda *a, r=create: r,
                                         AssignProcessToJobObject=lambda *a, r=assign: r, CloseHandle=lambda *a: 1)
            with mock.patch("ctypes.WinDLL", side_effect=lambda name, **kw: fake if name == "kernel32" else real(name, **kw)):
                with self.assertRaises(OSError):
                    adapt.run_bounded(cmd, 30)
                code, out = adapt.check.run(cmd, timeout=30)
            self.assertEqual(code, -1)
            self.assertIn("job object", out)
            time.sleep(1)
            self.assertFalse(os.path.exists(mark), (create, assign))

    def test_tests_that_cannot_start_count_as_failed(self):
        from unittest import mock
        with mock.patch.object(adapt, "run_bounded", side_effect=OSError("could not put the tests in a job object")):
            self.assertEqual(adapt.run_tests(self.tmp, OK, None, 30), (False, "tests could not start: could not put the tests in a job object"))


    # 8: a failed prune is reported like any other cleanup failure
    def test_a_failed_prune_is_reported_and_fails_the_run(self):
        from unittest import mock
        real = adapt.git

        def fake(*args, cwd=adapt.ROOT, **kw):
            if args[:2] == ("worktree", "prune"):
                return 1, "error: simulated prune failure"
            return real(*args, cwd=cwd, **kw)
        with mock.patch.object(adapt, "git", side_effect=fake):
            self.assertEqual(self.run_main(fixture=self.clean_fixture()), 1)
        self.assertIn("Cleanup failed", self.latest_report())
        self.assertIn("simulated prune failure", self.latest_report())


if __name__ == "__main__":
    unittest.main()
