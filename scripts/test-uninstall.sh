#!/usr/bin/env sh
set -eu
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
fixture="$(mktemp -d)"
cp "$SCRIPT_DIR/uninstall-nectra.sh" "$fixture/uninstall-nectra"
for name in nectra other-tool config.toml saved-session.jsonl; do
  echo fixture > "$fixture/$name"
done
sh "$fixture/uninstall-nectra" --dry-run
test -f "$fixture/nectra"
test -f "$fixture/uninstall-nectra"
sh "$fixture/uninstall-nectra"
test ! -e "$fixture/nectra"
test ! -e "$fixture/uninstall-nectra"
for name in other-tool config.toml saved-session.jsonl; do
  test "$(cat "$fixture/$name")" = fixture
done
sh "$SCRIPT_DIR/uninstall-nectra.sh" --install-dir "$fixture"
cp "$SCRIPT_DIR/uninstall-nectra.sh" "$fixture/uninstall-nectra"
sh "$fixture/uninstall-nectra"
test ! -e "$fixture/uninstall-nectra"
echo 'Uninstall checks passed: preview, removal, self-removal, preservation, and repeat invocation.'
