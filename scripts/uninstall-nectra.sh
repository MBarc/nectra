#!/usr/bin/env sh
set -eu
PREVIEW=false
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)"
DIR="$SCRIPT_DIR"
if [ -f "$SCRIPT_DIR/install.sh" ] && [ -f "$SCRIPT_DIR/../Cargo.toml" ]; then DIR="${HOME}/.local/bin"; fi
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) PREVIEW=true; shift;;
    --install-dir)
      [ "$#" -ge 2 ] || { echo "--install-dir requires a directory" >&2; exit 1; }
      DIR="$2"; shift 2;;
    --help|-h)
      echo "Usage: uninstall-nectra [--dry-run] [--install-dir DIRECTORY]"
      echo "Removes only Nectra and its uninstaller. Keeps sessions and configuration."
      exit 0;;
    *) echo "Unknown option: $1" >&2; exit 1;;
  esac
done
case "$DIR" in ''|/) echo "Specify an installation directory, not the filesystem root" >&2; exit 1;; esac
if [ ! -d "$DIR" ]; then echo "Nectra installation directory does not exist; nothing to remove."; exit 0; fi
DIR="$(cd "$DIR" && pwd -P)"
[ "$DIR" != / ] || { echo "Refusing filesystem root" >&2; exit 1; }
for name in nectra uninstall-nectra uninstall-nectra.sh; do
  if [ "$PREVIEW" = true ]; then
    echo "Would remove: $DIR/$name"
  else
    rm -f -- "$DIR/$name"
  fi
done
if [ "$PREVIEW" = true ]; then echo "Preview only. Nothing changed."
else echo "Nectra uninstalled. Saved sessions, configuration and the shared ~/.local/bin PATH entry were kept."; fi
