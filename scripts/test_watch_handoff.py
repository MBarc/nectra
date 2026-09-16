"""Regression coverage for handoffs arriving while the watcher is stopped."""
import os
import subprocess
import sys
import tempfile
import unittest

from watch_handoff import entry_key, load_seen, save_seen


class WatchCursorTests(unittest.TestCase):
    def test_cli_restart_reads_entry_written_while_stopped(self):
        with tempfile.TemporaryDirectory() as root:
            handoff = os.path.join(root, "handoff.md")
            cursor = os.path.join(root, "cursor.json")
            header = "## Messages and handoffs\n\n"
            old = "- 2026-09-15 / Claude / STATUS old: baseline\n"
            new = "- 2026-09-15 / Claude / DONE gap-001: ready for review\n"
            with open(handoff, "w", encoding="utf-8") as handle:
                handle.write(header + old)

            def run():
                return subprocess.run(
                    [sys.executable, "-c",
                     "import sys, watch_handoff as w; "
                     "w.HANDOFF = sys.argv.pop(1); raise SystemExit(w.main())",
                     handoff, "Codex", "1", "--cursor", cursor],
                    cwd=os.path.dirname(os.path.abspath(__file__)),
                    capture_output=True, text=True, timeout=5,
                )

            self.assertEqual(run().returncode, 1)
            with open(handoff, "a", encoding="utf-8") as handle:
                handle.write(new)
            delivered = run()
            self.assertEqual(delivered.returncode, 0, delivered.stderr)
            self.assertIn("DONE gap-001", delivered.stdout)
            repeated = run()
            self.assertEqual(repeated.returncode, 1, repeated.stderr)
            self.assertNotIn("DONE gap-001", repeated.stdout)

    def test_restart_delivers_gap_message_once(self):
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "cursor.json")
            old = "Claude / STATUS old: already read"
            new = "Claude / READY new: arrived after timeout"
            load_seen(path, [old])
            seen = load_seen(path, [old, new])
            unread = [e for e in [old, new] if entry_key(e) not in seen]
            self.assertEqual(unread, [new])
            save_seen(path, seen | {entry_key(new)})
            self.assertEqual(load_seen(path, [old, new]), {entry_key(old), entry_key(new)})

    def test_corrupt_cursor_does_not_silently_discard_unread_work(self):
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "cursor.json")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("broken")
            with self.assertRaises(ValueError):
                load_seen(path, ["unread"])


if __name__ == "__main__":
    unittest.main()
