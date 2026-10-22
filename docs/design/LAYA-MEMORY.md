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
