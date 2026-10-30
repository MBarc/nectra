"""Let a CI runner's provider CLIs start interactively with API keys.

Interactive first runs stop at onboarding and key prompts that the stream view
never sees; the terminal-view checks need them answered in advance. Codex is
signed in by `codex login --with-api-key` in the workflow.
"""
import json, os

home = os.path.expanduser("~")

key = os.environ.get("ANTHROPIC_API_KEY")
if key:
    claude = os.path.join(home, ".claude.json")
    config = json.load(open(claude)) if os.path.exists(claude) else {}
    config["hasCompletedOnboarding"] = True
    # Claude Code asks once whether to use ANTHROPIC_API_KEY, remembering its last 20 characters.
    approved = config.setdefault("customApiKeyResponses", {}).setdefault("approved", [])
    approved.append(key[-20:])
    json.dump(config, open(claude, "w"))

# Google/agy compatibility is intentionally local-only. CI has no agy installer
# or Google credential, and must remain useful when GEMINI_API_KEY is unset.
