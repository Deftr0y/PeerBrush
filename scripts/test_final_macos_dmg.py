"""Offline final-DMG regressions. Synthetic archives; every Apple command is fake."""
import contextlib
import io
import json
import os
import pathlib
import plistlib
import stat
import subprocess
import sys
import unittest
from unittest import mock
import zipfile

import package_macos_dmg as dmg
import qualify_macos_dmg as qualifier
import test_sign_macos
from package import digest


class FinalImage(unittest.TestCase):
    def setUp(self):
        self.signer = test_sign_macos.Signing(methodName='test_success_final_hashes_source_preservation_and_native_validation')
        self.addCleanup(self.signer.doCleanups)
        self.signer.setUp()
        self.archive = self.signer.sign()
        self.base = self.signer.base
        self.image = self.base / 'images' / (self.archive.stem + '-dmg-signed-r2.dmg')
        self.image.parent.mkdir()
        self.image.write_bytes(b'SYNTHETIC_FINAL_IMAGE_NEVER_EXECUTED')
        self.work, self.output = self.base / 'inspection', self.base / 'qualification'
        self.kwargs = dict(root=self.signer.fixture.root.resolve(), archive=self.archive, image=self.image,
                           commit=self.signer.commit, version='0.2.0', archive_sha256=digest(self.archive),
                           image_sha256=digest(self.image), team_id=test_sign_macos.TEAM, work=self.work)
        self.calls, self.mounted, self.device_attached = [], False, False
        self.attached_image = None
        self.mount, self.application_link = None, None
        self.damage, self.native_failure, self.after_detach = None, None, None
        self.link_override, self.reparse_override = None, None
        self.mounted_modes = {}
        if os.name == 'nt':
            self.real_payload_inventory = dmg.payload_inventory
            self.patch(dmg, 'payload_inventory', self.payload_inventory)
        self.real_lstat = pathlib.Path.lstat
        self.patch(pathlib.Path, 'lstat', lambda path, *args, **kwargs: self.lstat(path, *args, **kwargs))
        self.patch(pathlib.Path, 'is_mount', lambda path: self.mounted and path == self.mount)
        self.patch(os, 'readlink', lambda path, **kwargs: '/elsewhere' if self.damage == 'link' else '/Applications')
        self.signer.runner.stop()
        self.patch(subprocess, 'run', self.native)

    def patch(self, owner, name, replacement):
        patcher = mock.patch.object(owner, name, replacement)
        patcher.start()
        self.addCleanup(patcher.stop)

    def lstat(self, path, *args, **kwargs):
        metadata = self.real_lstat(path, *args, **kwargs)
        if path in [self.application_link, self.link_override]:
            return os.stat_result((stat.S_IFLNK | 0o777, *metadata[1:]))
        if path == self.reparse_override:
            from types import SimpleNamespace
            return SimpleNamespace(st_mode=metadata.st_mode, st_file_attributes=0x400)
        return metadata

    def payload_inventory(self, root):
        # Windows chmod cannot model Unix execute bits. Keep the real bounded
        # walk/hash/link/identity checks, replacing only fake-mounted ZIP modes.
        inventory = self.real_payload_inventory(root)
        if root == self.mount:
            for name, record in inventory.items():
                if 'mode' in record and name in self.mounted_modes:
                    record['mode'] = self.mounted_modes[name]
        return inventory

    def native(self, command, **kwargs):
        if command[0] == 'git':
            return self.signer.real_run(command, **kwargs)
        self.calls.append(command)
        tool = pathlib.Path(command[0]).name
        if tool == 'hdiutil':
            if command[1] == 'verify':
                self.attached_image = command[-1]
                return subprocess.CompletedProcess(command, 0, stdout='', stderr='')
            if command[1] == 'info':
                entities = [{'dev-entry': '/dev/disk98'}, {'dev-entry': '/dev/disk98s1', 'mount-point': str(self.mount)}]
                if self.damage == 'extra-volume':
                    entities.append({'dev-entry': '/dev/disk98s2', 'mount-point': '/Volumes/synthetic-extra-volume'})
                images = [{'image-path': self.attached_image, 'system-entities': entities}] if self.device_attached else []
                return subprocess.CompletedProcess(command, 0, stdout=plistlib.dumps({'images': images}).decode(), stderr='')
            if command[1] == 'attach':
                self.mount = pathlib.Path(command[command.index('-mountpoint') + 1])
                self.mounted, self.device_attached = True, True
                # Simulate the exact producer layout independently of the
                # consumer's computed expected inventory. No real symlink or
                # OS mount is created in a portable test.
                self.mounted_modes = {}
                with zipfile.ZipFile(self.archive) as source:
                    prefix = 'PeerBrush-0.2.0-macOS-arm64/'
                    for entry in source.infolist():
                        relative = pathlib.PurePosixPath(entry.filename[len(prefix):])
                        path = self.mount / relative if relative.parts[0] == dmg.APP else self.mount / dmg.SUPPORT / relative
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_bytes(source.read(entry))
                        mode = entry.external_attr >> 16 & 0o777
                        path.chmod(mode)
                        self.mounted_modes[path.relative_to(self.mount).as_posix()] = mode
                self.application_link = self.mount / 'Applications'
                self.application_link.write_bytes(b'SYNTHETIC_LINK_PLACEHOLDER')
                if self.damage == 'app':
                    (self.mount / dmg.APP / 'Contents/MacOS/peerbrush').write_bytes(b'CHANGED')
                elif self.damage == 'source':
                    (self.mount / dmg.SUPPORT / 'PeerBrush-0.2.0-source.zip').write_bytes(b'CHANGED')
                elif self.damage == 'mode':
                    (self.mount / dmg.APP / 'Contents/MacOS/peerbrush').chmod(0o644)
                    self.mounted_modes[dmg.APP + '/Contents/MacOS/peerbrush'] = 0o644
                elif self.damage == 'extra':
                    (self.mount / 'unexpected-private-sidecar.json').write_bytes(b'MUST_REJECT')
                if self.native_failure == 'partial-attach':
                    raise subprocess.CalledProcessError(1, command)
                entities = [{'dev-entry': '/dev/disk98'}, {'dev-entry': '/dev/disk98s1', 'mount-point': str(self.mount)}]
                if self.damage == 'extra-volume':
                    entities.append({'dev-entry': '/dev/disk98s2', 'mount-point': '/Volumes/synthetic-extra-volume'})
                result = {'system-entities': entities}
                return subprocess.CompletedProcess(command, 0, stdout=plistlib.dumps(result).decode(), stderr='')
            if command[1] == 'detach':
                if self.native_failure == 'detach':
                    raise subprocess.CalledProcessError(1, command)
                self.assertEqual(command[-1], '/dev/disk98')
                self.mounted, self.device_attached = False, self.native_failure == 'retained-device'
                if self.after_detach is not None:
                    self.after_detach()
                return subprocess.CompletedProcess(command, 0, stdout='', stderr='')
            raise AssertionError('No native image creation is allowed in the consumer')
        if tool == 'codesign':
            self.assertNotIn('--sign', command)
            self.assertNotIn('--force', command)
            if command[-1].endswith('.dmg'):
                if self.native_failure == 'image-signature':
                    raise subprocess.CalledProcessError(1, command)
                details = ('Identifier=com.peerbrush.PeerBrush.diskimage\n'
                           f'TeamIdentifier={test_sign_macos.TEAM}\n'
                           f'Authority=Developer ID Application: Synthetic Test ({test_sign_macos.TEAM})\n'
                           'Timestamp=Oct 12, 2026 at 12:00:00\n')
                return subprocess.CompletedProcess(command, 0, stdout='', stderr=details)
        if tool == 'spctl' and command[-1].endswith('.dmg'):
            source = 'Developer ID' if self.native_failure == 'image-gatekeeper' else 'Notarized Developer ID'
            return subprocess.CompletedProcess(command, 0, stdout='', stderr='source=' + source + '\n')
        if tool == 'xcrun':
            self.assertEqual(command[1:3], ['stapler', 'validate'])
        self.assertIn(tool, {'codesign', 'xcrun', 'spctl'}, 'Consumer attempted a non-verification native command')
        return self.signer.fake_run(command, **kwargs)

    def inspect(self, **overrides):
        return dmg.inspect_final(**(self.kwargs | overrides))

    def test_independent_exact_content_and_native_checks_emit_only_strict_public_facts(self):
        for suffix in ['.verification.json', '.sha256']:
            pathlib.Path(str(self.image) + suffix).write_text('PRIVATE_FORGED_RECEIPT_CANARY')
        before = self.archive.read_bytes(), self.image.read_bytes()
        result = self.inspect()
        self.assertEqual(set(result), {'schema', 'format', 'version', 'commit', 'record', 'signed_zip',
                                      'executable_sha256', 'publisher_team_id', 'candidate_verified', 'ready',
                                      'production_ready', 'public_availability', 'content_checks', 'native_checks',
                                      'limits', 'native_job_provenance'})
        record = result['record']
        self.assertEqual(set(record), {'file', 'bytes', 'sha256', 'platform', 'architecture', 'binary_source_commit', 'source_sha256'})
        self.assertEqual(record['sha256'], digest(self.image))
        self.assertEqual(record['bytes'], self.image.stat().st_size)
        self.assertTrue(result['candidate_verified'])
        self.assertFalse(result['ready'])
        self.assertFalse(result['production_ready'])
        self.assertEqual(result['native_job_provenance'], 'requires-trusted-native-job-at-consumption')
        self.assertEqual(result['public_availability'], 'unverified')
        self.assertEqual(set(result['limits'].values()), {'not-performed', 'not-assessed'})
        encoded = json.dumps(result)
        for excluded in ['PRIVATE_FORGED_RECEIPT_CANARY', str(self.base), str(self.signer.fixture.root), 'notary_submission_id', 'payload']:
            self.assertNotIn(excluded, encoded)
        self.assertEqual(before, (self.archive.read_bytes(), self.image.read_bytes()))
        self.assertFalse(self.work.exists())
        attach = next(command for command in self.calls if command[0] == '/usr/bin/hdiutil' and command[1] == 'attach')
        for flag in ['-readonly', '-nobrowse', '-noautoopen', '-mountpoint']:
            self.assertIn(flag, attach)
        self.assertEqual(sum(command[0] == '/usr/bin/codesign' and '--verify' in command for command in self.calls), 3)

    def test_native_and_stable_source_contract_fail_before_work_or_native_commands(self):
        invalid = [dict(version='0.2.0-dev.1'), dict(version='00.2.0'), dict(commit='wrong'),
                   dict(team_id='bad'), dict(archive_sha256='A' * 64), dict(image_sha256='not-a-hash')]
        for changes in invalid:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.inspect(**changes)
        with mock.patch.object(dmg.sys, 'platform', 'linux'), self.assertRaisesRegex(ValueError, 'actual macOS'):
            self.inspect()
        self.assertFalse(self.work.exists())
        self.assertEqual(self.calls, [])

    def test_wrong_hashes_source_team_and_unsigned_inputs_never_mount(self):
        invalid = [dict(image_sha256='0' * 64), dict(archive_sha256='0' * 64), dict(commit='0' * 40),
                   dict(team_id='Z9Y8X7W6V5'), dict(archive=self.signer.archive, archive_sha256=digest(self.signer.archive))]
        for index, changes in enumerate(invalid):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.inspect(work=self.base / f'rejected-{index}', **changes)
        self.assertFalse(any(command[0] == '/usr/bin/hdiutil' for command in self.calls))

    def test_candidate_name_is_not_accepted_as_a_final_signed_image(self):
        candidate = self.image.with_name(self.archive.stem + '-dmg-candidate-r2.dmg')
        candidate.write_bytes(self.image.read_bytes())
        with self.assertRaisesRegex(ValueError, 'filenames'):
            self.inspect(image=candidate)
        self.assertFalse(self.work.exists())
        self.assertEqual(self.calls, [])

    def test_input_links_reparse_points_and_oversized_files_fail_without_native_commands(self):
        self.link_override = self.image
        with self.assertRaisesRegex(ValueError, 'links or reparse'):
            self.inspect()
        self.link_override, self.reparse_override = None, self.image.parent
        with self.assertRaisesRegex(ValueError, 'links or reparse'):
            self.inspect()
        self.reparse_override = None
        with mock.patch.object(dmg, 'MAX_IMAGE_BYTES', 8), self.assertRaisesRegex(ValueError, 'bounded regular'):
            self.inspect()
        self.assertEqual(self.calls, [])

    def test_private_work_rejects_other_clones_worktrees_input_overlap_and_existing_state(self):
        for marker_is_file in [False, True]:
            checkout = self.base / ('other-worktree' if marker_is_file else 'independent-clone')
            checkout.mkdir()
            if marker_is_file:
                (checkout / '.git').write_text('gitdir: SYNTHETIC_PRIVATE_GITDIR')
            else:
                (checkout / '.git').mkdir()
            with self.assertRaisesRegex(ValueError, 'every Git checkout'):
                self.inspect(work=checkout / 'private-inspection')
        for target in [self.signer.fixture.root / 'private-inspection', self.image.parent / 'private-inspection']:
            with self.assertRaises(ValueError):
                self.inspect(work=target)
        self.work.mkdir()
        (self.work / 'retained.txt').write_bytes(b'PRESERVE')
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.inspect()
        self.assertEqual((self.work / 'retained.txt').read_bytes(), b'PRESERVE')
        self.assertEqual(self.calls, [])

    def test_altered_mounted_app_source_modes_extra_files_or_shortcut_always_detach(self):
        for damage in ['app', 'source', 'mode', 'extra', 'link']:
            self.damage = damage
            with self.subTest(damage=damage), self.assertRaises(ValueError):
                self.inspect(work=self.base / ('damage-' + damage))
            self.assertFalse(self.mounted)
        self.assertEqual(sum(command[0] == '/usr/bin/hdiutil' and command[1] == 'detach' for command in self.calls), 5)

    def test_app_image_and_gatekeeper_failures_cannot_create_qualified_output(self):
        for failure in ['image-signature', 'image-gatekeeper']:
            self.native_failure = failure
            with self.subTest(failure=failure), self.assertRaises(ValueError):
                self.inspect(work=self.base / failure)
        self.native_failure, self.signer.signature_team = None, 'Z9Y8X7W6V5'
        with self.assertRaises(ValueError):
            self.inspect(work=self.base / 'app-signature')
        self.assertFalse(any(command[0] == '/usr/bin/hdiutil' for command in self.calls))

    def test_partial_attach_and_failed_detach_retain_work_without_recursive_cleanup(self):
        for failure in ['partial-attach', 'detach', 'retained-device']:
            self.native_failure = failure
            work = self.base / failure
            with self.subTest(failure=failure), mock.patch.object(dmg.shutil, 'rmtree') as cleanup, self.assertRaises(ValueError):
                self.inspect(work=work)
            cleanup.assert_not_called()
            self.assertTrue(work.is_dir())
            self.assertEqual(self.mounted, failure == 'detach')
            self.mounted, self.device_attached = False, False

    def test_extra_volume_is_rejected_and_the_exact_image_whole_device_is_detached(self):
        self.damage = 'extra-volume'
        with self.assertRaisesRegex(ValueError, 'exactly one volume'):
            self.inspect()
        self.assertFalse(self.mounted)
        self.assertFalse(self.device_attached)
        self.assertTrue(self.work.is_dir())
        self.assertEqual(next(command[-1] for command in self.calls if command[0] == '/usr/bin/hdiutil' and command[1] == 'detach'), '/dev/disk98')

    def test_ambiguous_device_inventory_and_preexisting_attachments_are_never_detached(self):
        for entries in [[{'dev-entry': '/dev/disk98s1'}], [{'dev-entry': '/dev/disk98'}, {'dev-entry': '/dev/disk99'}],
                        [{'dev-entry': '/private/unrelated-device'}]]:
            response = {'images': [{'image-path': str(self.image), 'system-entities': entries}]}
            with self.subTest(entries=entries), mock.patch.object(dmg, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=plistlib.dumps(response).decode())), self.assertRaises(ValueError):
                dmg.attached_image_device(self.image)
        self.device_attached = True
        with self.assertRaisesRegex(ValueError, 'already attached'):
            dmg.verify_image(self.image, self.base / 'preexisting-mount', {})
        self.assertFalse(any(command[0] == '/usr/bin/hdiutil' and command[1] in {'attach', 'detach'} for command in self.calls))
        self.device_attached = False

    def test_original_and_owned_input_mutations_prevent_a_result(self):
        for index, owned in enumerate([False, True]):
            def mutate():
                target = self.mount.parent / 'inputs' / self.image.name if owned else self.image
                target.chmod(0o600)
                target.write_bytes(b'CHANGED_AFTER_NATIVE_CHECKS')
            self.after_detach = mutate
            with self.subTest(owned=owned), self.assertRaisesRegex(ValueError, 'changed during final inspection'):
                self.inspect(work=self.base / f'mutated-{index}')
            self.image.write_bytes(b'SYNTHETIC_FINAL_IMAGE_NEVER_EXECUTED')

    def test_source_change_and_retained_mount_prevent_success_cleanup(self):
        with mock.patch.object(dmg, 'git', return_value=b'0' * 40), self.assertRaisesRegex(ValueError, 'source changed'):
            self.inspect()
        self.assertTrue(self.work.is_dir())
        self.native_failure = 'detach'
        with self.assertRaises(ValueError):
            self.inspect(work=self.base / 'retained-mount')
        self.assertTrue(self.mounted)

    def test_payload_inventory_bounds_entries_and_file_reads(self):
        payload = self.base / 'oversized-payload'
        payload.mkdir()
        (payload / 'huge').write_bytes(b'x' * 17)
        with mock.patch.object(dmg, 'MAX_IMAGE_BYTES', 16), self.assertRaisesRegex(ValueError, 'bounded regular'):
            dmg.payload_inventory(payload)
        with mock.patch.object(dmg, 'MAX_PAYLOAD_ENTRIES', 0), self.assertRaisesRegex(ValueError, 'entry or path budget'):
            dmg.payload_inventory(payload)
        path = payload / 'huge'
        metadata = path.stat()
        changed = list(metadata)
        changed[6] += 1
        with mock.patch.object(dmg.os, 'fstat', side_effect=[metadata, os.stat_result(changed)]), self.assertRaisesRegex(ValueError, 'changed during its bounded read'):
            dmg.bounded_file(path, 32)

    def test_cli_writes_only_one_immutable_sanitized_report(self):
        result = qualifier.qualify(output=self.output, **self.kwargs)
        report = self.output / 'dmg-qualification.json'
        self.assertEqual(json.loads(report.read_bytes()), result)
        self.assertEqual({path.name for path in self.output.iterdir()}, {'dmg-qualification.json'})
        before = report.read_bytes()
        with mock.patch.object(qualifier, 'inspect_final') as inspect, self.assertRaisesRegex(ValueError, 'immutable'):
            qualifier.qualify(output=self.output, **self.kwargs)
        inspect.assert_not_called()
        self.assertEqual(report.read_bytes(), before)

    def test_cli_failure_does_not_expose_native_logs_paths_or_create_output(self):
        arguments = ['qualify_macos_dmg.py', '--output', str(self.output)]
        for name, value in self.kwargs.items():
            arguments.extend(['--source-root' if name == 'root' else '--' + name.replace('_', '-'), str(value)])
        stream = io.StringIO()
        with mock.patch.object(sys, 'argv', arguments), mock.patch.object(qualifier, 'inspect_final', side_effect=ValueError('PRIVATE_PATH_NATIVE_LOG_CANARY')), contextlib.redirect_stderr(stream):
            self.assertEqual(qualifier.main(), 1)
        self.assertNotIn('CANARY', stream.getvalue())
        self.assertNotIn(str(self.base), stream.getvalue())
        self.assertFalse(self.output.exists())


if __name__ == '__main__':
    unittest.main()
