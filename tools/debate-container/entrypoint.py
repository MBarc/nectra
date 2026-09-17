"""Copy only provider credentials into ephemeral HOME, never host settings."""
import json
import os
from pathlib import Path
import shutil
import sys

home = Path.home()
for provider, directory, filename in [
    ('codex', '.codex', 'auth.json'),
    ('claude', '.claude', '.credentials.json'),
]:
    destination = home / directory
    destination.mkdir(mode=0o700, exist_ok=True)
    source = Path('/run/nectra') / (provider + '-auth.json')
    if source.is_file():
        shutil.copyfile(source, destination / filename)
        (destination / filename).chmod(0o600)
if os.environ.get('OPENAI_API_KEY'):
    auth = home / '.codex' / 'auth.json'
    auth.write_text(json.dumps({'OPENAI_API_KEY': os.environ['OPENAI_API_KEY']}))
    auth.chmod(0o600)
if len(sys.argv) < 2 or sys.argv[1] not in ('claude', 'codex'):
    raise SystemExit('unsupported isolated provider')
os.execvp(sys.argv[1], sys.argv[1:])
