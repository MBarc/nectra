"""Tests for adapt.py: python scripts/compat/test_adapt.py"""
import json, os, shutil, subprocess, sys, tempfile, tomllib, unittest

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


class EndToEnd(unittest.TestCase):
    """The fixture through main(): branch with tests, installed registry, retrain."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.repo = os.path.join(self.tmp, "repo")
        for rel in ("config/models.toml", "scripts/compat/check.py", "src/policy.rs", "src/classifier.rs"):
            os.makedirs(os.path.dirname(os.path.join(self.repo, rel)), exist_ok=True)
            shutil.copy(os.path.join(adapt.ROOT, rel), os.path.join(self.repo, rel))
        os.makedirs(os.path.join(self.repo, "src", "executor"))
        with open(os.path.join(self.repo, "src", "executor", "process.rs"), "w") as f:
            f.write('const ARGS: &[&str] = &["exec", "--ephemeral"];\n')
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

    def run_main(self, test_cmd=OK):
        env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
        os.environ.update(env)
        return adapt.main(["--fixture", FIXTURE, "--repo", self.repo, "--state-dir", self.state,
                           "--config", os.path.join(self.user, "config.toml"), "--test-cmd", test_cmd,
                           "--providers", "anthropic,openai"])

    def state_json(self):
        return json.load(open(os.path.join(self.state, "state.json"), encoding="utf-8"))

    def branches(self):
        return [b.strip(" *") for b in git(self.repo, "branch", "--format=%(refname:short)").split()]

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
        self.assertEqual(open(installed + ".bak-" + adapt.TODAY, encoding="utf-8").read(), REGISTRY)
        # luna had the simple-coding role, so Laya needs retraining; the tool is absent.
        st = self.state_json()
        self.assertTrue(st["retrain"]["needed"])
        self.assertIn("gpt-5.6-luna", st["retrain"]["reason"])
        report = open(os.path.join(self.state, f"report-{adapt.TODAY}.md"), encoding="utf-8").read()
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
        report = open(os.path.join(self.state, f"report-{adapt.TODAY}.md"), encoding="utf-8").read()
        self.assertIn("tests FAILED", report)


if __name__ == "__main__":
    unittest.main()
