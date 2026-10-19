"""Synthetic DMG signing/notary tests; no credentials or Apple services/tools."""
import json
import pathlib
import subprocess
import unittest
from unittest import mock

import package_macos_dmg
import sign_macos
import test_macos_dmg
from package import digest
from test_sign_macos import TEAM, IDENTITY, SUBMISSION


class DmgSigning(unittest.TestCase):
    def setUp(self):
        self.preparation = test_macos_dmg.Preparation()
        self.preparation.setUp()
        self.addCleanup(self.preparation.doCleanups)
        self.signing = self.preparation.signing
        self.archive = self.preparation.archive
        self.base = self.signing.base
        self.work = self.base / 'dmg-signing-work'
        self.output = self.base / 'dmg-signed'
        self.kwargs = dict(archive=self.archive, output=self.output, work=self.work,
                           commit=self.signing.commit, version='0.2.0', expected_sha256=digest(self.archive),
                           team_id=TEAM, identity=IDENTITY, profile='synthetic-existing-profile', revision='1',
                           source_root=self.signing.fixture.root)
        sign_macos.extract_archive(self.archive, self.base / 'mounted-app-fixture')
        self.app = self.base / 'mounted-app-fixture/PeerBrush-0.2.0-macOS-arm64/PeerBrush.app'
        self.calls = []
        self.status = 'Accepted'
        self.wait_failure = False
        self.gatekeeper = True
        self.wrong_log = False
        self.issues = None
        self.image_team = TEAM
        self.mounted_failure = False
        self.staple_failure = False
        self.submit_failure = False
        self.submission = self.work / 'submission.dmg'
        self.signing.runner.stop()
        self.runner = mock.patch.object(sign_macos.subprocess, 'run', side_effect=self.fake_run)
        self.runner.start()
        self.addCleanup(self.runner.stop)
        self.verifier = mock.patch.object(package_macos_dmg, 'verify_image', side_effect=self.verify_mount)
        self.verify_mock = self.verifier.start()
        self.addCleanup(self.verifier.stop)

    def test_prereleases_fail_before_any_provider_or_preparation_calls(self):
        original = self.archive.read_bytes()
        with mock.patch.object(sign_macos, 'preflight') as provider, mock.patch.object(package_macos_dmg, 'prepare') as preparation:
            for version in ['0.30.0-dev.1', '0.30.0-beta.1', '0.30.0-rc.1', '0.30.0+build']:
                for resume in [False, True]:
                    with self.subTest(version=version, resume=resume), self.assertRaisesRegex(ValueError, 'stable A.B.C release'):
                        self.sign(version=version, resume=resume)
            provider.assert_not_called()
            preparation.assert_not_called()
        self.assertEqual(self.calls, [])
        self.assertEqual(self.archive.read_bytes(), original)
        self.assertFalse(self.work.exists())
        self.assertFalse(self.output.exists())

    def fake_run(self, command, **kwargs):
        self.calls.append(command)
        if command[0] == '/usr/bin/codesign' and command[-1].endswith('.dmg'):
            if '--sign' in command:
                with pathlib.Path(command[-1]).open('ab') as stream:
                    stream.write(b'SYNTHETIC_DMG_SIGNATURE')
            text = (f'Identifier={sign_macos.BUNDLE_ID}.diskimage\nTeamIdentifier={self.image_team}\n'
                    f'Authority=Developer ID Application: Synthetic Test ({self.image_team})\n'
                    'Timestamp=Oct 19, 2026 at 12:00:00\n')
            return subprocess.CompletedProcess(command, 0, stdout='', stderr=text)
        if command[0] == '/usr/bin/xcrun' and 'notarytool' in command:
            result = {}
            if 'submit' in command:
                if self.submit_failure:
                    raise subprocess.TimeoutExpired(command, 10)
                self.submission = pathlib.Path(command[3])
                result = {'id': SUBMISSION}
            elif 'wait' in command and self.wait_failure:
                raise subprocess.TimeoutExpired(command, 10)
            elif 'info' in command:
                result = {'id': SUBMISSION, 'status': self.status}
            elif 'log' in command:
                result = {'jobId': SUBMISSION, 'status': 'Accepted', 'issues': self.issues,
                          'sha256': '0' * 64 if self.wrong_log else digest(self.submission)}
            return subprocess.CompletedProcess(command, 0, stdout=json.dumps(result), stderr='')
        if command[0] == '/usr/bin/xcrun' and 'stapler' in command and command[-1].endswith('.dmg'):
            if 'staple' in command:
                if self.staple_failure:
                    raise subprocess.CalledProcessError(1, command)
                with pathlib.Path(command[-1]).open('ab') as stream:
                    stream.write(b'SYNTHETIC_DMG_TICKET')
            return subprocess.CompletedProcess(command, 0, stdout='', stderr='')
        if command[0] == '/usr/sbin/spctl' and command[-1].endswith('.dmg'):
            return subprocess.CompletedProcess(command, 0, stdout='', stderr='source=Notarized Developer ID\n' if self.gatekeeper else 'source=Developer ID\n')
        return self.signing.fake_run(command, **kwargs)

    def verify_mount(self, image, mount, expected, verify_app):
        self.assertIn('Applications', expected)
        self.assertTrue(image.read_bytes().endswith(b'SYNTHETIC_DMG_TICKET'))
        if self.mounted_failure:
            raise ValueError('Mounted DMG differs from the original payload')
        verify_app(self.app)

    def sign(self, **overrides):
        return sign_macos.sign_dmg(**(self.kwargs | overrides))

    def test_success_signs_and_notarizes_the_image_with_final_hash_and_original_app(self):
        original = self.archive.read_bytes()
        receipts = []
        final = self.sign(on_submission=receipts.append)
        self.assertEqual(self.archive.read_bytes(), original)
        receipt = json.loads(pathlib.Path(str(final) + '.verification.json').read_text())
        self.assertEqual(receipt['sha256'], digest(final))
        self.assertNotEqual(receipt['submission_image_sha256'], receipt['sha256'])
        self.assertEqual(receipt['notary_submission_id'], SUBMISSION)
        self.assertEqual(receipt['notary_status'], 'Accepted')
        self.assertEqual(receipt['image_signing'], 'developer-id-notarized')
        self.assertFalse(receipt['release_ready'])
        self.assertEqual(receipt['drag_to_applications_test'], 'not-performed')
        self.assertEqual(receipts[0]['submission_image_sha256'], digest(self.work / 'submission.dmg'))
        commands = [c for c in self.calls if c[0] == '/usr/bin/codesign' and '--sign' in c]
        self.assertEqual(len(commands), 1)
        self.assertTrue(commands[0][-1].endswith('submission.dmg'))
        self.assertIn('--timestamp', commands[0])
        self.assertNotIn('--options', commands[0], 'DMG signing must not pretend the image is executable hardened-runtime code')
        self.assertEqual(sum('submit' in c for c in self.calls), 1)
        self.assertTrue(any('--context' in c and 'context:primary-signature' in c for c in self.calls))
        self.verify_mock.assert_called_once()

    def test_timeout_and_failed_finalization_resume_without_resigning_or_resubmitting(self):
        self.wait_failure = True
        receipts = []
        with self.assertRaises(ValueError):
            self.sign(on_submission=receipts.append)
        self.assertEqual(receipts[0]['submission_id'], SUBMISSION)
        before = (self.work / 'submission.dmg').read_bytes()
        self.wait_failure = False
        self.gatekeeper = False
        with self.assertRaisesRegex(ValueError, 'Gatekeeper'):
            self.sign(resume=True)
        self.gatekeeper = True
        final = self.sign(resume=True)
        self.assertTrue(final.is_file())
        self.assertEqual(before, (self.work / 'submission.dmg').read_bytes())
        self.assertEqual(sum('--sign' in c for c in self.calls), 1)
        self.assertEqual(sum('submit' in c for c in self.calls), 1)

    def test_rejected_log_hash_issues_signature_and_payload_fail_closed(self):
        cases = [('status', 'Rejected'), ('status', 'In Progress'), ('wrong_log', True), ('issues', [{'severity': 'warning'}]),
                 ('image_team', 'B1B2C3D4E5'), ('mounted_failure', True), ('staple_failure', True)]
        for index, (field, value) in enumerate(cases):
            with self.subTest(field=field):
                original = getattr(self, field)
                setattr(self, field, value)
                with self.assertRaises(ValueError):
                    self.sign(work=self.base / f'failure-{index}', output=self.base / f'failed-output-{index}')
                self.assertFalse(list((self.base / f'failed-output-{index}').glob('*.dmg')))
                setattr(self, field, original)

    def test_resume_changed_submission_and_preparation_refuse_new_submission(self):
        self.wait_failure = True
        with self.assertRaises(ValueError):
            self.sign()
        submission = self.work / 'submission.dmg'
        before = submission.read_bytes()
        submission.write_bytes(before + b'changed')
        with self.assertRaisesRegex(ValueError, 'changed after submission'):
            self.sign(resume=True)
        submission.write_bytes(before)
        state_path = self.work / 'state.json'
        state = json.loads(state_path.read_text())
        original = dict(state['payload'])
        state['payload'] = {}
        state_path.write_text(json.dumps(state))
        with self.assertRaisesRegex(ValueError, 'payload journal differs'):
            self.sign(resume=True)
        state['payload'] = original
        state_path.write_text(json.dumps(state))
        report = next((self.work / 'prepared').glob('*.verification.json'))
        report.write_bytes(report.read_bytes() + b'changed')
        with self.assertRaisesRegex(ValueError, 'changed after submission'):
            self.sign(resume=True)
        self.assertEqual(sum('submit' in c for c in self.calls), 1)

    def test_ambiguous_submission_never_reuploads_automatically(self):
        self.submit_failure = True
        with self.assertRaises(ValueError):
            self.sign()
        state = json.loads((self.work / 'state.json').read_text())
        self.assertNotIn('submission_id', state)
        self.assertEqual(state['submission_sha256'], digest(self.work / 'submission.dmg'))
        with self.assertRaisesRegex(ValueError, 'fresh signing work'):
            self.sign()
        with self.assertRaisesRegex(ValueError, 'does not match'):
            self.sign(resume=True)
        self.assertEqual(sum('submit' in c for c in self.calls), 1)

    def test_explicit_keychain_is_used_by_image_signing_and_all_notary_calls(self):
        keychain = self.base / 'synthetic-signing.keychain-db'
        keychain.write_bytes(b'SYNTHETIC_KEYCHAIN_FIXTURE')
        self.sign(keychain=keychain)
        commands = [c for c in self.calls if ('notarytool' in c and '--find' not in c) or (c[0] == '/usr/bin/codesign' and '--sign' in c)]
        self.assertTrue(commands)
        for command in commands:
            self.assertIn('--keychain', command)
            self.assertEqual(command[command.index('--keychain') + 1], str(keychain))

    def test_completed_outputs_and_unrelated_source_team_are_never_replaced(self):
        final = self.sign()
        original = final.read_bytes()
        with self.assertRaisesRegex(ValueError, 'Refusing to replace'):
            self.sign(resume=True)
        self.assertEqual(final.read_bytes(), original)
        with self.assertRaisesRegex(ValueError, 'this Team ID'):
            self.sign(team_id='B1B2C3D4E5', output=self.base / 'other-output', work=self.base / 'other-work')
        self.assertEqual(sum('submit' in c for c in self.calls), 1)


if __name__ == '__main__':
    unittest.main()
