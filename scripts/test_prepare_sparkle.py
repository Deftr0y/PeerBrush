"""Offline synthetic input/staging regressions; no third-party code is executed."""
import hashlib
import json
import os
import pathlib
import stat
import subprocess
import tempfile
import unittest
from unittest import mock
import zipfile

import prepare_sparkle as sparkle


class Pin(unittest.TestCase):
    def test_complete_source_notice_matches_the_pinned_upstream_license(self):
        pin = sparkle.load_pin()
        # Source text can have checkout CRLF; staged upstream bytes are never normalized.
        source_notice = (sparkle.ROOT / 'packaging/sparkle/LICENSE').read_text(encoding='utf-8').encode('utf-8')
        self.assertEqual(hashlib.sha256(source_notice).hexdigest(), pin['license_sha256'])


class Preparation(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='peerbrush-sparkle-test-')
        self.addCleanup(temporary.cleanup)
        self.base = pathlib.Path(temporary.name).resolve()
        self.root = self.base / 'source'
        self.root.mkdir()
        self.output = self.base / 'prepared'
        self.archive = self.base / 'synthetic.zip'
        self.prefix = 'Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework/'
        self.entries = [(self.prefix, stat.S_IFDIR | 0o755, b''),
                        (self.prefix + 'Versions/', stat.S_IFDIR | 0o755, b''),
                        (self.prefix + 'Versions/B/', stat.S_IFDIR | 0o755, b''),
                        (self.prefix + 'Versions/B/Sparkle', stat.S_IFREG | 0o755, b'SYNTHETIC_NOT_EXECUTABLE'),
                        (self.prefix + 'Versions/Current', stat.S_IFLNK | 0o755, b'B'),
                        (self.prefix + 'Sparkle', stat.S_IFLNK | 0o755, b'Versions/Current/Sparkle'),
                        ('LICENSE', stat.S_IFREG | 0o644, b'Synthetic reviewed notice\n')]
        self.write(self.entries)
        self.pin = {'schema': 1, 'version': '2.10.0', 'source_commit': '1' * 40,
                    'archive_bytes': self.archive.stat().st_size, 'archive_sha256': sparkle.digest(self.archive),
                    'framework_prefix': self.prefix, 'license_sha256': hashlib.sha256(self.entries[-1][2]).hexdigest(),
                    'framework_entries': {'.': {'kind': 'directory', 'mode': 0o755},
                                          'Versions': {'kind': 'directory', 'mode': 0o755},
                                          'Versions/B': {'kind': 'directory', 'mode': 0o755},
                                          'Versions/B/Sparkle': {'kind': 'file', 'mode': 0o755,
                                                                 'bytes': len(self.entries[3][2]), 'sha256': hashlib.sha256(self.entries[3][2]).hexdigest()},
                                          'Versions/Current': {'kind': 'symlink', 'target': 'B'},
                                          'Sparkle': {'kind': 'symlink', 'target': 'Versions/Current/Sparkle'}}}
        pin_file = self.root / 'pin.json'
        pin_file.write_text(json.dumps(self.pin))
        for patch in [mock.patch.object(sparkle, 'PIN', pin_file),
                      mock.patch.object(sparkle, 'load_pin', return_value=self.pin),
                      mock.patch.object(sparkle.sys, 'platform', 'darwin'),
                      mock.patch.object(sparkle.platform, 'mac_ver', return_value=('12.0', ('', '', ''), 'arm64'))]:
            patch.start()
            self.addCleanup(patch.stop)

    def write(self, entries):
        with zipfile.ZipFile(self.archive, 'w') as archive:
            for name, mode, content in entries:
                entry = zipfile.ZipInfo(name)
                entry.create_system = 3
                entry.external_attr = mode << 16
                archive.writestr(entry, content)

    def reselect_archive(self):
        # Test the deeper inventory boundary using a selected synthetic archive,
        # never an override exposed by the preparation CLI.
        self.pin.update(archive_bytes=self.archive.stat().st_size, archive_sha256=sparkle.digest(self.archive))

    def test_exact_archive_checks_do_not_stage_or_execute_code(self):
        self.assertEqual(sparkle.inspect(self.archive, self.pin), self.archive)
        self.assertFalse(self.output.exists())
        self.archive.write_bytes(self.archive.read_bytes() + b'changed')
        with self.assertRaisesRegex(ValueError, 'official checksum'):
            sparkle.prepare(self.archive, self.output, self.root)
        self.assertFalse(self.output.exists())

    def test_unknown_paths_bytes_modes_and_symlink_targets_fail_before_writes(self):
        for replacement in [
            (self.prefix + 'Versions/B/Sparkle', stat.S_IFREG | 0o755, b'changed'),
            (self.prefix + 'Versions/B/Sparkle', stat.S_IFREG | 0o644, self.entries[3][2]),
            (self.prefix + 'Versions/B/Sparkle', stat.S_IFIFO | 0o644, b''),
            (self.prefix + 'Versions/Current', stat.S_IFLNK | 0o755, b'../../outside'),
            ('LICENSE', stat.S_IFREG | 0o644, b'changed')]:
            entries = [(name, mode, content) for name, mode, content in self.entries if name != replacement[0]] + [replacement]
            self.write(entries)
            self.reselect_archive()
            with self.subTest(replacement=replacement[0]), self.assertRaises(ValueError):
                sparkle.prepare(self.archive, self.output, self.root)
            self.assertFalse(self.output.exists())

    def test_ambiguous_and_traversing_archive_paths_are_rejected(self):
        for name in ['../escape', self.prefix + 'Versions/B/sparkle']:
            self.write(self.entries + [(name, stat.S_IFREG | 0o644, b'bad')])
            self.reselect_archive()
            with self.subTest(name=name), self.assertRaises(ValueError):
                sparkle.inspect(self.archive, self.pin)

    def test_unsupported_os_is_rejected_before_output(self):
        for version in ['11.0', '10.15']:
            with mock.patch.object(sparkle.platform, 'mac_ver', return_value=(version, (), 'arm64')):
                with self.assertRaisesRegex(ValueError, 'requires macOS12'):
                    sparkle.prepare(self.archive, self.output, self.root)
        self.assertFalse(self.output.exists())

    def test_outputs_reject_checkout_existing_paths_and_reparse_points(self):
        with self.assertRaisesRegex(ValueError, 'outside every source checkout'):
            sparkle.external_output(self.root / 'generated', self.root)
        self.output.mkdir()
        (self.output / 'canary').write_bytes(b'keep')
        with self.assertRaisesRegex(ValueError, 'immutable'):
            sparkle.prepare(self.archive, self.output, self.root)
        self.assertEqual((self.output / 'canary').read_bytes(), b'keep')
        with mock.patch.object(pathlib.Path, 'lstat') as metadata:
            metadata.return_value.st_mode = stat.S_IFDIR | 0o755
            metadata.return_value.st_file_attributes = 0x400
            with self.assertRaisesRegex(ValueError, 'reparse'):
                sparkle.external_output(self.base / 'not-created', self.root)

    def test_outputs_reject_other_registered_source_worktrees(self):
        for arguments in [('init', '-q'), ('add', 'pin.json'),
                          ('-c', 'user.name=Package Tests', '-c', 'user.email=package-tests@example.invalid', 'commit', '-qm', 'Synthetic fixture')]:
            subprocess.run(['git', '-C', str(self.root), *arguments], check=True, capture_output=True)
        other = self.base / 'other-checkout'
        subprocess.run(['git', '-C', str(self.root), 'worktree', 'add', '--detach', str(other), 'HEAD'], check=True, capture_output=True)
        with self.assertRaisesRegex(ValueError, 'outside every source checkout'):
            sparkle.external_output(other / 'private-output', self.root)
        self.assertEqual(sparkle.external_output(self.output, self.root), self.output)
        with mock.patch.object(sparkle.subprocess, 'check_output', side_effect=subprocess.CalledProcessError(1, ['git'])):
            with self.assertRaisesRegex(ValueError, 'Cannot identify every source checkout'):
                sparkle.external_output(self.output, self.root)

    @unittest.skipIf(os.name == 'nt', 'Native Unix framework symlink staging; covered by macOS/Linux CI')
    def test_staging_is_exact_private_immutable_and_disabled(self):
        before = self.archive.read_bytes()
        sparkle.prepare(self.archive, self.output, self.root)
        self.assertEqual(self.archive.read_bytes(), before)
        self.assertEqual(self.output.stat().st_mode & 0o777, 0o700)
        self.assertEqual(sparkle.tree(self.output / 'Sparkle.framework'), self.pin['framework_entries'])
        receipt = json.loads((self.output / 'preparation.json').read_text())
        self.assertFalse(receipt['updater_started'])
        self.assertFalse(receipt['update_installation_enabled'])
        self.assertFalse(receipt['release_ready'])
        self.assertEqual(receipt['feed'], 'unconfigured')
        self.assertEqual(receipt['public_key'], 'unconfigured')
        receipt['release_ready'] = True
        (self.output / 'preparation.json').write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, 'disabled exact pin'):
            sparkle.verify_stage(self.output, self.pin)
        with self.assertRaisesRegex(ValueError, 'immutable'):
            sparkle.prepare(self.archive, self.output, self.root)

    @unittest.skipIf(os.name == 'nt', 'Native Unix framework symlink staging; covered by macOS/Linux CI')
    def test_staged_tampering_unknown_files_hard_links_and_escaping_links_fail(self):
        sparkle.prepare(self.archive, self.output, self.root)
        framework = self.output / 'Sparkle.framework'
        binary = framework / 'Versions/B/Sparkle'
        original = binary.read_bytes()
        binary.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'differs'):
            sparkle.verify_stage(self.output, self.pin)
        binary.write_bytes(original)
        outside = self.base / 'outside'
        os.link(binary, outside)
        with self.assertRaisesRegex(ValueError, 'hard links'):
            sparkle.verify_stage(self.output, self.pin)
        outside.unlink()
        link = framework / 'Sparkle'
        link.unlink()
        outside.write_bytes(original)
        link.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, 'escapes'):
            sparkle.verify_stage(self.output, self.pin)


if __name__ == '__main__':
    unittest.main()
