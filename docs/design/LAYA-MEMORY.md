# Laya memory and latency on a small Linux machine

Measured 2026-10-22 on a throwaway linked clone of PonsOS template 9000 (Debian 13, 2 vCPU `x86-64-v2-AES` on an Opteron
6272, 1907 MiB, no swap, balloon off), destroyed afterwards. Release build of this branch, the pinned model and ONNX Runtime
1.31.0. Test: `NECTRA_LAYA_MODEL=<dir> NECTRA_LAYA_N=20 cargo test --release --lib -- --ignored latency --nocapture`
(first 20 messages of `eval/laya/dev.json`; prints load, first-message and steady-state time and `/proc/self/status`).
One run per row after a warm-up run, so differences under about 10% are noise (two baselines: 874/878 MB anon, 12.4/13.8 s load).

## Where the memory goes

After the load, before any message, in kB:

| | VmHWM (peak) | VmRSS | RssAnon (private) | RssFile (mapped file) |
|---|---:|---:|---:|---:|
| Baseline | 1,051,424 | 874,708 | 847,752 | 26,956 |

The weights are **private anonymous memory**, not a file mapping: 850 MB anon against a 667 MiB `model.onnx.data`. ONNX Runtime
builds a prepacked copy of every MatMul weight for its kernels, and the kernel cannot drop that copy without swap (the template
has none). The 1.03 GiB peak in the PonsOS report is this: 850 MB steady plus about 170 MB transient during the load.

## ONNX Runtime options

Tried through a local patch to the `laya-onnx` session builder (the crate exposes only the thread counts, not these); not
shipped. Messages are the first 20 of dev.json; load and per-message time on the 2-vCPU VM.

| Option | Peak RSS (kB) | RssAnon after load (kB) | Load | Median message |
|---|---:|---:|---:|---:|
| Baseline | 1,051,424 | 847,752 | 12.4 s | 2.47 s |
| Arena off (`CPU::with_arena_allocator(false)`) | 1,051,264 | 850,228 | 12.4 s | 2.54 s |
| Memory pattern off | 1,053,416 | 841,424 | 13.6 s | 2.77 s |
| Graph optimisation level 1 | 1,051,420 | 848,296 | 12.7 s | 2.45 s |
| Graph optimisation level 2 | 1,054,688 | 841,432 | 13.7 s | 2.51 s |
| Graph optimisation off | 1,037,404 | 865,108 | 12.8 s | 2.49 s |
| **Prepacking off** (`session.disable_prepacking`) | **918,788** | **403,368** | 13.3 s | 2.90 s |
| Arena + memory pattern + prepacking off | 918,612 | 403,428 | 12.6 s | 2.97 s |
| Intra-op threads | not varied: ONNX Runtime's default is already one per core (2 here) | | | |

- **Only disabling prepacking matters.** The weights then stay in the memory-mapped `model.onnx.data`: private memory drops
  from 850 MB to 403 MB right after the load, and the peak during the load from 1.05 GB to 577 MB. Once messages have touched
  the weights, RSS is about the same (919 MB) but 515 MB of it is `RssFile`, clean page cache the kernel can evict and re-read
  under memory pressure instead of OOM-killing. A second process maps the same file, so those pages are shared between
  processes too (the shared service in [LAYA-SHARED.md](LAYA-SHARED.md) matters less if this is adopted).
- It costs about 15% per message (2.47 -> 2.90 s median, 2.47 -> 2.97 s combined): the unpacked weights are slower kernels.
- Arena, memory pattern, graph optimisation level and threads change nothing meaningful in memory, and no latency win
  beyond noise.
- **To use it, `laya-onnx` must expose the session options** (an upstream change, or vendoring the crate like
  `vendor/portable-pty`). Not done here. Recommendation: do it, and turn prepacking off when the machine has under about 4 GiB.

## The load is mostly hashing

`sha256sum model.onnx.data` alone takes 7.9 s on this VM (no SHA extensions). The pinned-hash check runs on its own thread next
to the session build, but on 2 vCPUs they compete, so about 12-16 s of load is two thirds hashing. Skipping it would trade the
tamper check for time; not done. A cheaper idea for later: remember a verified (size, mtime, hash) stamp beside the model.

## Message latency against the 1000 ms budget

A loaded engine takes 2.5 s per message here (median 2.47-2.9 s, p95 about 3.5 s), so on this VM the default
`laya_timeout_ms = 1000` is shorter than every message, whether or not the load is charged. Before this branch the
first message also paid ONNX Runtime's first-run setup inside the budget. Now (end to end with a release `nectra route --dry-run`):

