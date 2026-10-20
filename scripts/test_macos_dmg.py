"""Synthetic portable DMG tests; Apple commands never run in this suite."""
import json
import pathlib
import plistlib
import subprocess
import tempfile
import unittest
from unittest import mock
import zipfile

import package_macos_dmg as dmg
import test_sign_macos
from package import digest


class Preparation(unittest.TestCase):
    def setUp(self):
        self.signing = test_sign_macos.Signing(methodName='test_success_final_hashes_source_preservation_and_native_validation')
        self.signing.setUp()
        self.addCleanup(self.signing.doCleanups)
        self.archive = self.signing.sign()
        self.output = self.signing.base / 'dmg-output'
        self.kwargs = dict(archive=self.archive, output=self.output, commit=self.signing.commit,
                           version='0.2.0', expected_sha256=digest(self.archive), revision='1',
                           source_root=self.signing.fixture.root)
        # Creating a directory symlink requires Windows privileges; only this
        # filesystem operation is mocked. The native macOS smoke checks it.
        self.link = mock.patch.object(pathlib.Path, 'symlink_to')
        self.link_mock = self.link.start()
        self.addCleanup(self.link.stop)
        self.images = mock.patch.object(dmg, 'create_image', side_effect=self.fake_image)
        self.image_mock = self.images.start()
        self.addCleanup(self.images.stop)

    def fake_image(self, payload, image, mount, volume, verify_app=None):
        self.assertEqual(volume, 'PeerBrush 0.2.0')
        self.assertTrue((payload / dmg.APP).is_dir())
        self.assertTrue((payload / dmg.SUPPORT / 'PeerBrush-0.2.0-source.zip').is_file())
        self.assertTrue((payload / dmg.SUPPORT / 'LICENSE').is_file())
        self.assertTrue((payload / dmg.SUPPORT / 'THIRD_PARTY_NOTICES.md').is_file())
        with zipfile.ZipFile(self.archive) as source:
            for entry in source.infolist():
                relative = pathlib.PurePosixPath(entry.filename).relative_to('PeerBrush-0.2.0-macOS-arm64')
                actual = payload / relative if relative.parts[0] == dmg.APP else payload / dmg.SUPPORT / relative
                self.assertEqual(actual.read_bytes(), source.read(entry))
        verify_app(payload / dmg.APP)
        image.write_bytes(b'SYNTHETIC_DMG_NOT_A_RELEASE')
        return {'Applications': {'link': '/Applications'}}

    def prepare(self, **overrides):
        return dmg.prepare(**(self.kwargs | overrides))

    def test_candidate_preserves_signed_input_source_and_gates_readiness(self):
        original = self.archive.read_bytes()
        candidate = self.prepare()
        self.assertEqual(original, self.archive.read_bytes())
        self.assertIn('-dmg-candidate-r1.dmg', candidate.name)
        self.assertEqual(candidate.with_suffix('.dmg.sha256').read_text(), f'{digest(candidate)}  {candidate.name}\n')
        receipt = json.loads(candidate.with_suffix('.dmg.verification.json').read_text())
        self.assertEqual(receipt['input_archive_sha256'], digest(self.archive))
        self.assertEqual(receipt['commit'], self.signing.commit)
        self.assertFalse(receipt['release_ready'])
        self.assertEqual(receipt['image_signing'], 'not-performed')
        self.assertEqual(receipt['image_notarization'], 'not-performed')
        self.assertEqual(receipt['drag_to_applications_test'], 'not-performed')
        self.link_mock.assert_called_once_with('/Applications', target_is_directory=True)
        self.assertFalse(list(self.output.glob('.peerbrush-dmg-*')))

    def test_rejects_unsigned_tampered_wrong_source_and_non_native_inputs(self):
        for overrides in [dict(archive=self.signing.archive, expected_sha256=digest(self.signing.archive)),
                          dict(expected_sha256='0' * 64), dict(commit='0' * 40), dict(revision='0')]:
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.prepare(**overrides)
        with mock.patch.object(dmg.sys, 'platform', 'linux'), self.assertRaisesRegex(ValueError, 'actual macOS'):
            self.prepare()
        self.image_mock.assert_not_called()
        self.assertFalse(self.output.exists())

    def test_linked_and_checkout_outputs_are_rejected_before_native_image(self):
        with self.assertRaisesRegex(ValueError, 'outside the source checkout'):
            self.prepare(output=self.signing.fixture.root.resolve() / 'output/dmg')
        with mock.patch.object(pathlib.Path, 'is_symlink', return_value=True), self.assertRaises(ValueError):
            self.prepare()
        self.image_mock.assert_not_called()

    def test_existing_candidate_or_receipt_is_never_replaced(self):
        candidate = self.prepare()
        for suffix in ['', '.sha256', '.verification.json']:
            target = pathlib.Path(str(candidate) + suffix)
            before = target.read_bytes()
            with self.assertRaisesRegex(ValueError, 'Refusing to replace'):
                self.prepare()
            self.assertEqual(target.read_bytes(), before)

    def test_native_failure_emits_no_final_candidate_and_preserves_work(self):
        self.image_mock.side_effect = ValueError('Native image check failed')
        with self.assertRaisesRegex(ValueError, 'Native image check failed'):
            self.prepare()
        self.assertFalse(list(self.output.glob('*.dmg*')))
        self.assertEqual(len(list(self.output.glob('.peerbrush-dmg-*'))), 1)

    def test_staging_mutation_is_rejected_before_image_creation(self):
        real_copy = dmg.shutil.copy2
        def changed_copy(source, destination, **kwargs):
            result = real_copy(source, destination, **kwargs)
            if pathlib.Path(source).name == 'LICENSE':
                pathlib.Path(destination).write_bytes(b'changed')
            return result
        with mock.patch.object(dmg.shutil, 'copy2', side_effect=changed_copy), self.assertRaisesRegex(ValueError, 'staging changed'):
            self.prepare()
        self.image_mock.assert_not_called()


