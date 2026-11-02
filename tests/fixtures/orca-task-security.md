Task: resume the Simple Packet Capture security-review-1 fixes. Read `.orca-task/RESUME.md` first.

- The rename stays PARKED (it needs Michael in the Orca UI). Ignore RESUME's "do this first" and keep the Strata names.
- Follow RESUME's "What's next" steps 1-3:
  1. The EPERM fix in `identify()`, plus the non-root test.
  2. Build and test on builder.
  3. Re-run the VM itest, then commit the results and their README.
- Builder: ~12 GB free, and calibration has priority. Keep it lean (no extra target dirs; clean up after). Stop and report if it drops below 5 GB.
- VMs: only 918/919, made and destroyed by itest. Check `lvs pve/data` before you start and stop above 88%. Destroy them when done.
- Local commits on MBarc/strata-engine only. No push.
- Report with worker_done: the commits, the test and itest numbers, and anything that failed.

Rules: C:/Users/micha/Desktop/Automated Company/Orchestrator/specs/worker-rules.md
