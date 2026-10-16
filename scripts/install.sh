#!/usr/bin/env sh
set -eu
REPO="${NECTRA_REPO:-MBarc/nectra}"
VERSION="${NECTRA_VERSION:-latest}"
case "$REPO" in *[!a-zA-Z0-9_./-]*|*..*) echo "Invalid NECTRA_REPO" >&2; exit 1;; esac
case "$REPO" in */*) ;; *) echo "Expected owner/repository" >&2; exit 1;; esac
case "$VERSION" in *[!a-zA-Z0-9_.-]*|'') echo "Invalid NECTRA_VERSION" >&2; exit 1;; esac
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64) TARGET=x86_64-unknown-linux-gnu;;
  # ponytail: macOS releases need a macOS build host; restore these lines once CI builds them.
  Darwin/*) echo "No prebuilt macOS release yet; build from source: https://github.com/MBarc/nectra#install" >&2; exit 1;;
  *) echo "No prebuilt release for this platform; build from source." >&2; exit 1;;
esac
NAME="nectra-$TARGET.tar.gz"
BASE="https://github.com/$REPO/releases"
if [ "$VERSION" = latest ]; then BASE="$BASE/latest/download"; else BASE="$BASE/download/$VERSION"; fi
DIR="${HOME}/.local/bin"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$BASE/$NAME" -o "$tmp/$NAME"
curl -fsSL "$BASE/$NAME.sha256" -o "$tmp/$NAME.sha256"
expected="$(awk 'NR==1 {print $1}' "$tmp/$NAME.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$NAME" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$NAME" | awk '{print $1}')"
fi
[ "${#expected}" -eq 64 ] && [ "$expected" = "$actual" ] || { echo "SHA-256 verification failed" >&2; exit 1; }
tar -xzf "$tmp/$NAME" -C "$tmp" nectra uninstall-nectra.sh
mkdir -p "$DIR"
install "$tmp/nectra" "$DIR/nectra"
install "$tmp/uninstall-nectra.sh" "$DIR/uninstall-nectra"
echo "Installed nectra to $DIR/nectra"
echo "Selecting providers and checking CLI dependencies..."
if [ -n "${NECTRA_PROVIDERS:-}" ]; then
  "$DIR/nectra" setup --if-needed --providers "$NECTRA_PROVIDERS" || { echo "Installed, but provider setup is incomplete. Fix the reported dependencies and run nectra setup." >&2; exit 1; }
elif ( : </dev/tty ) 2>/dev/null; then
  "$DIR/nectra" setup --if-needed </dev/tty || { echo "Installed, but provider setup is incomplete. Run nectra setup after fixing dependencies." >&2; exit 1; }
else
  "$DIR/nectra" setup --if-needed </dev/null || { echo "Installed, but setup needs a terminal or NECTRA_PROVIDERS=anthropic,openai (or none). Run nectra setup." >&2; exit 1; }
fi
echo "Ensure $DIR is on PATH, then run: nectra"
echo "To uninstall, run: uninstall-nectra"
