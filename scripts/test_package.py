"""Packaging regressions using isolated, synthetic committed repositories."""
import contextlib
import importlib.util
import io
import json
import os
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
    def test_historical_windows_scope_is_explicit(self):
        self.build('Windows')
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        with self.assertRaisesRegex(ValueError, 'All three'):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'rejected', self.root)
        verify_release.verify(self.output, commit, '0.2.0', self.root / 'verified', self.root, required_platforms={'Windows'})
        with self.assertRaisesRegex(ValueError, 'Unsupported historical'):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'rejected', self.root, required_platforms={'Linux'})

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
        (self.root / 'scripts/package-readme.md').write_text('Synthetic focused package README: {{SOURCE_ARCHIVE}}\n', encoding='utf-8')
        (self.root / '.gitignore').write_text('/output/\n/verified/\n/rejected/\n/registry/\n/synthetic-binary\n', encoding='utf-8')
        (self.root / 'integrations').mkdir()
        (self.root / 'integrations/segmentation_rembg.py').write_text('# Synthetic provider\n', encoding='utf-8')
        self.policy = {
            'version': 1,
            'source': sorted([p.relative_to(self.root).as_posix() for p in self.root.rglob('*') if p.is_file() and p.name not in ['README.md', 'FOLLOWUPS.MD', '.gitignore']] + [packaging.MANIFEST]),
            'portable': ['LICENSE', 'assets/fonts/LICENCE.txt', 'assets/peerbrush-logo.png', 'docs/usage.md', 'integrations/segmentation_rembg.py'],
            'readme': 'scripts/package-readme.md', 'vendored': [], 'rust_notices': ['COPYRIGHT.html', 'COPYRIGHT-library.html'],
        }
        self.save_policy()
        for args in [('init', '-q'), ('add', '.'), ('-c', 'user.name=Package Tests', '-c', 'user.email=package-tests@example.invalid', 'commit', '-qm', 'Synthetic fixture')]:
            subprocess.run(['git', '-C', str(self.root), *args], check=True, capture_output=True)
        self.output = self.root / 'output'

    def save_policy(self):
        (self.root / packaging.MANIFEST).write_text(json.dumps(self.policy), encoding='utf-8')

    def commit(self):
        subprocess.run(['git', '-C', str(self.root), 'add', '-A'], check=True, capture_output=True)
        subprocess.run(['git', '-C', str(self.root), '-c', 'user.name=Package Tests', '-c', 'user.email=package-tests@example.invalid', 'commit', '-qm', 'Synthetic update'], check=True, capture_output=True)

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


    def test_tracked_private_files_are_excluded_by_the_reviewed_policy(self):
        canary = b'SYNTHETIC_PRIVATE_CANARY_NOT_A_SECRET'
        for name in ['docs/connection.json', 'docs/outreach-plan.md', 'assets/.env', 'scripts/signing-key.pfx', 'AGENTS.md', 'website/.openai/hosting.json', 'docs/videos/marketing.mp4', 'assets/examples/website-demo.psd']:
            p = self.root / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(canary)
        self.commit()
        archive = self.build('Windows')
        with zipfile.ZipFile(archive) as bundle:
            prefix = archive.stem + '/'
            manifest = json.loads(bundle.read(prefix + 'release.json'))
            self.assertNotIn(prefix + 'FOLLOWUPS.MD', bundle.namelist())
            self.assertTrue(all(canary not in bundle.read(n) for n in bundle.namelist()))
            with zipfile.ZipFile(io.BytesIO(bundle.read(prefix + manifest['source']))) as sources:
                self.assertEqual(set(sources.namelist()), set(self.policy['source']) | {'README.md'})
                self.assertTrue(all(canary not in sources.read(n) for n in sources.namelist()))
                self.assertIn('integrations/segmentation_rembg.py', sources.namelist())
                self.assertIn(b'Synthetic focused', sources.read('README.md'))
                self.assertIn(manifest['source'].encode(), sources.read('README.md'))

    def test_unlisted_compiler_inputs_fail_and_preserve_existing_outputs(self):
        initial = self.build('Windows')
        original = initial.read_bytes()
        (self.root / 'src/new.rs').write_text('// synthetic new module')
        self.commit()
        with self.assertRaisesRegex(ValueError, 'Compiler input inventory'):
            self.build('Windows')
        self.assertEqual(initial.read_bytes(), original)
        self.assertFalse(list(self.output.glob('.peerbrush-package-*')))

    def test_committed_missing_inputs_and_invalid_manifest_paths_are_rejected(self):
        tree = {name: None for name in self.policy['source']}
        for name in ['../escape', '/absolute', 'docs/../escape', 'docs\\escape', 'docs/CON.txt', 'docs/name.', 'docs/name ', 'docs/name:alternate', 'README.md', 'licenses/private.txt', 'PeerBrush.app/private.txt']:
            with self.subTest(name=name):
                policy = dict(self.policy, source=self.policy['source'] + [name])
                with self.assertRaises(ValueError):
                    packaging.validate_policy(policy, tree | {name: None})
        duplicate = dict(self.policy, source=self.policy['source'] + ['LICENSE'])
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            packaging.validate_policy(duplicate, tree)
        (self.root / 'docs/usage.md').unlink()
        self.commit()
        with self.assertRaisesRegex(ValueError, 'Missing committed package input'):
            self.build('Windows')

    def test_source_snapshot_is_pinned_when_head_changes(self):
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        original = dict((n, d) for n, d, _ in packaging.source_files(self.root, commit))
        (self.root / 'src/lib.rs').write_text('// newer concurrent commit')
        self.commit()
        selected = dict((n, d) for n, d, _ in packaging.source_files(self.root, commit))
        self.assertEqual(selected, original)
        self.assertNotEqual(dict((n, d) for n, d, _ in packaging.source_files(self.root)), original)

    def test_binary_privacy_gate_is_not_skipped_by_synthetic_execution_override(self):
        canaries = [b'C:\\Users\\synthetic person\\build\\file.rs', b'/home/synthetic-person/build/file.rs', b'/Users/synthetic-person/build/file.rs', b'-----BEGIN PRIVATE KEY-----', b'ghp_' + b'x' * 36]
        for value in canaries:
            with self.subTest(value_type='synthetic marker'):
                for data in [value, value.decode().encode('utf-16le'), value.decode().encode('utf-16be')]:
                    with self.assertRaisesRegex(ValueError, 'credential marker or personal build path'):
                        packaging.validate_binary(data)
        self.build('Windows')
        binary = self.root / 'synthetic-binary'
        binary.write_bytes(binary.read_bytes() + canaries[0])
        with self.assertRaisesRegex(ValueError, 'credential marker or personal build path'):
            packaging.package(self.root, binary, self.output, self.root / 'registry', 'Windows', verify_binary=False)

    def test_dependency_notice_selection_rejects_traversal_and_ignores_private_files(self):
        lock = 'version=4\n[[package]]\nname="dep"\nversion="1.0.0"\nsource="registry+https://example.invalid"\n'
        (self.root / 'Cargo.lock').write_text(lock)
        self.commit()
        crate = self.root / 'registry/test/dep-1.0.0'
        for name, data in {'Cargo.toml': '[package]\nname="dep"\nversion="1.0.0"\nlicense="MIT"\nlicense-file="legal/custom.txt"\n', 'LICENSE-MIT': 'Synthetic license', 'LICENSE-private-notes.txt': 'PRIVATE_CANARY', 'legal/custom.txt': 'Synthetic declared license', 'fonts/NOTICE.txt': 'Synthetic font notice', 'fonts/connection.txt': 'PRIVATE_CANARY', 'fonts/LICENSE-private.txt': 'PRIVATE_CANARY', '.cargo-checksum.json': json.dumps({'files': {'fonts/NOTICE.txt': 'synthetic'}})}.items():
            p = crate / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(data)
        archive = self.build('Windows')
        with zipfile.ZipFile(archive) as bundle:
            prefix = archive.stem + '/'
            notices = json.loads(bundle.read(prefix + 'release.json'))['notice_files']
            self.assertEqual(set(notices), {'licenses/dep-1.0.0/LICENSE-MIT', 'licenses/dep-1.0.0/legal/custom.txt', 'licenses/dep-1.0.0/fonts/NOTICE.txt', 'licenses/ubuntu-sans/LICENCE.txt'})
            self.assertTrue(all(b'PRIVATE_CANARY' not in bundle.read(n) for n in bundle.namelist()))
        original = archive.read_bytes()
        source = self.output / 'PeerBrush-0.2.0-source.zip'
        source_original = source.read_bytes()
        (crate / 'Cargo.toml').write_text('[package]\nname="dep"\nversion="1.0.0"\nlicense-file="../outside.txt"\n')
        with self.assertRaises(ValueError):
            self.build('Windows')
        self.assertEqual(archive.read_bytes(), original)
        self.assertEqual(source.read_bytes(), source_original)
        self.assertFalse(list(self.output.glob('.peerbrush-package-*')))

    def test_windows_junction_notice_inputs_are_rejected(self):
        if os.name != 'nt':
            self.skipTest('Windows junction behavior')
        outside = self.root / 'outside-notices'
        outside.mkdir()
        (outside / 'LICENSE').write_text('Synthetic private fixture')
        junction = self.root / 'junction'
        subprocess.run(['cmd', '/c', 'mklink', '/J', str(junction), str(outside)], check=True, capture_output=True)
        try:
            with self.assertRaisesRegex(ValueError, 'links or reparse'):
                packaging.regular_file(self.root, 'junction/LICENSE')
        finally:
            junction.rmdir()

    def test_inspector_rejects_extra_entries_and_changed_notices(self):
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in ['Windows', 'Linux', 'macOS']:
            self.build(platform)
        archive_path = self.output / 'PeerBrush-0.2.0-Windows-x64.zip'
        original = archive_path.read_bytes()
        with zipfile.ZipFile(archive_path, 'a') as archive:
            archive.writestr(archive_path.stem + '/private-notes.txt', b'Synthetic private canary')
        with self.assertRaisesRegex(ValueError, 'Unexpected or missing release entries'):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'rejected', self.root)
        archive_path.write_bytes(original)
        with zipfile.ZipFile(archive_path) as archive:
            files = [(i.filename, archive.read(i.filename), i.external_attr >> 16 & 0o777) for i in archive.infolist()]
        tampered = [(name, data + b'changed' if '/licenses/' in name else data, mode) for name, data, mode in files]
        packaging.write_zip(archive_path, tampered, 1700000000)
        with self.assertRaisesRegex(ValueError, 'License notice checksum mismatch'):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'rejected', self.root)


    def test_repack_provenance_requires_identical_application_build_inputs(self):
        original_commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        (self.root / 'docs/usage.md').write_text('Updated public guide')
        self.commit()
        sources = packaging.source_files(self.root)
        packaging.equivalent_build_inputs(self.root, sources, original_commit)
        self.build('Windows')
        binary = self.root / 'synthetic-binary'
        with contextlib.redirect_stdout(io.StringIO()):
            archive_path = packaging.package(self.root, binary, self.output, self.root / 'registry', 'Windows', verify_binary=False, binary_source_commit=original_commit)
        with zipfile.ZipFile(archive_path) as archive:
            manifest = json.loads(archive.read(archive_path.stem + '/release.json'))
            self.assertEqual(manifest['binary_source_commit'], original_commit)
            self.assertEqual(manifest['commit'], packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip())
            self.assertNotEqual(manifest['commit'], manifest['binary_source_commit'])
        (self.root / 'src/lib.rs').write_text('// changed application source')
        self.commit()
        with self.assertRaisesRegex(ValueError, 'source inputs differ'):
            packaging.equivalent_build_inputs(self.root, packaging.source_files(self.root), original_commit)

    def test_embedded_font_notice_inventory_preserves_real_upstream_names(self):
        (self.root / 'Cargo.lock').write_text('version=4\n[[package]]\nname="epaint_default_fonts"\nversion="0.31.1"\nsource="registry+https://example.invalid"\n')
        self.commit()
        crate = self.root / 'registry/test/epaint_default_fonts-0.31.1'
        crate.mkdir(parents=True)
        (crate / 'Cargo.toml').write_text('[package]\nname="epaint_default_fonts"\nversion="0.31.1"\nlicense="MIT"\n')
        names = packaging.EMBEDDED_FONT_NOTICES['epaint_default_fonts']
        for name in names:
            path = crate / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('Synthetic font license')
        (crate / 'fonts/private.txt').write_text('SYNTHETIC_PRIVATE_CANARY')
        (crate / '.cargo-checksum.json').write_text(json.dumps({'files':{name:'synthetic' for name in names}}))
        archive_path = self.build('Windows')
        with zipfile.ZipFile(archive_path) as archive:
            manifest = json.loads(archive.read(archive_path.stem + '/release.json'))
            self.assertTrue({'licenses/epaint_default_fonts-0.31.1/' + name for name in names} <= set(manifest['notice_files']))
            self.assertFalse(any(name.endswith('/private.txt') for name in archive.namelist()))

    def test_dated_repack_names_preserve_internal_layout_and_require_matching_labels(self):
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in ['Windows', 'Linux', 'macOS']:
            path = self.build(platform)
            path.rename(path.with_name(path.stem + '-repacked-20261010.zip'))
        with contextlib.redirect_stdout(io.StringIO()):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'verified', self.root)
        inventory = json.loads((self.root / 'verified/release-inventory-repacked-20261010.json').read_text())
        self.assertEqual(inventory['packaging_correction'], '-repacked-20261010')
        self.assertTrue(all('-repacked-20261010.zip' in r['file'] for r in inventory['assets']))
        self.assertTrue((self.root / 'verified/SHA256SUMS-repacked-20261010').exists())
        mac = self.output / 'PeerBrush-0.2.0-macOS-arm64-repacked-20261010.zip'
        mac.rename(mac.with_name(mac.name.replace('20261010','20261011')))
        with self.assertRaisesRegex(ValueError, 'packaging correction label'):
            verify_release.verify(self.output, commit, '0.2.0', self.root / 'rejected', self.root)

    def test_archive_paths_reject_case_aliases_and_control_characters(self):
        with self.assertRaisesRegex(ValueError, 'duplicate archive paths'):
            packaging.write_zip(self.root / 'bad.zip', [('a.txt',b'one',0o644),('A.txt',b'two',0o644)], 1700000000)
        for name in ['docs/tab\t.txt', 'docs/newline\n.txt']:
            with self.assertRaises(ValueError):
                packaging.relative_path(name)


if __name__ == '__main__':
    unittest.main()
