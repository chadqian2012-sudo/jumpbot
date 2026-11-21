"""Exercise the Windows release scripts against a committed, disposable native build."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]
VERSION = '2.0-rc.3'
FLAVORS = {'full': 'dist-accelerated', 'portable': 'dist-portable'}
SOURCES = (
    'desktop/Cargo.toml', 'desktop/Cargo.lock', 'desktop/src/main.rs',
    'desktop/src/bin/jumpbot.rs', 'config.json', 'templates/piece.png',
    'templates/digit_0.png', 'run.bat', 'README.md', 'THIRD_PARTY_NOTICES.md',
    'LICENSE-OpenCV.txt', 'tools/build_app.ps1', 'tools/collect_licenses.ps1',
)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


@unittest.skipUnless(os.name == 'nt' and shutil.which('git'), 'Windows packaging')
class PackagingTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='jumpbot sealing test ')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.delivery = self.root / 'release' / VERSION
        self.apps = {f: self.root / dist / 'jumpbot' for f, dist in FLAVORS.items()}
        for name in (*SOURCES, 'tools/package_release.ps1'):
            dst = self.root / name
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, dst)
        shutil.copytree(ROOT / 'tools/installer', self.root / 'tools/installer')

        # Run the real installer in an empty directory without claiming either product's
        # AppId, registering an uninstaller, or creating a user's shortcuts.
        iss = self.root / 'tools/installer/jumpbot.iss'
        script = iss.read_text(encoding='utf-8-sig').replace(
            'AppId={#AppIdentity}', f'AppId=jumpbot-fixture-{uuid.uuid4()}\n'
            'Uninstallable=no\nCreateUninstallRegKey=no\nUsePreviousAppDir=no')
        # A file appears after validation, as ISCC preprocesses the real script.
        # Rescanning the payload for either output would now include this file.
        script += '\n#call SaveToFile(AddBackslash(PayloadDir) + "late-debug.txt")\n'
        iss.write_text(script, encoding='utf-8-sig')
        (self.root / '.gitignore').write_text('/dist-*/\n/release/\n/build/\n', encoding='utf8')
        self.summary = self.root / f'TEST_SUMMARY_v{VERSION}.md'
        self.summary.write_text('Offline packaging fixture\n', encoding='utf8')

        self.git('init', '-q')
        self.git('config', 'core.autocrlf', 'false')
        self.commit()
        self.head = self.git('rev-parse', 'HEAD').stdout.strip()
        sources = {name: sha(self.root / name) for name in SOURCES}
        system = Path(os.environ['SystemRoot']) / 'System32'
        self.files = {}
        for flavor, app in self.apps.items():
            app.mkdir(parents=True)
            for name in ('config.json', 'templates/piece.png', 'templates/digit_0.png',
                         'THIRD_PARTY_NOTICES.md', 'run.bat'):
                (app / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(self.root / name, app / name)
            # Valid Windows PE data. The packaging tests never run these binaries.
            shutil.copyfile(system / 'where.exe', app / 'jumpbot.exe')
            shutil.copyfile(system / 'whoami.exe', app / 'jumpbot-desktop.exe')
            vendor = app / 'licenses/vendor/debug/library notice.txt'
            vendor.parent.mkdir(parents=True)
            vendor.write_text(flavor, encoding='utf8')
            self.files[flavor] = {p.relative_to(app).as_posix(): sha(p)
                                  for p in app.rglob('*') if p.is_file()}
            manifest = dict(commit=self.head, flavor=flavor, source_sha256=sources,
                            files_sha256=self.files[flavor])
            (app / 'build-source.json').write_text(json.dumps(manifest), encoding='utf8')

    def git(self, *args):
        command = ['git', '-C', str(self.root), *args]
        result = subprocess.run(command, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def commit(self):
        self.git('add', '.')
        self.git('-c', 'user.name=Packaging Test', '-c', 'user.email=test@example.invalid',
                 '-c', 'commit.gpgsign=false', 'commit', '-qm', 'native fixture')

    def powershell(self, *args):
        return subprocess.run(['powershell', '-NoProfile', '-NonInteractive',
                               '-ExecutionPolicy', 'Bypass', *map(str, args)],
                              cwd=self.root, capture_output=True, text=True,
                              errors='replace', timeout=90)

    def release(self, *args, version=VERSION):
        return self.powershell('-File', self.root / 'tools/package_release.ps1',
                               '-Root', self.root, '-Version', version, *args)

    def installer(self, flavor, *args, version=VERSION):
        return self.powershell('-File', self.root / 'tools/installer/build_installer.ps1',
                               '-Root', self.root, '-Version', version, '-Flavor', flavor,
                               '-PayloadDir', self.apps[flavor], '-OutputDir',
                               self.root / 'build/standalone', '-SkipAppBuild', *args)

    def rejected(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stderr)
        self.assertFalse(self.delivery.exists(), 'A rejected release exposed output files')
        self.assertFalse(list((self.root / 'build/standalone').glob('*.exe')))

    def test_prerelease_manifests_precede_installers(self):
        result = self.release('-PrepareOnly')
        self.assertEqual(result.returncode, 0, result.stderr)
        for flavor, app in self.apps.items():
            info = json.loads((app / 'source-version.json').read_bytes())
            self.assertEqual((info['version'], info['commit'], info['flavor'], info['candidate']),
                             (VERSION, self.head, flavor, False))
            self.assertEqual(info['build_manifest_sha256'].lower(), sha(app / 'build-source.json'))
        self.assertFalse(self.delivery.exists())
        self.assertFalse(list(self.root.rglob('jumpbot-setup-*.exe')))

    def test_dirty_native_source_cannot_be_sealed(self):
        src = self.root / 'desktop/src/bin/jumpbot.rs'
        src.write_text('// edited after build\n', encoding='utf8')
        self.rejected(self.release(), 'Commit the source')
        self.rejected(self.release('-Candidate', '-OutputDir', self.root / 'build/candidate'),
                      'Source changed since build')
        self.assertFalse((self.root / 'build/candidate').exists())

    def test_stale_binaries_and_old_revision_cannot_be_sealed(self):
        app = self.apps['portable']
        for name in ('jumpbot.exe', 'jumpbot-desktop.exe'):
            with self.subTest(name=name):
                binary = app / name
                original = binary.read_bytes()
                binary.write_bytes(original + b'new bytes')
                self.rejected(self.release(), 'Payload changed since build')
                self.rejected(self.installer('portable'), 'Payload changed since build')
                binary.write_bytes(original)
        (self.root / 'desktop/src/main.rs').write_text('// next revision\n', encoding='utf8')
        self.commit()
        self.rejected(self.release(), 'Build revision mismatch')

    def test_version_cannot_escape_the_delivery_directory(self):
        for result in (self.release('-PrepareOnly', version='../outside'),
                       self.installer('full', version='../outside')):
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn('ParameterArgumentValidationError', result.stderr)
        self.assertFalse((self.root / 'outside').exists())
        self.assertFalse(list(self.root.rglob('source-version.json')))
        self.assertFalse(list(self.root.rglob('jumpbot-setup-*.exe')))

    def test_installer_prerelease_has_numeric_version_resource(self):
        result = self.installer('full')
        self.assertEqual(result.returncode, 0, result.stderr)
        setup = self.root / 'build/standalone' / f'jumpbot-setup-{VERSION}-full.exe'
        self.assertTrue((self.apps['full'] / 'late-debug.txt').is_file())
        version = self.powershell('-Command', "(Get-Item -LiteralPath "
                                  f"'build/standalone/{setup.name}').VersionInfo.FileVersionRaw.ToString()")
        self.assertEqual(version.returncode, 0, version.stderr)
        self.assertEqual(version.stdout.strip(), '2.0.0.0')

    def test_installers_and_zips_only_deliver_manifest_files(self):
        app = self.apps['portable']
        secret = app / 'templates/private.png'
        secret.write_bytes(b'not a user template')
        for hidden in (False, True):
            with self.subTest(hidden=hidden):
                if hidden:
                    self.assertTrue(ctypes.windll.kernel32.SetFileAttributesW(str(secret), 0x2))
                self.rejected(self.installer('portable'), 'Payload file list changed')
                self.rejected(self.release(), 'Payload file list changed')
        secret.unlink()
        for app in self.apps.values():
            for name in ('logs/run.csv', 'debug/frame.png', '_internal/logs/old.log',
                         '_internal/debug/old.png'):
                path = app / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'exclude from every distribution')

        result = self.release()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        sums = {}
        for line in (self.delivery / 'SHA256SUMS.txt').read_text(encoding='utf8').splitlines():
            digest, name = line.split('  ', 1)
            self.assertEqual(digest, sha(self.delivery / name))
            sums[name] = digest
        self.assertEqual(set(sums), {p.name for p in self.delivery.iterdir()} - {'SHA256SUMS.txt'})
        self.assertEqual((self.delivery / 'TEST_SUMMARY.md').read_bytes(), self.summary.read_bytes())
        for flavor, app in self.apps.items():
            with self.subTest(flavor=flavor):
                expected = dict(self.files[flavor])
                for name in ('build-source.json', 'source-version.json'):
                    expected[name] = sha(app / name)
                archive = self.delivery / f'jumpbot-win64-v{VERSION}-{flavor}.zip'
                with zipfile.ZipFile(archive) as opened:
                    self.assertCountEqual(opened.namelist(), (f'jumpbot/{p}' for p in expected))
                    for name, digest in expected.items():
                        self.assertEqual(hashlib.sha256(opened.read(f'jumpbot/{name}')).hexdigest(), digest)
                setup = self.delivery / f'jumpbot-setup-{VERSION}-{flavor}.exe'
                installed = self.root / 'build' / f'installed-{flavor}'
                run = subprocess.run([str(setup), '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART',
                                      '/CURRENTUSER', '/NOICONS', '/MERGETASKS=!desktopicon',
                                      f'/DIR={installed}'], timeout=90)
                self.assertEqual(run.returncode, 0)
                contents = {p.relative_to(installed).as_posix(): sha(p)
                            for p in installed.rglob('*') if p.is_file()}
                self.assertEqual(contents, expected)


if __name__ == '__main__':
    unittest.main()
