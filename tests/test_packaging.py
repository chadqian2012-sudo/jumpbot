"""Exercise release sealing in an isolated repo; never execute a game binary."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ('jumpbot.py', 'run_control.py', 'skin_vision.py', 'config.json',
           'tools/dashboard.py', 'tools/_scorelog.py', 'tools/live_acceptance.py',
           'tools/run_frozen_acceptance.py', 'tools/run_skin_acceptance.py')


@unittest.skipUnless(os.name == 'nt' and shutil.which('git'), 'Windows packaging')
class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='jumpbot sealing test ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in SOURCES:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('# fixture\n', encoding='utf8')
        self.git('init', '-q')
        self.git('add', '.')
        self.commit()
        self.head = self.git('rev-parse', 'HEAD').stdout.strip()
        for flavor, directory in [('full', 'dist-accelerated'), ('portable', 'dist-portable')]:
            app = self.root / directory / 'jumpbot'
            app.mkdir(parents=True)
            binary = app / 'jumpbot.exe'
            binary.write_bytes(b'not executable: isolated packaging fixture')
            manifest = dict(commit=self.head, flavor=flavor,
                            exe_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                            source_sha256={name: hashlib.sha256((self.root / name).read_bytes()).hexdigest()
                                           for name in SOURCES[:6]})
            (app / 'build-source.json').write_text(json.dumps(manifest), encoding='utf8')

    def git(self, *args):
        return subprocess.run(['git', '-C', str(self.root), *args], capture_output=True,
                              text=True, check=True, timeout=30)

    def commit(self):
        self.git('-c', 'user.name=Packaging Test', '-c', 'user.email=test@example.invalid',
                 '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture')

    def prepare(self, version='1.3-rc.1'):
        return subprocess.run(['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass',
                               '-File', str(ROOT / 'tools/package_release.ps1'),
                               '-Root', str(self.root), '-Version', version, '-PrepareOnly'],
                              capture_output=True, text=True, timeout=30)

    def test_prerelease_manifests_precede_installers(self):
        result = self.prepare()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for flavor, directory in [('full', 'dist-accelerated'), ('portable', 'dist-portable')]:
            app = self.root / directory / 'jumpbot'
            data = json.loads((app / 'source-version.json').read_text(encoding='utf8'))
            self.assertEqual((data['version'], data['commit'], data['flavor']),
                             ('1.3-rc.1', self.head, flavor))
            self.assertEqual(data['exe_sha256'], hashlib.sha256((app / 'jumpbot.exe').read_bytes()).hexdigest())
            self.assertEqual(set(data['source_sha256']), set(SOURCES))
        self.assertFalse(list((self.root / 'release').rglob('*.zip')))

    def test_dirty_skin_module_cannot_be_sealed(self):
        (self.root / 'skin_vision.py').write_text('# changed\n', encoding='utf8')
        result = self.prepare()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('commit tracked source', result.stderr)

    def test_old_build_cannot_be_relabeled_as_new_commit(self):
        (self.root / 'skin_vision.py').write_text('# changed\n', encoding='utf8')
        self.git('add', 'skin_vision.py')
        self.commit()
        result = self.prepare()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('build identity does not match', result.stderr)

    def test_version_cannot_escape_the_delivery_directory(self):
        result = self.prepare('../outside')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'outside').exists())

    def test_installer_prerelease_has_numeric_version_resource(self):
        script = (ROOT / 'tools/installer/build_installer.ps1').read_text(encoding='utf8')
        lines = [line for line in script.splitlines()
                 if line.startswith(('$parsedVersion =', '$numericVersion ='))]
        self.assertEqual(len(lines), 2)
        result = subprocess.run(['powershell', '-NoProfile', '-Command',
                                 "$Version = '1.3-rc.1'; " + '; '.join(lines) + '; $numericVersion'],
                                capture_output=True, text=True, check=True, timeout=30)
        self.assertEqual(result.stdout.strip(), '1.3.0.0')

    def test_release_excludes_root_and_internal_runtime_logs_only(self):
        for flavor, directory, installer_dir in (
                ('full', 'dist-accelerated', 'installer-accelerated'),
                ('portable', 'dist-portable', 'installer-portable')):
            app = self.root / directory / 'jumpbot'
            for name in ('debug/status.json', 'logs/run.csv', '_internal/debug/panel_exit.log',
                         '_internal/logs/helper.log', '_internal/vendor/debug/legitimate.bin'):
                file = app / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes(b'QA-only payload')
            installer = self.root / installer_dir
            installer.mkdir()
            (installer / f'jumpbot-setup-1.3-rc.3-{flavor}.exe').write_bytes(b'installer fixture')
        (self.root / 'TEST_SUMMARY_v1.3-rc.3.md').write_text('no-input packaging fixture', encoding='utf8')
        self.assertEqual(self.prepare('1.3-rc.3').returncode, 0)
        result = subprocess.run(['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass',
                                 '-File', str(ROOT / 'tools/package_release.ps1'),
                                 '-Root', str(self.root), '-Version', '1.3-rc.3'],
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for flavor in ('full', 'portable'):
            with zipfile.ZipFile(self.root / 'release/1.3-rc.3' / f'jumpbot-win64-v1.3-rc.3-{flavor}.zip') as archive:
                names = archive.namelist()
                self.assertIn('jumpbot/_internal/vendor/debug/legitimate.bin', names)
                self.assertFalse(any(n.startswith(('jumpbot/debug/', 'jumpbot/logs/',
                    'jumpbot/_internal/debug/', 'jumpbot/_internal/logs/')) for n in names))
        installer = (ROOT / 'tools/installer/jumpbot.iss').read_text(encoding='utf8')
        self.assertIn('\\_internal\\debug\\*', installer)
        self.assertIn('\\_internal\\logs\\*', installer)


if __name__ == '__main__':
    unittest.main()
