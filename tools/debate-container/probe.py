import errno
import json
import tempfile
from pathlib import Path

def probe(root):
    denied = 0
    for operation in [
        lambda: (root / 'created.txt').write_text('changed'),
        lambda: (root / 'overwrite.txt').write_text('changed'),
        lambda: (root / 'delete.txt').unlink(),
        lambda: (root / 'rename.txt').rename(root / 'renamed.txt'),
        lambda: (root / 'new-directory').mkdir(),
    ]:
        try:
            operation()
        except OSError as error:
            if error.errno not in (errno.EROFS, errno.EACCES, errno.EPERM):
                raise
            denied += 1
    return denied


with tempfile.TemporaryDirectory() as directory:
    control = Path(directory)
    for name in ['overwrite.txt', 'delete.txt', 'rename.txt']:
        (control / name).write_text('nectra-probe')
    assert probe(control) == 0

root = Path('/workspace')
assert (root / 'read.txt').read_text() == 'nectra-probe'
denied = probe(root)
assert not Path('/var/run/docker.sock').exists()
print(json.dumps({'read': True, 'denied': denied}))
assert denied == 5
