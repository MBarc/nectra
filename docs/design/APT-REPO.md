# Nectra APT repository

Status: approved 2026-10-22 and built for an **alpha** release ("the official
release can happen with astra"). Built: `nectra classifier fetch --dir`,
`[package.metadata.deb]`, `packaging/` (the `nectra-laya` deb, the keyring,
`nectra.sources`, `build-apt-repo.sh`), `release.yml` attaching the debs, and
`apt-publish.yml`. The signing key exists (§3); the GitHub environment, secret
and Pages site do not yet. Decided going in: Debian packages, a static signed
repo, two packages (`nectra`, optional `nectra-laya`), the PonsOS installer's
"Laya vs Jev" question installs or omits `nectra-laya`.

**Alpha channel.** Nectra `0.5.0-alpha.1` (deb `0.5.0~alpha.1`, which sorts
before `0.5.0`) pins Laya `laya-model-1-alpha.1`, packaged as `nectra-laya`
`1~alpha.1`. Tags with a `-` are GitHub prereleases and go only into the
`alpha` suite; `stable` gets non-prereleases. `alpha` also carries stable
releases, so an alpha machine moves on to `0.5.0` without a sources change.
`packaging/nectra.sources` points at `alpha` until then.

Facts this plan rests on (read from the code, 2026-10-22):

- `src/laya.rs` pins every Laya file's sha256 **in the binary** and downloads
  from `releases/download/laya-model-1`, writing `*.part` then `rename`.
  So a Laya model is only usable by a nectra build that pins it.
- The default config is compiled in (`include_str!("../config/nectra.toml")`);
  `registry` may be absolute (`Config::registry_path`).
- `laya-model-1` is not published yet (`gh release list` shows only `v*`).
- PonsOS today: Debian 13 template, nectra at `/usr/local/bin/nectra`,
  `/etc/pons/nectra/config.toml` owned by the `pons` deb, `models.toml` baked
  into the template, enrollment requires `nectra --version` == `0.4.5`
  exactly (`src/enroll.rs`), `unattended-upgrades.service` masked.

## 1. Packaging

**`nectra`** (cargo-deb, `[package.metadata.deb]` in `Cargo.toml`, amd64):

| File | Notes |
|---|---|
| `/usr/bin/nectra` | |
| `/usr/share/nectra/models.toml` | Not a conffile: it tracks the binary. pons points `registry` here (absolute) and stops baking its own copy. |
| `/usr/share/keyrings/nectra-archive-keyring.gpg` | Repo public key, shipped so key rotation travels as an ordinary upgrade (§3). |
| `/usr/share/doc/nectra/` | README, LICENSE, `config/` as examples. |

No conffiles. The default config is compiled in, and on PonsOS pons owns the
config (`/etc/pons/nectra/config.toml`, already a conffile of `pons`). Add an
`/etc/nectra/` conffile only when a non-pons user needs a system-wide config.

**`nectra-laya`: recommend the postinst download, not a bundle.**

- *Bundle* (700 MB deb): apt verifies and caches it, works from an offline
  mirror. But it cannot live on Pages (100 MB file / 1 GB site), forces the
  repo onto the NAS or a release-asset flat repo, and every retrain adds
  700 MB to the pool.
- *Download* (deb of a few KB): postinst runs nectra's existing downloader,
  which already fetches from the pinned `laya-model-N` release, checks each
  sha256 and renames into place. The repo stays small enough for Pages. Cost:
  network to github.com at install time, outside apt's cache.

Download wins because the trust chain is already there and already tested:
signed repo → signed `nectra` deb → hashes compiled into the binary → files.
Install-time network is needed either way (apt would pull the same 700 MB).

Shape of `nectra-laya`:

- `Version:` = the model number N (`1`, `2`, ...), independent of nectra's
  version. `Architecture: amd64` (the ONNX Runtime pin is linux-x64 only).
- `Depends: nectra (>= <first version pinning model N>)`.
- `nectra` declares `Breaks: nectra-laya (<< N)` for the model it pins. Upgrading
  nectra past a retrain then forces `nectra-laya` to N; if N isn't in the repo
  yet, apt holds nectra back rather than shipping a binary whose model won't
  load. No dpkg triggers needed.
- postinst: `nectra classifier fetch --dir /usr/lib/nectra/laya`. That is a
  **new, small subcommand**: today's `classifier laya` downloads to the
  per-user `data_local_dir()` and rewrites the user config. Postinst failure
  (no network) exits non-zero, so the package stays unconfigured and loud,
  and `apt` retries on the next run.
- postrm (remove/purge): `rm -rf /usr/lib/nectra/laya`. Those files belong to
  no package, so they need explicit cleanup.
- Files live in `/usr/lib/nectra/laya/` (`/usr/lib` because it holds an
  arch-specific `.so`). Nectra finds them through `classifier.model_dir`,
  which the pons-owned config sets when Laya is chosen. No search-path code
  change. Add a `/usr/lib/nectra/laya` fallback in `model_dir()` when
  non-pons users install the deb.

## 2. Hosting: GitHub Pages

The repo is public, so Pages is free. Limits: 1 GB published site, 100 MB per
file, soft 100 GB/month bandwidth. With the download design the site is
`nectra` debs (tens of MB each) plus a few-KB `nectra-laya`, which fits if the
pool keeps the **last 3 nectra versions** (rollback headroom). Pages deploys
the whole site in one step, so clients never see half-published metadata.

The NAS is the fallback, and only if a bundled 700 MB package is ever
required. It is not reachable from every PonsOS machine and adds a box to
keep up. A flat repo on a GitHub release asset is not recommended: assets are
replaced one at a time, so `InRelease` and `Packages` can disagree during a
publish.

URL: `https://mbarc.github.io/nectra/apt` (or a custom domain later).

## 3. Signing

- One dedicated **ed25519** key, `Nectra APT signing`, 2-year expiry, signing
  only. Debian 13's apt (sqv) accepts ed25519. RSA-4096 is the fallback only
  if some client's gpgv rejects it.
- Private key and passphrase in **hush at `Nectra/apt-signing`** (a credential:
  username = fingerprint, password = passphrase, notes = armored secret key
  and revocation certificate). Generated 2026-10-22 in tmpfs on builder, never
  written to disk. Fingerprint
  **`18C46D9FDD3A411C3DB663FFA1A6F3C961636194`**, expires 2028-10-21. Public
  key: `packaging/nectra-archive-keyring.gpg` (and the PonsOS repo).
- CI gets it as an **Environment secret** on a `apt-publish` GitHub
  Environment. The environment is restricted to `master` and needs Michael's
  approval for each run. (This is a settings change, made at build time, not
  now.)
- Clients, as deb822 `/etc/apt/sources.list.d/nectra.sources`:
  ```
  Types: deb
  URIs: https://mbarc.github.io/nectra/apt
  Suites: stable
  Components: main
  Signed-By: /usr/share/keyrings/nectra-archive-keyring.gpg
  ```
  `build-template.sh` writes this file and the keyring (from the PonsOS repo's
  checked-in copy, fingerprint asserted) before the first `apt install nectra`.
  After that the `nectra` deb owns the keyring.
- Rotation: generate key 2 and ship a keyring holding keys 1 and 2 in a nectra
  release signed by key 1. Once the fleet has it (one release cycle), sign
  with key 2, then drop key 1 from the keyring. On compromise, do the same
  immediately, plus a pons release if any machine is too stale to upgrade.

## 4. Release flow

1. `release.yml` (tag `v*`, unchanged trigger) also runs `cargo deb` and
   attaches `nectra_<ver>_amd64.deb` to the GitHub release, and
   `nectra-laya_<N>_amd64.deb` (`packaging/build-laya-deb.sh`) for the model
   `RELEASE` in `src/laya.rs` pins. As built it attaches `nectra-laya` to every
   release, and apt-publish keeps one copy per version, which is simpler than
   detecting a new model. Tags containing `-` become prereleases.
2. A new **`apt-publish.yml`**, `workflow_dispatch` only, run from `master`
   in the `apt-publish` environment. This follows laya-release.yml's reasoning:
   a tag-push workflow runs whatever workflow file is at the tag, so it must
   never see the key. It:
   - downloads the debs from the last 3 `v*` releases (`stable`) and the last 3
     prereleases (`alpha` only), skipping any release whose title or notes
     contain `[yanked]` and failing if a tag is not on `master`;
   - checks each `.deb` against the release's `.sha256`;
   - runs `packaging/build-apt-repo.sh`: `apt-ftparchive packages` / `release`
     with `Origin: Nectra`, `Suite: stable|alpha` (no reprepro or aptly: the repo
     is rebuilt from scratch each time, so there is no state to manage). It
     refuses a signing key that is not in the shipped keyring;
   - `gpg --clearsign` → `InRelease`, `gpg -abs` → `Release.gpg`;
   - deploys with `actions/deploy-pages`.
3. Stateless means a re-run is a no-op: same releases in, same repo out
   (only the `Date:` changes).
4. **Pulling a bad release:** mark it `[yanked]` and re-run publish. apt never
   downgrades on its own, so also tag a fixed `X.Y.Z+1` (roll forward). One
   machine can roll back with `apt install nectra=<old>`, which stays in the
   pool.
5. arm64: later. The binary cross-builds cheaply, but Laya needs an arm64 ONNX
   Runtime pin. Add it when there is an arm64 PonsOS target.

## 5. Update policy on PonsOS

- **Admin machines:** the admin runs `apt upgrade`. Nothing automatic.
- **Agent VMs:** `apt-daily-upgrade.timer` stays on. It runs
  `unattended-upgrade`, and the masked `unattended-upgrades.service` is only
  the shutdown hook. Add `Unattended-Upgrade::Origins-Pattern { "origin=Nectra"; };`
  (`/etc/apt/apt.conf.d/52pons-nectra`) so our origin is allowed. This needs
  checking on the template: is `unattended-upgrades` installed and
  `APT::Periodic::Unattended-Upgrade` set?
- **Hold during a task: one flock, no dpkg hook.** dpkg and the downloader
  both replace files by rename, so a *running* nectra keeps its old binary and
  model. The real risk is a task that *starts* mid-upgrade: new binary, old
  model, so Laya fails and nectra silently falls back to keywords. It also
  catches a task that runs nectra twice across the upgrade.
  - pons runner prefixes the task argv with
    `/usr/bin/flock --shared /run/pons/nectra.lock`.
  - drop-in `apt-daily-upgrade.service.d/pons.conf`:
    `ExecStart=` / `ExecStart=/usr/bin/flock -w 3600 /run/pons/nectra.lock /usr/lib/apt/apt.systemd.daily install`.
  - The kernel releases the lock if a task crashes, so a crash can never
    leave the hold stuck. The ceiling: flock has no writer priority, so on a
    VM that never idles for an hour the upgrade times out and waits for the
    next day. Revisit only if that is observed.
- **Contract guard:** `pons` declares
  `Depends: nectra (>= 0.4.5), nectra (<< 0.5~)`, so auto-upgrades stay inside
  the `run` contract (C1). Enrollment changes from an exact version match to
  "dpkg says satisfied". The runner path changes `/usr/local/bin` → `/usr/bin`.
  `0.5.0~alpha.1` is outside `<< 0.5~`: a PonsOS build that takes the alpha
  widens the range itself, after checking the `run` contract against it.

### Interface for PonsOS (the PonsOS side is a separate task)

What nectra provides, and what PonsOS must do to use it:

| Item | Value |
|---|---|
| Repo | `https://mbarc.github.io/nectra/apt`, suites `alpha` and `stable`, component `main`, amd64 |
| Sources | `packaging/nectra.sources`, installed as `/etc/apt/sources.list.d/nectra.sources` |
| Keyring | `/usr/share/keyrings/nectra-archive-keyring.gpg`; fingerprint `18C46D9FDD3A411C3DB663FFA1A6F3C961636194` (assert it in `build-template.sh`) |
| Binary | `/usr/bin/nectra` |
| Registry | `/usr/share/nectra/models.toml` (set `registry` to it, absolute) |
| Laya | `apt install nectra-laya` puts the model in `/usr/lib/nectra/laya`; nectra uses that directory when the config names no `classifier.model_dir`, so `backend = "laya"` is enough |
| Mirror | `NECTRA_LAYA_RELEASE=<url>` in the environment of `apt install nectra-laya` downloads from a mirror; the compiled-in hashes still apply |
| Upgrade lock | `/run/pons/nectra.lock`: tasks hold it **shared**, the upgrade **exclusive** |

The lock, exactly:

```ini
# /etc/systemd/system/apt-daily-upgrade.service.d/pons.conf
[Service]
ExecStart=
ExecStart=/usr/bin/flock -w 3600 /run/pons/nectra.lock /usr/lib/apt/apt.systemd.daily install
```

```text
# pons runner, every task argv:
/usr/bin/flock --shared /run/pons/nectra.lock <task argv...>
```

`/run/pons` must exist before either runs (a `tmpfiles.d` line,
`d /run/pons 0755 root root -`). flock creates the lock file. An admin running
`apt upgrade` by hand bypasses the lock; that is the admin's choice.

## 6. Work breakdown

| # | Work | Repo | Effort |
|---|---|---|---|
| 1 | Publish `laya-model-1` (prerequisite) | nectra | 0.5 d |
| 2 | `[package.metadata.deb]` + `classifier fetch --dir` + `nectra-laya` control/postinst/postrm | nectra | 1 d |
| 3 | `release.yml`: build and attach debs; pin actions by commit | nectra | 0.5 d |
| 4 | Key generation into hush; `apt-publish` environment + secret; enable Pages | nectra / GitHub | 0.5 d |
| 5 | `apt-publish.yml` (ftparchive, sign, yank list, deploy) | nectra | 1 d |
| 6 | PonsOS: `.sources` + keyring in template, Depends range, `/usr/bin` path, registry path, enroll check, flock argv + drop-in, Origins-Pattern, installer Laya/Jev → `nectra-laya` | PonsOS | 1.5 d |
| 7 | itest: fresh install from the repo; upgrade across a model change; task during upgrade | PonsOS | 1 d |

About 6 days in total. Steps 2–3 and 6 can run in parallel once step 4 fixes
the URL and key.

## Security review points

- **Who can publish:** only an approved `apt-publish` run on `master`. Pushing
  a tag builds debs but cannot sign them. Remaining gap: the debs are built by
  the tag-push workflow. The approver should check that the tag is on
  `master`, and publish can enforce it (`git merge-base --is-ancestor`).
- **Key custody:** the private key exists only in hush and the environment
  secret, never on a dev machine after generation. The fingerprint is pinned
  in both repos. If GitHub is compromised, the secret leaks too: treat it as
  a compromise and rotate (§3).
- **Impact of a repo compromise:** anyone holding the key can push code that
  runs as root (maintainer scripts) on every agent VM within a day, via the
  timer. That makes this key as sensitive as pons's own release path. The
  `<< 0.5~` range does not limit an attacker, only accidents.
- **Laya integrity:** hashes are compiled into the signed binary, so the
  700 MB that bypasses apt is still verified. A swapped GitHub release asset
  fails the hash check, and the result is keyword fallback, not code
  execution.
- **Actions supply chain:** `release.yml` uses unpinned `@v4`/`@v2` actions.
  Pin them by commit, as `laya-release.yml` does, before they build debs that
  get signed.
- **Not covered:** reproducible builds, transparency logs, per-machine repo
  pinning. Add them if the fleet goes beyond Michael's machines.
