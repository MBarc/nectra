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

gemini = os.path.join(home, ".gemini", "settings.json")
os.makedirs(os.path.dirname(gemini), exist_ok=True)
settings = json.load(open(gemini)) if os.path.exists(gemini) else {}
settings.setdefault("security", {}).setdefault("auth", {})["selectedType"] = "gemini-api-key"
json.dump(settings, open(gemini, "w"))
