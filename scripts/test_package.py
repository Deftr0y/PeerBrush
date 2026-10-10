"""Packaging regressions using isolated, synthetic committed repositories."""
import contextlib
import importlib.util
import io
import json
import pathlib
import plistlib
import struct
import subprocess
import tempfile
import unittest
import zipfile
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import verify_release
from build_release import release_environment

spec = importlib.util.spec_from_file_location('packaging', pathlib.Path(__file__).with_name('package.py'))
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)


class Packages(unittest.TestCase):
    def test_release_flags_remap_paths_and_preserve_encoded_options(self):
        environment = release_environment(pathlib.Path('synthetic checkout'), {'CARGO_ENCODED_RUSTFLAGS': '-C\x1ftarget-feature=+sse2'})
        flags = environment['CARGO_ENCODED_RUSTFLAGS'].split('\x1f')
        self.assertEqual(flags[:2], ['-C', 'target-feature=+sse2'])
        self.assertTrue(any(f.startswith('--remap-path-prefix=') and f.endswith('=peerbrush') for f in flags))
        self.assertTrue(any(f.endswith('=cargo') for f in flags))
        self.assertTrue(any(f.endswith('=build-home') for f in flags))
        with self.assertRaisesRegex(ValueError, 'CARGO_ENCODED_RUSTFLAGS'):
            release_environment(pathlib.Path('.'), {'RUSTFLAGS': '-C target-cpu=native'})

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-package-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        for name in ['vendor', 'docs', 'assets/fonts', 'src', 'scripts']:
            (self.root / name).mkdir(parents=True, exist_ok=True)
        for name, content in {'Cargo.toml': '[package]\nname="peerbrush"\nversion="0.2.0"\n', 'Cargo.lock': 'version=4\n[[package]]\nname="peerbrush"\nversion="0.2.0"\n', 'LICENSE': 'GNU GENERAL PUBLIC LICENSE\nGPL-3.0-only\n', 'README.md': 'Synthetic package fixture\n', 'FOLLOWUPS.MD': 'Synthetic backlog\n', 'assets/fonts/LICENCE.txt': 'Synthetic font license\n', 'assets/peerbrush-logo.png': 'Synthetic artwork\n', 'src/lib.rs': '// synthetic\n', 'scripts/start-windows.cmd': '@echo off\n', 'docs/usage.md': 'Synthetic instructions\n'}.items():
            (self.root / name).write_text(content, encoding='utf-8')
        for args in [('init', '-q'), ('add', '.'), ('-c', 'user.name=Package Tests', '-c', 'user.email=package-tests@example.invalid', 'commit', '-qm', 'Synthetic fixture')]:
            subprocess.run(['git', '-C', str(self.root), *args], check=True, capture_output=True)
        self.output = self.root / 'output'

    def build(self, platform):
        header = bytearray(128)
        if platform == 'Windows':
            header[:2] = b'MZ'
            header[60:64] = struct.pack('<I', 64)
            header[64:70] = b'PE\0\0\x64\x86'
        elif platform == 'Linux':
            header[:6] = b'\x7fELF\x02\x01'
            header[18:20] = struct.pack('<H', 62)
        else:
            header[:8] = b'\xcf\xfa\xed\xfe' + struct.pack('<I', 0x100000c)
        binary = self.root / 'synthetic-binary'
        binary.write_bytes(header)
        with contextlib.redirect_stdout(io.StringIO()):
            return packaging.package(self.root, binary, self.output, self.root / 'registry', platform, verify_binary=False)

    def test_platform_architecture_source_license_and_macos_bundle_match(self):
        sources = []
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in ['Windows', 'Linux', 'macOS']:
            archive = self.build(platform)
            with zipfile.ZipFile(archive) as package:
                self.assertIsNone(package.testzip())
                root = package.namelist()[0].split('/')[0] + '/'
                manifest = json.loads(package.read(root + 'release.json'))
                self.assertEqual(manifest['version'], '0.2.0')
                self.assertEqual(manifest['commit'], commit)
                self.assertEqual(manifest['architecture'], 'arm64' if platform == 'macOS' else 'x64')
                self.assertIn('GPL-3.0-only', package.read(root + 'LICENSE').decode())
                self.assertIn(root + 'licenses/ubuntu-sans/LICENCE.txt', package.namelist())
                self.assertEqual(package.getinfo(root + manifest['executable']).external_attr >> 16 & 0o777, 0o755)
                sources.append(package.read(root + manifest['source']))
                if platform == 'macOS':
                    info = plistlib.loads(package.read(root + 'PeerBrush.app/Contents/Info.plist'))
                    self.assertEqual(info['CFBundleShortVersionString'], '0.2.0')
                    self.assertEqual(info['CFBundleExecutable'], 'peerbrush')
        self.assertEqual(sources[0], sources[1])
        self.assertEqual(sources[0], sources[2])

    def test_untracked_private_files_and_stale_output_never_enter_packages(self):
        (self.root / 'docs' / 'connection.json').write_text('private fixture, must be excluded')
        (self.root / 'assets' / '.env').write_text('private fixture, must be excluded')
        self.output.mkdir()
        stale = self.output / 'PeerBrush-0.2.0-Windows-x64'
        stale.mkdir()
        (stale / 'recovery.psd').write_text('private fixture, must be excluded')
        first = self.build('Windows')
        before = first.read_bytes()
        second = self.build('Windows')
        self.assertEqual(second.read_bytes(), before)
        with zipfile.ZipFile(second) as package:
            self.assertFalse(any('connection.json' in n or '.env' in n or 'recovery.psd' in n for n in package.namelist()))
            root = package.namelist()[0].split('/')[0] + '/'
            manifest = json.loads(package.read(root + 'release.json'))
            with zipfile.ZipFile(io.BytesIO(package.read(root + manifest['source']))) as source:
                self.assertNotIn('docs/connection.json', source.namelist())
                self.assertNotIn('assets/.env', source.namelist())

    def test_modified_sources_and_wrong_architecture_refuse_packaging(self):
        (self.root / 'src/lib.rs').write_text('// changed\n')
        with self.assertRaisesRegex(ValueError, 'Commit tracked'):
            self.build('Windows')
        wrong = self.root / 'wrong-binary'
        wrong.write_bytes(bytes(64))
        with self.assertRaisesRegex(ValueError, 'architecture'):
            packaging.binary_architecture(wrong, 'macOS')

    def test_complete_matrix_is_required_and_tampered_executables_are_rejected(self):
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        self.build('Windows')
        with self.assertRaisesRegex(ValueError, 'All three'):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'verified', self.root)
        self.build('Linux')
        mac = self.build('macOS')
        with contextlib.redirect_stdout(io.StringIO()):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'verified', self.root)
        inventory = json.loads((self.root / 'verified/release-inventory.json').read_text())
        self.assertEqual(len(inventory['assets']), 4)
        self.assertEqual(inventory['commit'], commit)
        with zipfile.ZipFile(mac) as archive:
            files = [(i.filename, archive.read(i.filename), i.external_attr >> 16 & 0o777) for i in archive.infolist()]
        tampered = [(name, data + b'changed' if name.endswith('/MacOS/peerbrush') else data, mode) for name, data, mode in files]
        packaging.write_zip(mac, tampered, 1700000000)
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'rejected', self.root)


if __name__ == '__main__':
    unittest.main()
