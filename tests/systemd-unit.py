"""Validate generated service paths against systemd in an isolated project root.

Run with: python3 tests/systemd-unit.py
No service is installed or started. Requires systemd-analyze.
"""
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(dir=ROOT, prefix='.unit-check-') as temp:
    root = Path(temp)
    units = root / 'etc/systemd/system'
    units.mkdir(parents=True)
    env = root / 'usr/bin/env'
    env.parent.mkdir(parents=True)
    env.write_text('#!/bin/sh\nexit 0\n')
    env.chmod(0o755)
    # Exercise the exact renderer used by the remote installer.
    installer = (ROOT / 'ops/remote-install.sh').read_text()
    renderer = installer.split("<<'PY'\n", 1)[1].split('\nPY\n', 1)[0]
    names = ('ordinary', 'space quote\' dollar$ percent% double" backslash\\', 'trailing\\')
    for name in names:
        path = '/' + name
        (root / name).mkdir()
        unit = units / 'amalthea.service'
        subprocess.run(['python3', '-', path, str(ROOT / 'ops/amalthea.service.in'), str(unit)],
                       input=renderer, text=True, check=True)
        # The isolated root has no boot targets; only validate this service.
        unit.write_text(unit.read_text().replace('[Unit]', '[Unit]\nDefaultDependencies=no'))
        subprocess.run(['systemd-analyze', '--root=' + temp, '--man=no', '--generators=no',
                        'verify', 'amalthea.service'], check=True)
        print(f'Validated path: {path!r}')
