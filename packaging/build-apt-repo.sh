#!/usr/bin/env bash
# Builds the signed apt repository from scratch:  packaging/build-apt-repo.sh DEBS_DIR OUT_DIR
#
# DEBS_DIR/stable/*.deb are published in both suites, DEBS_DIR/alpha/*.deb only
# in `alpha`. Signs with the key whose fingerprint is APT_KEY, from the default
# GNUPGHOME, with the passphrase in the file APT_PASSPHRASE_FILE. Stateless: the
# same debs in give the same repo out (only Date changes).
set -euo pipefail
debs=$1 out=$2
: "${APT_KEY:?fingerprint of the signing key}" "${APT_PASSPHRASE_FILE:?}"
mkdir -p "$out/pool/stable" "$out/pool/alpha"
for s in stable alpha; do
  [ ! -d "$debs/$s" ] || find "$debs/$s" -name '*.deb' -exec cp -t "$out/pool/$s" {} +
done
cd "$out"
sign() { gpg --batch --yes --pinentry-mode loopback --passphrase-file "$APT_PASSPHRASE_FILE" --local-user "$APT_KEY" "$@"; }
for suite in stable alpha; do
  # alpha scans the whole pool, stable only pool/stable.
  [ $suite = stable ] && src=pool/stable || src=pool
  bin=dists/$suite/main/binary-amd64
  mkdir -p "$bin"
  apt-ftparchive packages "$src" > "$bin/Packages"
  gzip -9nkf "$bin/Packages"
  apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=Nectra -o APT::FTPArchive::Release::Label=Nectra \
    -o APT::FTPArchive::Release::Suite=$suite -o APT::FTPArchive::Release::Codename=$suite \
    -o APT::FTPArchive::Release::Architectures=amd64 -o APT::FTPArchive::Release::Components=main \
    release "dists/$suite" > Release.tmp
  mv Release.tmp "dists/$suite/Release"
  sign --clearsign -o "dists/$suite/InRelease" "dists/$suite/Release"
  sign --detach-sign --armor -o "dists/$suite/Release.gpg" "dists/$suite/Release"
done
echo "apt repo in $out: $(find pool -name '*.deb' | wc -l) debs"
