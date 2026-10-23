"""Offline deployment integration checks: python3 tests/deployment.py.

SSH, SCP, Cargo and systemd are simulated. No Git index or real server is used.
"""
import json
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, shlex, shutil, subprocess, sys
tool = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
root = pathlib.Path(os.environ['TEST_ROOT'])
with (root / 'events').open('a') as log:
    log.write(json.dumps([tool, *args]) + '\n')
if tool == 'git':
    if args[2] == 'status':
        if os.environ.get('TEST_DIRTY'):
            print(' M src/main.rs')
    elif args[2] == 'archive':
        output = next(a.split('=', 1)[1] for a in args if a.startswith('--output='))
        shutil.copyfile(root / 'payload.tar.gz', output)
    else:
        sys.exit(subprocess.call([os.environ['TEST_GIT'], *args]))
elif tool == 'ssh':
    command = args[-1]
    if command.startswith('mktemp '):
        print('/tmp/amalthea-transfer.TEST1234.tar.gz')
    elif command.startswith('rm '):
        (root / 'remote.tar.gz').unlink(missing_ok=True)
    else:
        command = command.replace('/tmp/amalthea-transfer.TEST1234.tar.gz', str(root / 'remote.tar.gz'))
        sys.exit(subprocess.call(['bash', '-c', command]))
elif tool == 'scp':
    shutil.copyfile(args[-2], root / 'remote.tar.gz')
elif tool == 'cargo':
    if os.environ.get('TEST_BUILD_FAIL'):
        sys.exit(1)
    target = pathlib.Path(args[args.index('--target-dir') + 1]) / 'release'
    target.mkdir(parents=True, exist_ok=True)
    (target / 'amalthea').write_text(os.environ.get('TEST_BINARY', 'first'))
elif tool == 'systemctl':
    unit = pathlib.Path(os.environ['XDG_CONFIG_HOME']) / 'systemd/user/amalthea.service'
    if 'cat' in args:
        sys.exit(0 if unit.exists() else 1)
    if 'show' in args:
        print(os.environ['TEST_SERVICE_DIR'])