class ImageRoundTrip(unittest.TestCase):
    def test_partial_attach_failure_detaches_and_detach_failure_is_not_hidden(self):
        for detach_failure in [False, True]:
            with self.subTest(detach_failure=detach_failure), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                payload, image, mount = root / 'payload', root / 'test.dmg', root / 'mount'
                payload.mkdir()
                mounted = False
                calls = []
                def run(command, **kwargs):
                    nonlocal mounted
                    calls.append(command[1])
                    if command[1] == 'attach':
                        mounted = True
                        raise ValueError('attach failed after mounting')
                    if command[1] == 'detach':
                        if detach_failure:
                            raise ValueError('detach failed')
                        mounted = False
                    return subprocess.CompletedProcess(command, 0, stdout='')
                with mock.patch.object(dmg.sys, 'platform', 'darwin'), mock.patch.object(dmg, 'run', side_effect=run), mock.patch.object(dmg, 'payload_inventory', return_value={}), mock.patch.object(pathlib.Path, 'is_mount', side_effect=lambda: mounted):
                    with self.assertRaisesRegex(ValueError, 'detach failed' if detach_failure else 'attach failed'):
                        dmg.create_image(payload, image, mount, 'Synthetic')
                self.assertEqual(calls, ['create', 'verify', 'attach', 'detach'])
                self.assertEqual(mounted, detach_failure)

    def test_native_commands_compare_mounted_payload_and_always_detach(self):
        for tampered, signature_failure in [(False, False), (True, False), (False, True)]:
            with self.subTest(tampered=tampered, signature_failure=signature_failure), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                payload, image, mount = root / 'payload', root / 'test.dmg', root / 'mount'
                payload.mkdir()
                mounted = False
                calls = []
                def run(command, **kwargs):
                    nonlocal mounted
                    calls.append(command)
                    if command[1] == 'create':
                        image.write_bytes(b'SYNTHETIC')
                    if command[1] == 'attach':
                        mounted = True
                        return subprocess.CompletedProcess(command, 0, stdout=plistlib.dumps({'system-entities': [{'mount-point': str(mount)}]}).decode())
                    if command[1] == 'detach':
                        mounted = False
                    return subprocess.CompletedProcess(command, 0, stdout='')
                def inventory(path):
                    return {'bytes': 'bad' if path == mount and tampered else 'good'}
                verify = mock.Mock(side_effect=ValueError('bad native signature') if signature_failure else None)
                with mock.patch.object(dmg.sys, 'platform', 'darwin'), mock.patch.object(dmg, 'run', side_effect=run), mock.patch.object(dmg, 'payload_inventory', side_effect=inventory), mock.patch.object(pathlib.Path, 'is_mount', side_effect=lambda: mounted):
                    if tampered or signature_failure:
                        with self.assertRaises(ValueError):
                            dmg.create_image(payload, image, mount, 'Synthetic', verify)
                    else:
                        dmg.create_image(payload, image, mount, 'Synthetic', verify)
                        verify.assert_called_once_with(mount / dmg.APP)
                self.assertFalse(mounted)
                self.assertEqual([command[1] for command in calls], ['create', 'verify', 'attach', 'detach'])
                self.assertIn('-readonly', calls[2])
                self.assertIn('-nobrowse', calls[2])

    def test_payload_rejects_missing_shortcut_and_additional_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / 'regular').write_bytes(b'original')
            with self.assertRaisesRegex(ValueError, 'Applications shortcut'):
                dmg.payload_inventory(root)
            try:
                (root / 'Applications').symlink_to('/Applications', target_is_directory=True)
            except OSError:
                self.skipTest('Directory symlinks need permissions on this platform; native macOS smoke covers the link')
            inventory = dmg.payload_inventory(root)
            self.assertEqual(inventory['regular']['sha256'], digest(root / 'regular'))
            (root / 'unexpected').symlink_to('/Applications', target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'Unexpected link'):
                dmg.payload_inventory(root)


if __name__ == '__main__':
    unittest.main()
