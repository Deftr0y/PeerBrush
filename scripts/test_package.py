"""Packaging regressions using isolated, synthetic committed repositories."""
import contextlib
import gzip
import importlib.util
import io
import json
import os
import pathlib
import plistlib
import struct
import subprocess
import tempfile
import tarfile
import unittest
from unittest import mock
import zipfile
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import verify_release
from build_release import release_environment, source_input_state

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
        (self.root / 'scripts/package-readme.md').write_text('Synthetic focused package README: {{SOURCE_ARCHIVE}}\n', encoding='utf-8')
        (self.root / 'assets/peerbrush.icns').write_bytes((pathlib.Path(__file__).resolve().parents[1] / 'assets/peerbrush.icns').read_bytes())
        (self.root / '.gitignore').write_text('/output/\n/verified/\n/rejected/\n/registry/\n/cache/\n/synthetic-binary\n', encoding='utf-8')
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
        self.compiler = tempfile.TemporaryDirectory(prefix='peerbrush synthetic compiler ')
        self.addCleanup(self.compiler.cleanup)
        self.rust_docs = pathlib.Path(self.compiler.name) / 'share' / 'doc' / 'rust'
        self.rust_docs.mkdir(parents=True)
        for name in self.policy['rust_notices']:
            (self.rust_docs / name).write_text('Synthetic active compiler notice: ' + name, encoding='utf-8')

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
        if self.policy['version'] == 2:
            header.extend(json.dumps(self.build_info(), separators=(',', ':')).encode())
        binary = self.root / 'synthetic-binary'
        binary.write_bytes(header)
        with contextlib.redirect_stdout(io.StringIO()):
            return packaging.package(self.root, binary, self.output, self.root / 'registry', platform, verify_binary=False, rust_docs=self.rust_docs)

    def build_info(self, commit=None):
        return {'format': 'PeerBrushBuildInfoV1', 'schema': 1, 'product': 'PeerBrush',
                'version': '0.2.0', 'commit': commit or packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip(),
                'tree': 'clean', 'end': 'PeerBrushBuildInfoEnd'}

    def require_provenance(self):
        for name in ['build.rs', 'src/build_info.rs']:
            (self.root / name).write_text('// synthetic build metadata source\n')
            self.policy['source'].append(name)
        self.policy['version'] = 2
        self.save_policy()
        self.commit()

    def test_embedded_provenance_rejects_missing_duplicate_dirty_unknown_and_malformed_labels(self):
        info = self.build_info()
        encoded = json.dumps(info, separators=(',', ':')).encode()
        self.assertEqual(packaging.binary_build_info(b'native-header' + encoded, '0.2.0', info['commit']), info)
        invalid = [b'no build label', encoded + encoded, encoded.replace(b'"tree":"clean"', b'"tree":"dirty"'),
                   encoded.replace(info['commit'].encode(), b'unknown'), encoded.replace(b'"schema":1', b'"schema":true'),
                   encoded.replace(b'"version":"0.2.0"', b'"version":"0.1.0"'), encoded[:-1],
                   encoded.replace(b'"tree":"clean"', b'"tree":"' + b'x' * 513 + b'"')]
        for record in invalid:
            with self.subTest(record=record[:64]), self.assertRaisesRegex(ValueError, 'build provenance'):
                packaging.binary_build_info(record, '0.2.0', info['commit'])

    def test_new_policy_cannot_be_downgraded_or_skip_embedded_binary_checks(self):
        self.require_provenance()
        self.build('Windows')
        binary = self.root / 'synthetic-binary'
        binary.write_bytes(binary.read_bytes()[:128])
        with self.assertRaisesRegex(ValueError, 'embedded build provenance'):
            packaging.package(self.root, binary, self.output, self.root / 'registry', 'Windows', verify_binary=False, rust_docs=self.rust_docs)
        self.policy['version'] = 1
        self.save_policy()
        self.commit()
        with self.assertRaisesRegex(ValueError, 'requires package policy version 2'):
            packaging.source_files(self.root)

    def test_new_policy_embedded_records_are_inspected_without_execution_on_each_platform(self):
        self.require_provenance()
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in ['Windows', 'Linux', 'macOS']:
            archive = self.build(platform)
            checked = verify_release.inspect_archive(archive, commit, '0.2.0', self.root)
            self.assertEqual(checked['manifest']['build_info'], self.build_info())

    def test_new_policy_detects_relabelled_same_version_binary_and_allows_explicit_matching_correction(self):
        self.require_provenance()
        original = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        self.build('Windows')
        binary = self.root / 'synthetic-binary'
        (self.root / 'docs/usage.md').write_text('Updated guide, unchanged application')
        self.commit()
        with self.assertRaisesRegex(ValueError, 'clean expected version/commit'):
            packaging.package(self.root, binary, self.output, self.root / 'registry', 'Windows', verify_binary=False, rust_docs=self.rust_docs)
        with contextlib.redirect_stdout(io.StringIO()):
            archive = packaging.package(self.root, binary, self.output, self.root / 'registry', 'Windows', verify_binary=False, rust_docs=self.rust_docs, binary_source_commit=original)
        current = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        checked = verify_release.inspect_archive(archive, current, '0.2.0', self.root)
        self.assertEqual(checked['manifest']['build_info']['commit'], original)
        self.assertEqual(checked['record']['binary_source_commit'], original)

    def test_inspector_rejects_forged_provenance_even_with_updated_executable_checksum(self):
        self.require_provenance()
        archive = self.build('Windows')
        with zipfile.ZipFile(archive) as package:
            files = [(i.filename, package.read(i.filename), i.external_attr >> 16 & 0o777) for i in package.infolist()]
        binary_name = archive.stem + '/peerbrush.exe'
        manifest_name = archive.stem + '/release.json'
        forged = next(data for name, data, _ in files if name == binary_name).replace(b'"tree":"clean"', b'"tree":"dirty"')
        manifest = json.loads(next(data for name, data, _ in files if name == manifest_name))
        manifest['executable_sha256'] = verify_release.sha(forged)
        packaging.write_zip(archive, [(name, forged if name == binary_name else json.dumps(manifest).encode() if name == manifest_name else data, mode) for name, data, mode in files], 1700000000)
        with self.assertRaisesRegex(ValueError, 'build provenance'):
            verify_release.inspect_archive(archive, manifest['commit'], '0.2.0', self.root)
        manifest['executable_sha256'] = verify_release.sha(next(data for name, data, _ in files if name == binary_name))
        manifest['build_info']['commit'] = '0' * 40
        packaging.write_zip(archive, [(name, json.dumps(manifest).encode() if name == manifest_name else data, mode) for name, data, mode in files], 1700000000)
        with self.assertRaisesRegex(ValueError, 'manifest differs from actual embedded'):
            verify_release.inspect_archive(archive, manifest['commit'], '0.2.0', self.root)

    def test_executed_build_info_must_match_the_actual_embedded_record(self):
        self.require_provenance()
        self.build('Windows')
        real_check_output = packaging.subprocess.check_output
        def executable_output(command, *args, **kwargs):
            if command[-1] == '--version':
                return 'PeerBrush 0.2.0\n'
            if command[-1] == '--build-info':
                return json.dumps(self.build_info('0' * 40), separators=(',', ':'))
            return real_check_output(command, *args, **kwargs)
        with mock.patch.object(packaging.subprocess, 'check_output', side_effect=executable_output), self.assertRaisesRegex(ValueError, '--build-info differs'):
            packaging.package(self.root, self.root / 'synthetic-binary', self.output, self.root / 'registry', 'Windows', rust_docs=self.rust_docs)

    def test_release_builder_invalidation_ignores_caller_fingerprint_and_detects_new_root_inputs(self):
        original = source_input_state(self.root)
        environment = release_environment(self.root, {'PEERBRUSH_BUILD_INPUT_STATE': 'caller-supplied-source-claim'})
        self.assertEqual(environment['PEERBRUSH_BUILD_INPUT_STATE'], original)
        self.assertRegex(original, r'^[0-9a-f]{64}$')
        (self.root / 'new-root-input').write_text('new visible input')
        self.assertNotEqual(source_input_state(self.root), original)
        nested = self.root / 'extracted-source'
        nested.mkdir()
        self.assertEqual(source_input_state(nested), 'unknown')

    def test_git_provenance_calls_ignore_inherited_repository_and_config_overrides(self):
        actual = packaging.git(self.root, 'rev-parse', 'HEAD')
        with mock.patch.dict(os.environ, {'GIT_DIR': str(self.root / 'nonexistent'), 'GIT_CONFIG_COUNT': '1', 'GIT_CONFIG_KEY_0': 'core.bare', 'GIT_CONFIG_VALUE_0': 'true'}):
            self.assertEqual(packaging.git(self.root, 'rev-parse', 'HEAD'), actual)
            self.assertRegex(source_input_state(self.root), r'^[0-9a-f]{64}$')

    def test_policy_two_rejects_index_flags_hiding_changed_worktree_bytes(self):
        self.require_provenance()
        self.build('Windows')
        path = self.root / 'src/lib.rs'
        original = path.read_bytes()
        for flag, clear in [('--assume-unchanged', '--no-assume-unchanged'), ('--skip-worktree', '--no-skip-worktree')]:
            packaging.git(self.root, 'update-index', flag, 'src/lib.rs')
            path.write_text('hidden different application bytes')
            self.assertFalse(packaging.git(self.root, 'status', '--porcelain', '--untracked-files=no'))
            with self.assertRaisesRegex(ValueError, 'Hidden index inputs'):
                packaging.package(self.root, self.root / 'synthetic-binary', self.output, self.root / 'registry', 'Windows', verify_binary=False, rust_docs=self.rust_docs)
            packaging.git(self.root, 'update-index', clear, 'src/lib.rs')
            path.write_bytes(original)

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
                for name in self.policy['rust_notices']:
                    notice = 'licenses/rust-runtime/' + name
                    self.assertEqual(package.read(root + notice), (self.rust_docs / name).read_bytes())
                    self.assertEqual(manifest['notice_files'][notice], packaging.digest(self.rust_docs / name))
                self.assertEqual(package.getinfo(root + manifest['executable']).external_attr >> 16 & 0o777, 0o755)
                sources.append(package.read(root + manifest['source']))
                if platform == 'macOS':
                    info = plistlib.loads(package.read(root + 'PeerBrush.app/Contents/Info.plist'))
                    self.assertEqual(info['CFBundleShortVersionString'], '0.2.0')
                    self.assertEqual(info['CFBundleExecutable'], 'peerbrush')
                    self.assertEqual(info['CFBundleIconFile'], 'PeerBrush.icns')
                    self.assertEqual({kind['LSHandlerRank'] for kind in info['CFBundleDocumentTypes']}, {'Alternate'})
                    self.assertEqual({kind['CFBundleTypeRole'] for kind in info['CFBundleDocumentTypes']}, {'Editor'})
                    resource = root + packaging.MACOS_RESOURCES
                    self.assertEqual(package.read(resource + 'PeerBrush.icns'), (self.root / 'assets/peerbrush.icns').read_bytes())
                    for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md', *manifest['notice_files']]:
                        self.assertEqual(package.read(resource + name), package.read(root + name))
        self.assertEqual(sources[0], sources[1])
        self.assertEqual(sources[0], sources[2])

    def test_macos_installed_resources_and_consent_cannot_be_changed_or_removed(self):
        path = self.build('macOS')
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        prefix = path.stem + '/'
        with zipfile.ZipFile(path) as archive:
            original = [(item.filename, archive.read(item.filename), item.external_attr >> 16 & 0o777)
                        for item in archive.infolist()]
        for target, expected in [('PeerBrush.icns', 'committed artwork'),
                                 ('LICENSE', 'installed license/notices'),
                                 ('THIRD_PARTY_NOTICES.md', 'installed license/notices'),
                                 ('licenses/ubuntu-sans/LICENCE.txt', 'installed license/notices')]:
            name = prefix + packaging.MACOS_RESOURCES + target
            changed = [(n, data + b'tampered' if n == name else data, mode) for n, data, mode in original]
            packaging.write_zip(path, changed, 1700000000)
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, expected):
                verify_release.inspect_archive(path, commit, '0.2.0', self.root)
        changed = []
        for name, data, mode in original:
            if name == prefix + 'PeerBrush.app/Contents/Info.plist':
                info = plistlib.loads(data)
                info['CFBundleDocumentTypes'][0]['LSHandlerRank'] = 'Default'
                data = plistlib.dumps(info)
            changed.append((name, data, mode))
        packaging.write_zip(path, changed, 1700000000)
        with self.assertRaisesRegex(ValueError, 'document consent'):
            verify_release.inspect_archive(path, commit, '0.2.0', self.root)
        changed = []
        for name, data, mode in original:
            if name == prefix + 'release.json':
                manifest = json.loads(data)
                del manifest['macos_bundle_resources']
                data = json.dumps(manifest).encode()
            if not name.startswith(prefix + packaging.MACOS_RESOURCES):
                changed.append((name, data, mode))
        packaging.write_zip(path, changed, 1700000000)
        with self.assertRaisesRegex(ValueError, 'Missing required installed macOS resources'):
            verify_release.inspect_archive(path, commit, '0.2.0', self.root)

    def test_macos_internal_development_mapping_retains_exact_cargo_version(self):
        for name in ['Cargo.toml', 'Cargo.lock']:
            path = self.root / name
            path.write_text(path.read_text().replace('0.2.0', '0.30.0-dev.1'))
        self.commit()
        path = self.build('macOS')
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        prefix = path.stem + '/'
        with zipfile.ZipFile(path) as archive:
            original = [(item.filename, archive.read(item.filename), item.external_attr >> 16 & 0o777)
                        for item in archive.infolist()]
            info = plistlib.loads(archive.read(prefix + 'PeerBrush.app/Contents/Info.plist'))
            self.assertEqual(info['CFBundleShortVersionString'], '0.30.0')
            self.assertEqual(info['CFBundleVersion'], '0.30.0')
            self.assertEqual(info['PeerBrushDevelopmentVersion'], '0.30.0-dev.1')
            self.assertEqual(json.loads(archive.read(prefix + 'release.json'))['version'], '0.30.0-dev.1')
        verify_release.inspect_archive(path, commit, '0.30.0-dev.1', self.root)
        for key, changed in [('PeerBrushDevelopmentVersion', None), ('PeerBrushDevelopmentVersion', '0.30.0-dev.2'),
                             ('CFBundleVersion', '0.30.0-dev.1'), ('CFBundleShortVersionString', '0.30.1')]:
            altered = dict(info)
            if changed is None:
                del altered[key]
            else:
                altered[key] = changed
            files = [(name, plistlib.dumps(altered) if name.endswith('/Info.plist') else data, mode)
                     for name, data, mode in original]
            packaging.write_zip(path, files, 1700000000)
            with self.subTest(key=key, changed=changed), self.assertRaises(ValueError):
                verify_release.inspect_archive(path, commit, '0.30.0-dev.1', self.root)
        self.assertNotIn('PeerBrushDevelopmentVersion', packaging.macos_info('0.30.0'))
        for version in ['0.30.0-beta.1', '0.30.0-dev.01', '0.30.0+build', '00.30.0-dev.1']:
            with self.subTest(version=version), self.assertRaises(ValueError):
                packaging.macos_info(version)

    def test_historical_macos_layout_remains_valid_for_its_matching_source(self):
        path = self.build('macOS')
        prefix = path.stem + '/'
        with zipfile.ZipFile(path) as archive:
            original = [(item.filename, archive.read(item.filename), item.external_attr >> 16 & 0o777)
                        for item in archive.infolist()]
        # An earlier source revision had neither ICNS nor installed Resources.
        (self.root / 'assets/peerbrush.icns').unlink()
        self.policy['source'].remove('assets/peerbrush.icns')
        self.save_policy()
        self.commit()
        self.build('Windows')
        source = (self.output / 'PeerBrush-0.2.0-source.zip').read_bytes()
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        changed = []
        for name, data, mode in original:
            if name.startswith(prefix + packaging.MACOS_RESOURCES):
                continue
            if name == prefix + 'release.json':
                manifest = json.loads(data)
                del manifest['macos_bundle_resources']
                manifest.update(commit=commit, binary_source_commit=commit, source_sha256=verify_release.sha(source))
                data = json.dumps(manifest).encode()
            elif name == prefix + 'PeerBrush-0.2.0-source.zip':
                data = source
            elif name == prefix + 'PeerBrush.app/Contents/Info.plist':
                info = plistlib.loads(data)
                for key in ['CFBundleIconFile', 'CFBundleDocumentTypes', 'UTImportedTypeDeclarations']:
                    del info[key]
                data = plistlib.dumps(info)
            changed.append((name, data, mode))
        packaging.write_zip(path, changed, 1700000000)
        verify_release.inspect_archive(path, commit, '0.2.0', self.root)

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

    def test_source_preflight_reports_omissions_without_building(self):
        script = self.root / 'scripts/package.py'
        script.write_bytes(pathlib.Path(packaging.__file__).read_bytes())
        command = [sys.executable, str(script), '--check-source']
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Package source policy valid', result.stdout)
        (self.root / 'tests').mkdir()
        (self.root / 'tests/psb.rs').write_text('// synthetic codec regression')
        self.commit()
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('tests/psb.rs', result.stderr)
        self.policy['source'].append('tests/psb.rs')
        self.save_policy()
        self.commit()
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / 'dist').exists())
        self.assertFalse((self.root / 'target').exists())

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
            self.assertEqual(set(notices), {'licenses/dep-1.0.0/LICENSE-MIT', 'licenses/dep-1.0.0/legal/custom.txt', 'licenses/dep-1.0.0/fonts/NOTICE.txt', 'licenses/ubuntu-sans/LICENCE.txt'} | {'licenses/rust-runtime/' + name for name in self.policy['rust_notices']})
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
            archive_path = packaging.package(self.root, binary, self.output, self.root / 'registry', 'Windows', verify_binary=False, rust_docs=self.rust_docs, binary_source_commit=original_commit)
        with zipfile.ZipFile(archive_path) as archive:
            manifest = json.loads(archive.read(archive_path.stem + '/release.json'))
            self.assertEqual(manifest['binary_source_commit'], original_commit)
            self.assertEqual(manifest['commit'], packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip())
            self.assertNotEqual(manifest['commit'], manifest['binary_source_commit'])
        (self.root / 'src/lib.rs').write_text('// changed application source')
        self.commit()
        with self.assertRaisesRegex(ValueError, 'source inputs differ'):
            packaging.equivalent_build_inputs(self.root, packaging.source_files(self.root), original_commit)

    def test_active_compiler_notice_path_uses_absolute_sysroot_with_spaces(self):
        self.build('Windows')
        stale = self.root / '.dev-tools' / 'rust' / 'share' / 'doc' / 'rust'
        stale.mkdir(parents=True)
        for name in self.policy['rust_notices']:
            (stale / name).write_text('SYNTHETIC_STALE_COMPILER_NOTICE', encoding='utf-8')
        real_check_output = packaging.subprocess.check_output
        def compiler_output(command, *args, **kwargs):
            if command == ['rustc', '--print', 'sysroot']:
                return self.compiler.name + '\n'
            return real_check_output(command, *args, **kwargs)
        with mock.patch.object(packaging.subprocess, 'check_output', side_effect=compiler_output) as compiler:
            with contextlib.redirect_stdout(io.StringIO()):
                archive = packaging.package(self.root, self.root / 'synthetic-binary', self.output, self.root / 'registry', 'Windows', verify_binary=False)
            compiler.assert_any_call(['rustc', '--print', 'sysroot'], text=True)
            self.assertEqual(sum(call.args[0] == ['rustc', '--print', 'sysroot'] for call in compiler.call_args_list), 1)
        with zipfile.ZipFile(archive) as bundle:
            for name in self.policy['rust_notices']:
                self.assertEqual(bundle.read(archive.stem + '/licenses/rust-runtime/' + name), (self.rust_docs / name).read_bytes())
        for result in ['', '\n', 'relative/toolchain']:
            with self.subTest(result=result), mock.patch.object(packaging.subprocess, 'check_output', return_value=result):
                with self.assertRaisesRegex(ValueError, 'absolute sysroot'):
                    packaging.active_rust_docs()
        with mock.patch.object(packaging.subprocess, 'check_output', side_effect=subprocess.CalledProcessError(1, ['rustc'])):
            with self.assertRaises(subprocess.CalledProcessError):
                packaging.active_rust_docs()

    def test_missing_or_empty_runtime_notice_preserves_previous_outputs(self):
        self.build('Windows')
        original = {path.name: path.read_bytes() for path in self.output.iterdir() if path.is_file()}
        for name in self.policy['rust_notices']:
            notice = self.rust_docs / name
            data = notice.read_bytes()
            for missing in [True, False]:
                with self.subTest(name=name, missing=missing):
                    if missing:
                        notice.unlink()
                    else:
                        notice.write_bytes(b'')
                    try:
                        with self.assertRaisesRegex(ValueError, 'Rust runtime notice'):
                            self.build('Windows')
                        self.assertEqual(original, {path.name: path.read_bytes() for path in self.output.iterdir() if path.is_file()})
                        self.assertFalse(list(self.output.glob('.peerbrush-package-*')))
                    finally:
                        notice.write_bytes(data)

    def test_inspector_requires_runtime_notices_even_when_inventory_is_removed(self):
        archive_path = self.build('Windows')
        commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        prefix = archive_path.stem + '/'
        with zipfile.ZipFile(archive_path) as archive:
            original = [(i.filename, archive.read(i.filename), i.external_attr >> 16 & 0o777) for i in archive.infolist()]
        for name in self.policy['rust_notices']:
            notice = 'licenses/rust-runtime/' + name
            manifest = json.loads(next(data for filename, data, _ in original if filename == prefix + 'release.json'))
            del manifest['notice_files'][notice]
            files = [(filename, json.dumps(manifest).encode() if filename == prefix + 'release.json' else data, mode)
                     for filename, data, mode in original if filename != prefix + notice]
            packaging.write_zip(archive_path, files, 1700000000)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'Missing required Rust runtime notices'):
                verify_release.inspect_archive(archive_path, commit, '0.2.0', self.root)
            manifest['notice_files'][notice] = verify_release.sha(b'')
            files = [(filename, json.dumps(manifest).encode() if filename == prefix + 'release.json' else b'' if filename == prefix + notice else data, mode)
                     for filename, data, mode in original]
            packaging.write_zip(archive_path, files, 1700000000)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'Required Rust runtime notice is empty'):
                verify_release.inspect_archive(archive_path, commit, '0.2.0', self.root)

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

    def test_tls_notices_include_ring_subcomponents_and_require_exact_checksums(self):
        (self.root / 'Cargo.lock').write_text('version=4\n[[package]]\nname="ring"\nversion="0.17.14"\nsource="registry+https://example.invalid"\n[[package]]\nname="rustls"\nversion="0.23.46"\nsource="registry+https://example.invalid"\n')
        self.commit()
        ring = self.root / 'registry/test/ring-0.17.14'
        ring.mkdir(parents=True)
        (ring / 'Cargo.toml').write_text('[package]\nname="ring"\nversion="0.17.14"\nlicense="Apache-2.0 AND ISC"\n')
        (ring / 'LICENSE').write_text('Synthetic license index')
        names = packaging.EXTRA_CRATE_NOTICES['ring']
        for name in names:
            path = ring / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('Synthetic component notice: ' + name)
        (ring / 'LICENSE-private').write_text('SYNTHETIC_PRIVATE_CANARY')
        checksums = {name: packaging.digest(ring / name) for name in names}
        (ring / '.cargo-checksum.json').write_text(json.dumps({'files': checksums}))
        rustls = self.root / 'registry/test/rustls-0.23.46'
        rustls.mkdir(parents=True)
        (rustls / 'Cargo.toml').write_text('[package]\nname="rustls"\nversion="0.23.46"\nlicense="Apache-2.0 OR ISC OR MIT"\n')
        (rustls / 'LICENSE-ISC').write_text('Synthetic ISC alternative')
        archive_path = self.build('Windows')
        with zipfile.ZipFile(archive_path) as archive:
            manifest = json.loads(archive.read(archive_path.stem + '/release.json'))
            self.assertTrue({'licenses/ring-0.17.14/' + name for name in names} <= set(manifest['notice_files']))
            self.assertIn('licenses/rustls-0.23.46/LICENSE-ISC', manifest['notice_files'])
            self.assertFalse(any('LICENSE-private' in name for name in archive.namelist()))
        notice = ring / 'LICENSE-other-bits'
        original = notice.read_bytes()
        notice.write_text('Tampered component notice')
        with self.assertRaisesRegex(ValueError, 'notice checksum mismatch'):
            self.build('Windows')
        notice.write_bytes(original)
        del checksums['LICENSE-other-bits']
        (ring / '.cargo-checksum.json').write_text(json.dumps({'files': checksums}))
        with self.assertRaisesRegex(ValueError, 'required dependency notice inventory'):
            self.build('Windows')

    def cached_notice_fixture(self, name='ring'):
        version = '0.17.14' if name == 'ring' else '0.31.1'
        key = name + '-' + version
        self.cached_package = dict(name=name, version=version, source='registry+https://example.invalid')
        self.cached_crate = self.root / 'registry/test' / key
        self.cached_metadata = f'[package]\nname="{name}"\nversion="{version}"\nlicense="MIT"\nlicense-file="legal/custom.txt"\n'.encode()
        self.cached_files = {'Cargo.toml': self.cached_metadata, 'LICENSE': b'Synthetic root license',
                             'legal/custom.txt': b'Synthetic declared license', 'fonts/NOTICE.txt': b'Synthetic font notice',
                             'LICENSE-private': b'SYNTHETIC_PRIVATE_CANARY', 'fonts/connection.txt': b'SYNTHETIC_PRIVATE_CANARY'}
        for path in packaging.EXTRA_CRATE_NOTICES.get(name, set()) | packaging.EMBEDDED_FONT_NOTICES.get(name, set()):
            self.cached_files[path] = b'Synthetic required notice: ' + path.encode()
        for path, data in self.cached_files.items():
            target = self.cached_crate / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        self.cached_archive = self.root / 'cache/test' / (key + '.crate')
        self.cached_archive.parent.mkdir(parents=True, exist_ok=True)
        self.write_cached_notice_archive()
        (self.root / 'Cargo.lock').write_text('version=4\n[[package]]\n' + ''.join(f'{key}="{value}"\n' for key, value in self.cached_package.items()))
        self.commit()

    def write_cached_notice_archive(self, mutation=None, extra=()):
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode='w') as archive:
            for name, data in [*self.cached_files.items(), *extra]:
                entry = tarfile.TarInfo(self.cached_crate.name + '/' + name)
                entry.size = len(data)
                if mutation:
                    mutation(entry)
                if not entry.isfile():
                    entry.size = 0
                archive.addfile(entry, io.BytesIO(data) if entry.isfile() else None)
        self.cached_archive.write_bytes(gzip.compress(raw.getvalue()))
        self.cached_package['checksum'] = packaging.digest(self.cached_archive)

    def cached_notices(self, metadata=None):
        return packaging.cached_crate_notices(self.root / 'registry', self.cached_crate, self.cached_package,
                                             self.cached_metadata if metadata is None else metadata)

    def test_registry_cache_fallback_preserves_root_declared_component_and_embedded_font_notices(self):
        for name in ['ring', 'epaint_default_fonts']:
            self.cached_notice_fixture(name)
            archive_path = self.build('Windows')
            expected = {'LICENSE', 'legal/custom.txt', 'fonts/NOTICE.txt'} | packaging.EXTRA_CRATE_NOTICES.get(name, set()) | packaging.EMBEDDED_FONT_NOTICES.get(name, set())
            with zipfile.ZipFile(archive_path) as archive:
                prefix = archive_path.stem + '/licenses/' + self.cached_crate.name + '/'
                self.assertEqual({path.removeprefix(prefix) for path in archive.namelist() if path.startswith(prefix)}, expected)
                for path in expected:
                    self.assertEqual(archive.read(prefix + path), self.cached_files[path])
                self.assertFalse(any('LICENSE-private' in path or 'connection.txt' in path for path in archive.namelist()))

    def test_registry_cache_requires_locked_bytes_and_matching_extracted_metadata_and_notices(self):
        self.cached_notice_fixture()
        original = self.cached_archive.read_bytes()
        self.cached_archive.write_bytes(original + b'changed')
        with self.assertRaisesRegex(ValueError, 'Cargo.lock checksum'):
            self.cached_notices()
        self.cached_archive.write_bytes(original)
        checksum = self.cached_package.pop('checksum')
        with self.assertRaisesRegex(ValueError, 'locked dependency archive checksum'):
            self.cached_notices()
        self.cached_package['checksum'] = checksum
        with self.assertRaisesRegex(ValueError, 'metadata differs'):
            self.cached_notices(self.cached_metadata + b'# changed extracted metadata\n')
        notice = self.cached_crate / 'LICENSE-other-bits'
        notice.write_bytes(b'Tampered notice')
        with self.assertRaisesRegex(ValueError, 'notice checksum mismatch'):
            self.cached_notices()
        notice.write_bytes(self.cached_files['LICENSE-other-bits'])
        (self.cached_crate / 'LICENSE').unlink()
        with self.assertRaises(ValueError):
            self.cached_notices()

    def test_registry_cache_rejects_unsafe_members_and_missing_required_notices(self):
        self.cached_notice_fixture()
        for field, value in [('name', '../escape'), ('name', 'other-crate/LICENSE'),
                             ('type', tarfile.SYMTYPE), ('type', tarfile.LNKTYPE), ('type', tarfile.FIFOTYPE)]:
            def mutate(entry):
                if entry.name.endswith('/LICENSE'):
                    setattr(entry, field, value)
            with self.subTest(field=field, value=value):
                self.write_cached_notice_archive(mutate)
                with self.assertRaises(ValueError):
                    self.cached_notices()
        for name in ['LICENSE', 'license']:
            self.write_cached_notice_archive(extra=[(name, b'Duplicate')])
            with self.assertRaisesRegex(ValueError, 'duplicates'):
                self.cached_notices()
        del self.cached_files['LICENSE-other-bits']
        self.write_cached_notice_archive()
        with self.assertRaisesRegex(ValueError, 'required dependency notice inventory'):
            self.cached_notices()

    def test_registry_cache_hard_budgets_fail_closed_before_unbounded_reads(self):
        self.cached_notice_fixture()
        for constant, limit in [('MAX_CRATE_COMPRESSED_BYTES', 8), ('MAX_CRATE_DECODED_BYTES', 512),
                                ('MAX_CRATE_MEMBERS', 1), ('MAX_CRATE_NOTICE_BYTES', 8), ('MAX_CRATE_NOTICES_BYTES', 1)]:
            with self.subTest(limit=constant), mock.patch.object(packaging, constant, limit):
                with self.assertRaises(ValueError):
                    self.cached_notices()
        self.cached_archive.write_bytes(b'Not a gzip archive')
        self.cached_package['checksum'] = packaging.digest(self.cached_archive)
        with self.assertRaisesRegex(ValueError, 'lock-verified dependency notice archive'):
            self.cached_notices()

    def test_present_invalid_notice_inventory_never_uses_registry_cache_fallback(self):
        for name in ['ring', 'epaint_default_fonts']:
            self.cached_notice_fixture(name)
            for data in ['{"files":{}}', 'not JSON']:
                (self.cached_crate / '.cargo-checksum.json').write_text(data)
                with mock.patch.object(packaging, 'cached_crate_notices') as cached:
                    with self.assertRaises(ValueError):
                        self.build('Windows')
                    cached.assert_not_called()

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
