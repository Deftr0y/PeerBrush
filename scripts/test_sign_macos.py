"""Portable synthetic tests. These never sign, upload, notarize or run macOS tools."""
import contextlib
import hashlib
import io
import json
import pathlib
import plistlib
import subprocess
import tempfile
import unittest
from unittest import mock
import zipfile

import sign_macos
import verify_release
import test_package
from test_package import packaging


TEAM = 'A1B2C3D4E5'
IDENTITY = '0123456789ABCDEF0123456789ABCDEF01234567'
SUBMISSION = '12345678-1234-1234-1234-123456789abc'


class Signing(unittest.TestCase):
    def setUp(self):
        # Reuse the isolated, committed synthetic GPL/source fixtures, not any
        # real checkout, native executable, credentials or external service.
        self.fixture = test_package.Packages(methodName='test_platform_architecture_source_license_and_macos_bundle_match')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.archive = self.fixture.build('macOS')
        self.commit = packaging.git(self.fixture.root, 'rev-parse', 'HEAD').decode().strip()
        self.external = tempfile.TemporaryDirectory(prefix='peerbrush-sign-test-')
        self.addCleanup(self.external.cleanup)
        self.base = pathlib.Path(self.external.name).resolve()
        self.work = self.base / 'work'
        self.output = self.base / 'output'
        self.calls = []
        self.status = 'Accepted'
        self.interrupt_wait = False
        self.gatekeeper = True
        self.entitlements = ''
        self.identity_team = TEAM
        self.signature_team = TEAM
        self.notary_issues = None
        self.signature_extra = False
        self.staple_existing_change = False
        self.log_wrong_hash = False
        self.kwargs = dict(archive=self.archive, output=self.output, work=self.work,
                           commit=self.commit, version='0.2.0', expected_sha256=packaging.digest(self.archive),
                           team_id=TEAM, identity=IDENTITY, profile='synthetic-existing-profile', revision='1',
                           source_root=self.fixture.root)
        self.platform = mock.patch.object(sign_macos.sys, 'platform', 'darwin')
        self.platform.start()
        self.addCleanup(self.platform.stop)
        self.real_run = subprocess.run
        self.runner = mock.patch.object(sign_macos.subprocess, 'run', side_effect=self.fake_run)
        self.runner.start()
        self.addCleanup(self.runner.stop)

    def fake_run(self, command, **kwargs):
        if command[0] == 'git':
            return self.real_run(command, **kwargs)
        self.calls.append(command)
        out, err = '', ''
        tool = pathlib.Path(command[0]).name
        if tool == 'security':
            out = f'  1) {IDENTITY} "Developer ID Application: Synthetic Test ({self.identity_team})"\n'
        elif tool == 'codesign':
            app = pathlib.Path(command[-1])
            if '--sign' in command:
                with (app / 'Contents/MacOS/peerbrush').open('ab') as stream:
                    stream.write(b'SYNTHETIC_SIGNED_BYTES')
                resources = app / 'Contents/_CodeSignature/CodeResources'
                resources.parent.mkdir()
                resources.write_bytes(b'SYNTHETIC_RESOURCE_SEAL')
                if self.signature_extra:
                    (app / 'Contents/_CodeSignature/private.txt').write_bytes(b'CANARY')
            elif '--entitlements' in command:
                out = self.entitlements
            elif '--display' in command:
                err = (f'Identifier={sign_macos.BUNDLE_ID}\nTeamIdentifier={self.signature_team}\n'
                       f'Authority=Developer ID Application: Synthetic Test ({self.signature_team})\n'
                       'CodeDirectory v=20500 size=120 flags=0x10000(runtime) hashes=12\n'
                       'Timestamp=Oct 12, 2026 at 12:00:00\n')
        elif tool == 'ditto':
            pathlib.Path(command[-1]).write_bytes(b'SYNTHETIC_SUBMISSION_ZIP')
        elif tool == 'spctl':
            err = 'accepted\nsource=Notarized Developer ID\n' if self.gatekeeper else 'source=Developer ID\n'
        elif tool == 'xcrun' and 'stapler' in command:
            if 'staple' in command:
                app = pathlib.Path(command[-1])
                (app / 'Contents/CodeResources').write_bytes(b'SYNTHETIC_STAPLED_TICKET')
                if self.staple_existing_change:
                    (app / 'Contents/Info.plist').write_bytes(b'altered')
        elif tool == 'xcrun' and 'notarytool' in command:
            if 'submit' in command:
                out = json.dumps({'id': SUBMISSION})
            elif 'wait' in command:
                if self.interrupt_wait:
                    raise subprocess.TimeoutExpired(command, 10)
            elif 'info' in command:
                out = json.dumps({'id': SUBMISSION, 'status': self.status})
            elif 'log' in command:
                out = json.dumps({'jobId': SUBMISSION.upper(), 'status': 'Accepted',
                                  'sha256': '0'*64 if self.log_wrong_hash else packaging.digest(self.work / 'submission.zip'),
                                  'issues': self.notary_issues})
        else:
            raise AssertionError(f'Unexpected native command: {command}')
        return subprocess.CompletedProcess(command, 0, stdout=out, stderr=err)

    def sign(self, **overrides):
        return sign_macos.sign_archive(**(self.kwargs | overrides))

    def test_success_final_hashes_source_preservation_and_native_validation(self):
        original = self.archive.read_bytes()
        final = self.sign()
        result = verify_release.inspect_archive(final, self.commit, '0.2.0', self.fixture.root)
        unsigned = verify_release.inspect_archive(self.archive, self.commit, '0.2.0', self.fixture.root)
        self.assertEqual(result['source'], unsigned['source'])
        self.assertEqual(self.archive.read_bytes(), original)
        self.assertEqual(result['manifest']['macos_signing']['unsigned_executable_sha256'], unsigned['record']['executable_sha256'])
        self.assertNotEqual(result['record']['executable_sha256'], unsigned['record']['executable_sha256'])
        self.assertEqual(result['manifest']['binary_source_commit'], self.commit)
        self.assertEqual(result['record']['signing_verification'], 'metadata-and-hashes-only')
        self.assertEqual(set(result['manifest']['macos_signing']['metadata_files']), verify_release.MACOS_SIGNING_FILES)
        receipt = json.loads(pathlib.Path(str(final) + '.verification.json').read_text())
        self.assertEqual(receipt['sha256'], packaging.digest(final))
        self.assertEqual(receipt['fresh_quarantined_download_test'], 'not-performed')
        signs = [c for c in self.calls if '--sign' in c]
        self.assertEqual(len(signs), 1)
        self.assertIn('--timestamp', signs[0])
        self.assertIn('runtime', signs[0])
        self.assertNotIn('--deep', signs[0])
        self.assertEqual(sum('submit' in c for c in self.calls), 1)
        self.assertEqual(sum('spctl' in c[0] for c in self.calls), 2)
        self.assertTrue(any('final-check' in c[-1] for c in self.calls if 'spctl' in c[0]))
        self.assertFalse(any(any(x in c for x in ['store-credentials', 'import', 'unlock-keychain', 'set-key-partition-list']) for c in self.calls))
        self.assertNotIn(self.kwargs['profile'], final.read_bytes().decode('latin1'))

    def test_installed_source_identity_is_sealed_before_signing_and_never_rewritten(self):
        self.fixture.require_installed_macos_source()
        self.archive = self.fixture.build('macOS')
        self.commit = packaging.git(self.fixture.root, 'rev-parse', 'HEAD').decode().strip()
        self.kwargs.update(archive=self.archive, commit=self.commit,
                           expected_sha256=packaging.digest(self.archive))
        with zipfile.ZipFile(self.archive) as archive:
            resources = self.archive.stem + '/' + packaging.MACOS_RESOURCES
            names = [resources + packaging.MACOS_INSTALLED_RELEASE,
                     resources + 'PeerBrush-0.2.0-source.zip']
            before = {name: archive.read(name) for name in names}
        final = self.sign()
        inspected = verify_release.inspect_archive(final, self.commit, '0.2.0', self.fixture.root)
        with zipfile.ZipFile(final) as archive:
            self.assertEqual(before, {name: archive.read(name) for name in names})
            identity = json.loads(before[names[0]])
            self.assertEqual(identity, packaging.macos_installed_release(inspected['manifest']))
            self.assertNotIn('macos_signing', identity)
            self.assertEqual(identity['source_sha256'], packaging.digest(
                self.work / 'payload' / self.archive.stem / 'PeerBrush.app/Contents/Resources/PeerBrush-0.2.0-source.zip'))

    def test_missing_identity_team_profile_and_runner_fail_before_native_commands(self):
        for overrides in [dict(team_id=''), dict(identity='-'), dict(profile=''), dict(revision='0')]:
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.sign(**overrides)
        with mock.patch.object(sign_macos.sys, 'platform', 'linux'), self.assertRaisesRegex(ValueError, 'actual macOS'):
            self.sign()
        self.assertEqual(self.calls, [])

    def test_prereleases_fail_before_any_provider_access_or_signing_work(self):
        original = self.archive.read_bytes()
        for version in ['0.30.0-dev.1', '0.30.0-beta.1', '0.30.0-rc.1', '0.30.0+build']:
            for resume in [False, True]:
                with self.subTest(version=version, resume=resume), self.assertRaisesRegex(ValueError, 'stable A.B.C release'):
                    self.sign(version=version, resume=resume)
        self.assertEqual(self.calls, [])
        self.assertEqual(self.archive.read_bytes(), original)
        self.assertFalse(self.work.exists())
        self.assertFalse(self.output.exists())

    def test_explicit_file_keychain_reaches_identity_signing_and_every_notary_command(self):
        keychain = self.base / 'synthetic signing.keychain-db'
        keychain.write_bytes(b'SYNTHETIC_NON_SECRET_KEYCHAIN')
        final = self.sign(keychain=keychain)
        notary = [c for c in self.calls if 'notarytool' in c and '--find' not in c]
        self.assertEqual({c[2] for c in notary}, {'history', 'submit', 'wait', 'info', 'log'})
        for command in notary:
            self.assertEqual(command.count('--keychain'), 1)
            self.assertEqual(command[command.index('--keychain') + 1], str(keychain))
            self.assertEqual(command[command.index('--keychain-profile') + 1], self.kwargs['profile'])
        identity = next(c for c in self.calls if 'find-identity' in c)
        self.assertEqual(identity[-1], str(keychain))
        signed = next(c for c in self.calls if '--sign' in c)
        self.assertEqual(signed[signed.index('--keychain') + 1], str(keychain))
        self.assertNotIn(str(keychain), final.read_bytes().decode('latin1'))

    def test_default_keychain_behavior_is_unchanged(self):
        self.sign()
        self.assertFalse(any('--keychain' in c for c in self.calls))

    def test_invalid_explicit_keychains_fail_before_native_commands(self):
        for path in [pathlib.Path('relative.keychain-db'), self.base / 'missing', self.base]:
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, 'explicit keychain'):
                self.sign(keychain=path)
        self.assertEqual(self.calls, [])

    def test_linked_explicit_keychains_fail_before_native_commands(self):
        keychain = self.base / 'regular.keychain-db'
        keychain.write_bytes(b'SYNTHETIC_NON_SECRET_KEYCHAIN')
        symlink = self.base / 'linked.keychain-db'
        linked_directory = self.base / 'linked-directory'
        try:
            symlink.symlink_to(keychain)
            linked_directory.symlink_to(self.base, target_is_directory=True)
        except OSError as error:
            if getattr(error, 'winerror', None) == 1314:
                self.skipTest('Windows symlink creation privilege is unavailable')
            raise
        for path in [symlink, linked_directory / keychain.name]:
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, 'explicit keychain'):
                self.sign(keychain=path)
        self.assertEqual(self.calls, [])

    def test_resume_uses_selected_keychain_without_new_submission(self):
        first = self.base / 'first.keychain-db'
        second = self.base / 'second.keychain-db'
        first.write_bytes(b'SYNTHETIC_NON_SECRET_KEYCHAIN')
        second.write_bytes(b'SYNTHETIC_NON_SECRET_KEYCHAIN')
        self.interrupt_wait = True
        with self.assertRaisesRegex(ValueError, 'did not complete'):
            self.sign(keychain=first)
        self.interrupt_wait = False
        initial_calls = len(self.calls)
        self.sign(resume=True, keychain=second)
        for command in self.calls[initial_calls:]:
            if 'notarytool' in command and '--find' not in command:
                self.assertEqual(command[command.index('--keychain') + 1], str(second))
        self.assertEqual(sum('submit' in c for c in self.calls), 1)
        self.assertEqual(sum('--sign' in c for c in self.calls), 1)

    def test_public_submission_receipt_is_emitted_before_interrupted_wait(self):
        self.interrupt_wait = True
        receipts = []
        with self.assertRaisesRegex(ValueError, 'did not complete'):
            self.sign(on_submission=receipts.append)
        self.assertEqual(receipts, [{'submission_id': SUBMISSION, 'commit': self.commit,
                                    'unsigned_archive_sha256': self.kwargs['expected_sha256'],
                                    'submission_archive_sha256': packaging.digest(self.work / 'submission.zip')}])
        self.assertNotIn(self.kwargs['profile'], json.dumps(receipts))

    def test_wrong_archive_hash_or_unsafe_work_fail_before_signing(self):
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            self.sign(expected_sha256='0'*64)
        with self.assertRaisesRegex(ValueError, 'outside the source'):
            self.sign(work=self.fixture.root.resolve() / 'private-signing')
        self.assertEqual(self.calls, [])

    def test_wrong_keychain_identity_or_team_never_signs(self):
        self.identity_team = 'Z9Y8X7W6V5'
        with self.assertRaisesRegex(ValueError, 'not available'):
            self.sign()
        self.assertFalse(any('--sign' in c for c in self.calls))

    def test_signature_identity_mismatch_and_unexpected_entitlements_fail(self):
        self.signature_team = 'Z9Y8X7W6V5'
        with self.assertRaisesRegex(ValueError, 'Native signature'):
            self.sign()
        self.assertFalse(any('submit' in c for c in self.calls))
        self.signature_team = TEAM
        self.entitlements = plistlib.dumps({'com.apple.security.get-task-allow': True}).decode()
        with self.assertRaisesRegex(ValueError, 'Unexpected entitlements'):
            self.sign(work=self.base / 'other-work')
        self.assertFalse(any('submit' in c for c in self.calls))

    def test_unreviewed_codesign_files_fail_before_notarization(self):
        self.signature_extra = True
        with self.assertRaisesRegex(ValueError, 'outside the reviewed'):
            self.sign()
        self.assertFalse(any('submit' in c for c in self.calls))

    def test_pending_and_rejected_submissions_emit_no_artifact(self):
        self.status = 'In Progress'
        with self.assertRaisesRegex(ValueError, 'not Accepted'):
            self.sign()
        self.assertFalse(self.output.exists())
        self.assertEqual(json.loads((self.work / 'state.json').read_text())['submission_id'], SUBMISSION)
        self.status = 'Invalid'
        with self.assertRaisesRegex(ValueError, 'not Accepted'):
            self.sign(resume=True)
        self.assertEqual(sum('submit' in c for c in self.calls), 1)
        self.assertFalse(any('staple' in c for c in self.calls))

    def test_timeout_resumes_same_submission_without_signing_or_uploading_again(self):
        self.interrupt_wait = True
        with self.assertRaisesRegex(ValueError, 'did not complete'):
            self.sign()
        self.assertFalse(self.output.exists())
        self.interrupt_wait = False
        final = self.sign(resume=True)
        self.assertTrue(final.is_file())
        self.assertEqual(sum('submit' in c for c in self.calls), 1)
        self.assertEqual(sum('--sign' in c for c in self.calls), 1)

    def test_resume_rejects_changed_work(self):
        self.status = 'In Progress'
        with self.assertRaises(ValueError):
            self.sign()
        (self.work / 'payload' / self.archive.stem / 'private.txt').write_bytes(b'CANARY')
        with self.assertRaisesRegex(ValueError, 'changed after submission'):
            self.sign(resume=True)
        self.assertEqual(sum('submit' in c for c in self.calls), 1)

    def test_gatekeeper_failure_can_resume_the_accepted_submission(self):
        self.gatekeeper = False
        with self.assertRaisesRegex(ValueError, 'Gatekeeper'):
            self.sign()
        self.assertFalse(self.output.exists())
        self.gatekeeper = True
        final = self.sign(resume=True)
        self.assertTrue(final.is_file())
        self.assertEqual(sum('submit' in c for c in self.calls), 1)

    def test_notary_log_hash_mismatch_and_warnings_block_distribution(self):
        self.log_wrong_hash = True
        with self.assertRaisesRegex(ValueError, 'exact submitted archive'):
            self.sign()
        self.log_wrong_hash = False
        self.notary_issues = [{'severity': 'warning', 'message': 'synthetic warning'}]
        with self.assertRaisesRegex(ValueError, 'contains issues'):
            self.sign(resume=True)
        self.assertFalse(self.output.exists())
        self.assertFalse(any('staple' in c for c in self.calls))

    def test_stapling_cannot_change_preexisting_files(self):
        self.staple_existing_change = True
        with self.assertRaisesRegex(ValueError, 'Stapling changed'):
            self.sign()
        self.assertFalse(self.output.exists())

    def test_existing_final_artifact_is_never_replaced(self):
        self.output.mkdir()
        target = self.output / (self.archive.stem + '-apple-signed-r1.zip')
        target.write_bytes(b'PRESERVE_EXISTING')
        with self.assertRaisesRegex(ValueError, 'Refusing to replace'):
            self.sign()
        self.assertEqual(target.read_bytes(), b'PRESERVE_EXISTING')
        self.assertEqual(self.calls, [])

    def test_signed_inventory_rejects_extra_tampered_and_missing_apple_files(self):
        final = self.sign()
        pristine = final.read_bytes()
        with zipfile.ZipFile(final) as archive:
            files = [(i.filename, archive.read(i), i.external_attr >> 16 & 0o777) for i in archive.infolist()]
        prefix = self.archive.stem + '/'
        mutations = [files + [(prefix + 'PeerBrush.app/Contents/_CodeSignature/private.txt', b'CANARY', 0o644)],
                     [(n, d + b'changed' if n.endswith('/Contents/CodeResources') else d, m) for n, d, m in files],
                     [(n, d, m) for n, d, m in files if not n.endswith('/Contents/CodeResources')]]
        for changed in mutations:
            packaging.write_zip(final, changed, 1700000000)
            with self.assertRaises((ValueError, KeyError)):
                verify_release.inspect_archive(final, self.commit, '0.2.0', self.fixture.root)
        final.write_bytes(pristine)

    def test_complete_matrix_accepts_signed_mac_but_still_requires_matching_source(self):
        final = self.sign()
        self.runner.stop()
        self.fixture.build('Windows')
        self.fixture.build('Linux')
        matrix = self.base / 'matrix'
        matrix.mkdir()
        for path in [final, self.fixture.output / 'PeerBrush-0.2.0-Windows-x64.zip', self.fixture.output / 'PeerBrush-0.2.0-Linux-x64.zip']:
            (matrix / path.name).write_bytes(path.read_bytes())
        with contextlib.redirect_stdout(io.StringIO()):
            verify_release.verify(matrix, self.commit, '0.2.0', self.base / 'verified', self.fixture.root)
        inventory = json.loads((self.base / 'verified/release-inventory.json').read_text())
        self.assertEqual(len(inventory['assets']), 4)
        mac = next(a for a in inventory['assets'] if a.get('platform') == 'macOS')
        self.assertEqual(mac['native_publisher_signing'], 'developer-id-notarized')
        self.assertEqual(mac['signing_verification'], 'metadata-and-hashes-only')


if __name__ == '__main__':
    unittest.main()