'''


class Deployment(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='.deployment-test-', dir=ROOT)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.install = self.root / "server's files % $DOLLAR"
        mockbin = self.root / 'mockbin'
        mockbin.mkdir()
        for tool in ('git', 'ssh', 'scp', 'cargo', 'systemctl'):
            path = mockbin / tool
            path.write_text(MOCK)
            path.chmod(0o755)
        # Pack the candidate working files without committing or staging them.
        with tarfile.open(self.root / 'payload.tar.gz', 'w:gz') as archive:
            for name in ('Cargo.toml', 'Cargo.lock', 'build.rs', 'src', 'migrations',
                         'res', 'ops', 'LICENSE', 'README.md', 'config.json.example',
                         'imprint.html.example'):
                archive.add(ROOT / name, arcname=name)
        self.env = dict(os.environ, PATH=f'{mockbin}:{os.environ["PATH"]}',
                        TEST_ROOT=str(self.root), TEST_GIT=shutil.which('git'),
                        TEST_SERVICE_DIR=str(self.install), TMPDIR=str(self.root),
                        XDG_CONFIG_HOME=str(self.root / 'config'))

    def run_script(self, mode, *args, success=True, **env):
        result = subprocess.run(['bash', str(ROOT / f'{mode}.sh'), *args],
                                env=dict(self.env, **env), capture_output=True, text=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def events(self):
        path = self.root / 'events'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def install_service(self):
        self.run_script('setup', 'user@example', str(self.install))

    def test_setup_and_deploy_preserve_private_files(self):
        self.install.mkdir()
        config = self.install / 'config.json'
        config.write_text('{"custom":true}')
        self.install_service()
        self.assertEqual(config.read_text(), '{"custom":true}')
        self.assertFalse(any('start' in event for event in self.events()))
        unit = (self.root / 'config/systemd/user/amalthea.service').read_text()
        self.assertNotIn('@INSTALL_DIR@', unit)
        self.assertNotIn('@EXEC_INSTALL_DIR@', unit)
        self.assertIn('%% $$DOLLAR/bin/amalthea', unit)
        (self.install / 'data').mkdir()
        (self.install / 'data/private').write_text('private')
        (self.install / 'imprint.html').write_text('custom imprint')
        (self.install / 'res/obsolete').write_text('old asset')
        self.run_script('deploy', 'user@example', str(self.install), TEST_BINARY='second')
        self.assertEqual((self.install / 'bin/amalthea').read_text(), 'second')
        self.assertEqual((self.install / 'bin/amalthea.previous').read_text(), 'first')
        self.assertFalse((self.install / 'res/obsolete').exists())
        self.assertTrue((self.install / 'res.previous/obsolete').exists())
        self.assertEqual(config.read_text(), '{"custom":true}')
        self.assertEqual((self.install / 'data/private').read_text(), 'private')
        self.assertEqual((self.install / 'imprint.html').read_text(), 'custom imprint')
        events = self.events()
        build = max(i for i, event in enumerate(events) if event[0] == 'cargo')
        stop = next(i for i, event in enumerate(events) if 'stop' in event)
        start = next(i for i, event in enumerate(events) if 'start' in event)
        self.assertLess(build, stop)
        self.assertLess(stop, start)
        self.assertFalse(list(self.install.glob('.release.*')))
        self.assertFalse((self.root / 'remote.tar.gz').exists())
        self.assertFalse(list(self.root.glob('amalthea-*.tar.gz')))

    def test_build_failure_leaves_running_install_untouched(self):
        self.install_service()
        self.run_script('deploy', 'user@example', str(self.install),
                        TEST_BUILD_FAIL='1', success=False)
        self.assertEqual((self.install / 'bin/amalthea').read_text(), 'first')
        self.assertTrue((self.install / 'res/static/index.html').exists())
        self.assertFalse(any('stop' in event for event in self.events()))
        self.assertFalse(list(self.install.glob('.release.*')))
        self.assertFalse((self.root / 'remote.tar.gz').exists())

    def test_setup_creates_private_config_and_refuses_reinstall(self):
        self.install_service()
        config = self.install / 'config.json'
        self.assertEqual(config.read_bytes(), (ROOT / 'config.json.example').read_bytes())
        self.assertEqual(config.stat().st_mode & 0o777, 0o600)
        result = self.run_script('setup', 'user@example', str(self.install), success=False)
        self.assertIn('Already installed', result.stderr)

    def test_missing_install_and_wrong_service_directory(self):
        result = self.run_script('deploy', 'user@example', str(self.install), success=False)
        self.assertIn('Run setup.sh', result.stderr)
        self.install_service()
        result = self.run_script('deploy', 'user@example', str(self.install),
                                 TEST_SERVICE_DIR=str(self.root / 'elsewhere'), success=False)
        self.assertIn('another directory', result.stderr)

    def test_bad_arguments_and_dirty_inputs_never_connect(self):
        for mode in ('setup', 'deploy'):
            for args in ((), ('host',), ('host', 'relative'), ('-host', '/absolute'),
                         ('', '/absolute'), ('host', '/bad\npath')):
                self.run_script(mode, *args, success=False)
            result = self.run_script(mode, 'user@example', str(self.install),
                                     TEST_DIRTY='1', success=False)
            self.assertIn('Commit the deployment inputs', result.stderr)
        self.assertFalse(any(event[0] == 'ssh' for event in self.events()))

    def test_real_git_archive_excludes_private_and_untracked_files(self):
        payload = subprocess.check_output([
            self.env['TEST_GIT'], '-C', str(ROOT), 'archive', '--format=tar', 'HEAD',
            '--', 'Cargo.toml', 'Cargo.lock', 'build.rs', 'src', 'migrations',
            'res', 'ops', 'LICENSE', 'README.md', 'config.json.example', 'imprint.html.example'])
        with tarfile.open(fileobj=io.BytesIO(payload)) as archive:
            names = archive.getnames()
        self.assertIn('res/static/vendor/solid.js', names)
        self.assertIn('Cargo.lock', names)
        for name in names:
            self.assertNotIn(name.split('/')[0], ('.git', 'config.json', 'data', 'target',
                                                 'imprint.html', self.root.name))


if __name__ == '__main__':
    unittest.main()
