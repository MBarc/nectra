#!/usr/bin/env bash
# Builds nectra-laya_<N>_amd64.deb into OUT_DIR:  packaging/build-laya-deb.sh OUT_DIR
# A few KB: its postinst downloads the Laya model this nectra pins (RELEASE in
# src/laya.rs) into /usr/lib/nectra/laya and checks every file against the pins.
# Version N is the model's (laya-model-1-alpha.1 -> 1~alpha.1), which
# Cargo.toml's `breaks` must match.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$(cd "$1" && pwd)
tag=$(grep -o 'releases/download/laya-model-[^"]*' "$here/../src/laya.rs" | sed 's|.*/||')
version=${tag#laya-model-}; version=${version//-/\~}
nectra=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../Cargo.toml" | head -1); nectra=${nectra//-/\~}
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT
mkdir -p "$root/DEBIAN" "$root/usr/share/doc/nectra-laya"
chmod 755 "$root" "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<EOF
Package: nectra-laya
Version: $version
Architecture: amd64
Maintainer: MBarc <michaelb4185@gmail.com>
Depends: nectra (>= $nectra)
Section: utils
Priority: optional
Homepage: https://github.com/MBarc/nectra
Description: Laya local classifier model for Nectra
 Downloads $tag (about 700 MB) from the Nectra GitHub releases into
 /usr/lib/nectra/laya at install time, and checks every file against the
 sha256 pins built into nectra. Removing the package removes the files.
EOF
install -m755 "$here/nectra-laya.postinst" "$root/DEBIAN/postinst"
install -m755 "$here/nectra-laya.postrm" "$root/DEBIAN/postrm"
install -m644 "$here/../LICENSE" "$root/usr/share/doc/nectra-laya/copyright"
dpkg-deb --root-owner-group --build "$root" "$out/nectra-laya_${version}_amd64.deb"
