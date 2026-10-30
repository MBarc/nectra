"""Let a CI runner's provider CLIs start interactively with API keys.

Interactive first runs stop at onboarding and key prompts that the stream view
never sees; the terminal-view checks need them answered in advance. Codex is
signed in by `codex login --with-api-key` in the workflow.
"""
import json, os

home = os.path.expanduser("~")

claude = os.path.join(home, ".claude.json")
config = json.load(open(claude)) if os.path.exists(claude) else {}
config["hasCompletedOnboarding"] = True
# Claude Code asks once whether to use ANTHROPIC_API_KEY, remembering its last 20 characters.
approved = config.setdefault("customApiKeyResponses", {}).setdefault("approved", [])
approved.append(os.environ["ANTHROPIC_API_KEY"][-20:])
json.dump(config, open(claude, "w"))

# agy reads the Google credential from the environment in CI. Keep its
# account-specific configuration out of the runner's shared Gemini settings.
if not os.environ.get("GEMINI_API_KEY"):
    raise SystemExit("GEMINI_API_KEY is required for agy compatibility checks")
