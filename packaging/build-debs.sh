#!/usr/bin/env bash
# Builds the two small packages next to `cargo deb`'s nectra, into OUT_DIR:
#   packaging/build-debs.sh OUT_DIR
#
# nectra-laya_<N>_amd64.deb: its postinst downloads the Laya model this nectra
#   pins (RELEASE in src/laya.rs) into /usr/lib/nectra/laya and checks every file
#   against the pins. N is the model's (laya-model-1-alpha.1 -> 1~alpha.1), which
#   Cargo.toml's `breaks` must match.
# nectra-archive-keyring_<K>_all.deb: the apt signing keys, a package of their
#   own so that removing nectra doesn't remove the keyring apt's Signed-By needs.
#
# release.yml builds both for every release; apt-publish.yml publishes each
# version from the oldest release that has it, so its bytes never change.
set -euo pipefail
# Bump whenever nectra-archive-keyring.gpg changes, and Cargo.toml's depends with it.
keyring_version=1
here=$(cd "$(dirname "$0")" && pwd)
out=$(cd "$1" && pwd)
tag=$(grep -o 'releases/download/laya-model-[^"]*' "$here/../src/laya.rs" | sed 's|.*/||')
version=${tag#laya-model-}; version=${version//-/\~}
nectra=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../Cargo.toml" | head -1); nectra=${nectra//-/\~}
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT

# package NAME VERSION ARCH: builds $root/NAME (DEBIAN/control from stdin).
package() {
  local dir=$root/$1
  mkdir -p "$dir/DEBIAN" "$dir/usr/share/doc/$1"
  { cat; echo "Maintainer: MBarc <michaelb4185@gmail.com>"; echo "Homepage: https://github.com/MBarc/nectra"; } > "$dir/DEBIAN/control"
  install -m644 "$here/../LICENSE" "$dir/usr/share/doc/$1/copyright"
}
finish() {
  chmod 755 "$root/$1" "$root/$1/DEBIAN"
  dpkg-deb --root-owner-group --build "$root/$1" "$out/${1}_${2}_${3}.deb"
}

package nectra-laya <<EOF
Package: nectra-laya
Version: $version
Architecture: amd64
Depends: nectra (>= $nectra)
Section: utils
Priority: optional
Description: Laya local classifier model for Nectra
 Downloads $tag (about 700 MB) from the Nectra GitHub releases into
 /usr/lib/nectra/laya at install time, and checks every file against the
 sha256 pins built into nectra. Removing the package removes the files.
EOF
install -m755 "$here/nectra-laya.postinst" "$root/nectra-laya/DEBIAN/postinst"
install -m755 "$here/nectra-laya.postrm" "$root/nectra-laya/DEBIAN/postrm"
finish nectra-laya "$version" amd64

package nectra-archive-keyring <<EOF
Package: nectra-archive-keyring
Version: $keyring_version
Architecture: all
Section: misc
Priority: optional
Multi-Arch: foreign
Description: OpenPGP keys of the Nectra apt repository
 The keys that sign https://mbarc.github.io/nectra/apt, used through
 Signed-By in /etc/apt/sources.list.d/nectra.sources.
EOF
install -Dm644 "$here/nectra-archive-keyring.gpg" "$root/nectra-archive-keyring/usr/share/keyrings/nectra-archive-keyring.gpg"
finish nectra-archive-keyring "$keyring_version" all