- `laya_timeout_ms = 1000`: "keywords · Laya timed out: no answer within 1000 ms; local heuristic classified prompt ..."
- `laya_timeout_ms = 8000`: "laya classified prompt as code_generation (confidence 0.97, difficulty 0.01 -> Simple, ...)"

On a machine like this set `laya_timeout_ms` to about 5000 (or use the keyword matcher). `nectra route` is still about 15 s
because the one-shot process loads the model first; that wait is no longer inside the budget but is still a wait.

## Follow-up, 2026-10-22: what shipped

Same VM recipe (clone of template 9000, 2 vCPU, 1907 MiB, balloon off, destroyed afterwards), release build.

**Prepacking off is now the default under 4 GiB (Linux).** `laya-onnx` is vendored in `vendor/laya-onnx` with one addition,
`LayaOptions::disable_prepacking`. It is a config key, `classifier.laya_prepacking`, unset by default: unset means off on
Linux when `MemTotal` is under 4 GiB and on everywhere else; `true`/`false` force it. (A key with a machine-dependent default,
rather than a hard-coded rule, so a 2 GiB box that wants speed over memory can say so.)

| Through the shipped option | Peak RSS | Private after load | Load | Median message |
|---|---:|---:|---:|---:|
| Prepacking on (2 runs) | 1,054,516 / 1,053,040 kB | 841,408 / 848,336 kB | 12.6 / 13.5 s | 2.60 / 2.86 s |
| Prepacking off (2 runs) | 576,104 / 575,888 kB | 403,372 / 403,368 kB | 12.7 / 11.1 s | 2.69 / 2.62 s |

Private memory is halved again and the peak falls 45%. The per-message cost is inside the run-to-run noise this time (the first
measurement saw +15%); treat it as 0 to 15%.

**Default timeout scales to the machine.** `classifier.laya_timeout_ms` is unset by default: 5000 ms with 2 or fewer cores or
no AVX2, 1000 ms otherwise. Setting it keeps the old meaning. A timeout still reads "keywords · Laya timed out: no answer
within N ms".

**The pin check runs once, at install.** See the trust model below. On the 2 GiB VM with a root-sealed, stamped install:

| Load (test harness, prepacking off) | Time |
|---|---:|
| Stamp trusted, size + mtime only | 8.4 s, 8.4 s |
| No stamp, full hash of 667 MiB | 13.2 s |
| `nectra classifier laya --verify-only` (once, as root) | 12.8 s |

`nectra route --dry-run` with defaults on that VM (nothing configured but `backend = "laya"` and `model_dir`): 11.4 s and 10.7 s
wall, classified by Laya ("laya classified prompt as code_generation"), against 15.5-16.2 s and a keyword fallback before.
The remaining 8 s is building the ONNX session and the warm-up message.

## Trust model for the verification stamp

The pins in the binary say what the right files are; hashing them proves the files on disk are those. Hashing 667 MiB costs
about 8 s on a small machine, so it is done once and remembered in `<model dir>/.nectra-verified.json`: for each file its pin,
size and modification time at the moment it was fully hashed.

- **Written** only by a process running as root (a user-written stamp would never be trusted), after every file has been
  hashed against its pin: `nectra classifier laya` and `nectra classifier fetch --dir <dir>` after downloading, and
  `nectra classifier laya --model-dir <dir> --verify-only`, which hashes the files already in `<dir>`, downloads nothing and
  leaves the configuration alone. The `nectra-laya` package's postinst runs `classifier fetch --dir /usr/lib/nectra/laya` as
  root, so it writes the stamp with no separate verify step; `--verify-only` is for files put in place by other means.
- **Trusted on load only when root alone can have changed anything:** the directory, `tokenizer/`, the stamp and every pinned
  file are owned by uid 0, are not group- or world-writable, and are regular files (no symlinks). Then a file whose pin, size
  file are owned by uid 0, are not group- or world-writable (and every parent up to /, via the canonical path the load then uses, is root-owned and not writable by others unless sticky), and are regular files (no symlinks). Then a file whose pin, size
  unreadable or for other pins, gets the full hash and a mismatch is refused as before.
- **Why root only:** mtime is not a proof of content. Anyone who can write a file can change it and set its mtime back. So a
  user-owned install, such as `nectra classifier laya` run by a normal user into the per-user data directory, or any
  group/world-writable directory, never trusts the stamp and hashes everything on every load, exactly as before. Non-unix
  platforms do the same.
- **What it does not defend against:** root, a compromised root, or a kernel/filesystem fault that changes content without
  changing size or mtime. Those are outside what a hash check at load ever covered.
- A new release (new pins) invalidates old stamps automatically: the stamp entry must carry the pin the binary expects.
