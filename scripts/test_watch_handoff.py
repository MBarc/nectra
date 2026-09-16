"""Regression coverage for handoffs arriving while the watcher is stopped."""
import os
import tempfile
import unittest

from watch_handoff import entry_key, load_seen, save_seen


class WatchCursorTests(unittest.TestCase):
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
