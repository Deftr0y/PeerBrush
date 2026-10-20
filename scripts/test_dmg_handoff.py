"""Offline DMG handoff regressions: no native trust or public writes."""
import contextlib
import copy
import hashlib
import io
import json
import pathlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock

import prepare_site_release as website
import production_readiness as readiness
import test_deb
import test_final_macos_dmg
import test_sign_macos
from package import digest, git
from verify_release import inspect_archive, verify


class Handoff(unittest.TestCase):
    def setUp(self):
        self.debian = test_deb.DebianPackage(methodName='runTest')
        self.debian.setUp()
        self.addCleanup(self.debian.doCleanups)
        self.root = self.debian.root
        self.commit = git(self.root, 'rev-parse', 'HEAD').decode().strip()
        self.deb = self.debian.final_package()
        windows = self.debian.fixture.build('Windows')
        self.unsigned = self.debian.fixture.build('macOS')
        self.signer = test_sign_macos.Signing(methodName='test_success_final_hashes_source_preservation_and_native_validation')
        self.signer.setUp()
        self.addCleanup(self.signer.doCleanups)
        self.signer.archive = self.unsigned
        self.signer.kwargs.update(archive=self.unsigned, commit=self.commit,
                                  expected_sha256=digest(self.unsigned), source_root=self.root)
        self.signed = self.signer.sign()  # Existing fixture fakes every Apple command.
        external = tempfile.TemporaryDirectory(prefix='peerbrush-dmg-handoff-')
        self.addCleanup(external.cleanup)
        self.base = pathlib.Path(external.name).resolve()
        self.selected = self.base / 'selected'
        self.selected.mkdir()
        for archive in [windows, self.debian.archive, self.signed]:
            shutil.copyfile(archive, self.selected / archive.name)
        self.seed, self.directory, self.output = self.base / 'portable', self.base / 'with-deb', self.base / 'with-dmg'
        with contextlib.redirect_stdout(io.StringIO()):
            verify(self.selected, self.commit, '0.2.0', self.seed, self.root)
        readiness.handoff_deb(self.seed, self.deb, self.directory, self.commit, '0.2.0',
                             package_sha256=digest(self.deb), portable_sha256=digest(self.debian.archive),
                             maintainer=test_deb.MAINTAINER, source_root=self.root)
        self.before = json.loads((self.directory / 'release-inventory.json').read_bytes())
        self.macos = next(a for a in self.before['assets'] if a.get('platform') == 'macOS')
        native = self.base / 'native-inputs'
        native.mkdir()
        self.image = native / (self.signed.stem + '-dmg-signed-r2.dmg')
        self.image.write_bytes(b'SYNTHETIC_UNVERIFIED_BYTES_NOT_A_DMG')
        self.record = dict(file=self.image.name, bytes=self.image.stat().st_size, sha256=digest(self.image),
                           platform='macOS', architecture='arm64', binary_source_commit=self.commit,
                           source_sha256=self.macos['source_sha256'])
        # Matching self-described claims intentionally cannot establish native trust.
        self.claims = dict(schema=1, format='PeerBrushDmgQualificationV1', version='0.2.0', commit=self.commit,
                           record=self.record, signed_zip=dict(file=self.signed.name, sha256=digest(self.signed)),
                           executable_sha256=self.macos['executable_sha256'], publisher_team_id=test_sign_macos.TEAM,
                           candidate_verified=True, ready=False, production_ready=False, public_availability='unverified',
                           content_checks=['exact-source-signed-zip-and-corresponding-source', 'exact-mounted-files-modes-and-Applications-link'],
                           native_checks=['Developer-ID-publisher-and-secure-timestamps', 'app-hardened-runtime-and-empty-entitlements',
                                          'image-and-app-stapled-tickets-and-Gatekeeper', 'hdiutil-integrity-and-read-only-mount'],
                           limits={'drag-to-Applications-and-launch': 'not-performed', 'fresh-quarantined-download': 'not-performed',
                                   'upgrade-settings-and-unsaved-work': 'not-performed', 'public-availability': 'not-assessed'},
                           native_job_provenance='requires-trusted-native-job-at-consumption')
        evidence = self.base / 'native-qualification'
        evidence.mkdir()
        self.evidence = evidence / 'dmg-qualification.json'
        self.evidence.write_text(json.dumps(self.claims), encoding='utf-8')
        self.native_calls = len(self.signer.calls)

    def inputs(self):
        return dict(image_sha256=digest(self.image), archive_sha256=self.macos['sha256'],
                    qualification_sha256=digest(self.evidence), team_id=test_sign_macos.TEAM,
                    deb_sha256=digest(self.deb), linux_portable_sha256=digest(self.debian.archive),
                    maintainer=test_deb.MAINTAINER, source_root=self.root)

    def handoff(self, **changes):
        output = changes.pop('output', self.output)
        return readiness.handoff_dmg(self.directory, self.image, self.evidence, output, self.commit, '0.2.0', **(self.inputs() | changes))

    def assess(self, **changes):
        inputs = dict(deb_sha256=digest(self.deb), linux_portable_sha256=digest(self.debian.archive), maintainer=test_deb.MAINTAINER,
                      dmg_sha256=digest(self.image), macos_portable_sha256=self.macos['sha256'],
                      dmg_qualification=self.evidence.read_bytes(), dmg_qualification_sha256=digest(self.evidence),
                      macos_team_id=test_sign_macos.TEAM)
        return readiness.assess(self.output, self.commit, '0.2.0', source_root=self.root, **(inputs | changes))

    def test_exact_six_assets_preserve_seed_without_authenticating_native_claims(self):
        originals = {a['file']: (self.directory / a['file']).read_bytes() for a in self.before['assets']}
        for parent in [self.directory, self.image.parent, self.evidence.parent]:
            (parent / 'private-recovery.json').write_text('PRIVATE_CANARY_MUST_NOT_COPY')
        result = self.handoff()
        inventory = json.loads((self.output / 'release-inventory.json').read_bytes())
        self.assertEqual(inventory['assets'][:-1], self.before['assets'])
        self.assertEqual(inventory['assets'][-1], self.record)
        self.assertNotIn(self.unsigned.name, {a['file'] for a in inventory['assets']})
        for name, data in originals.items():
            self.assertEqual((self.output / name).read_bytes(), data)
            self.assertEqual((self.directory / name).read_bytes(), data)
        self.assertEqual({p.name for p in self.output.iterdir()}, {a['file'] for a in inventory['assets']} | {'release-inventory.json', 'SHA256SUMS', 'production-readiness.json'})
        self.assertEqual((self.output / 'SHA256SUMS').read_bytes(), ''.join(f"{a['sha256']}  {a['file']}\n" for a in inventory['assets']).encode())
        self.assertFalse(result['candidate_verified'])
        self.assertTrue(result['asset_binding_verified'])
        self.assertFalse(result['production_ready'])
        macos = result['installed_candidates']['macOS']
        self.assertTrue(macos['bytes_verified'])
        self.assertFalse(macos['candidate_verified'])
        self.assertEqual(macos['native_verification'], 'not-performed-by-aggregate')
        self.assertEqual(macos['native_producer_authentication'], 'unverified-requires-trusted-native-job')
        self.assertEqual(self.assess(), result)
        self.assertEqual(len(self.signer.calls), self.native_calls)
        self.assertNotIn('PRIVATE_CANARY', json.dumps(result))
        self.assertNotIn(str(self.base), json.dumps(result))

    def test_unsigned_or_duplicate_mac_inputs_cannot_be_selected_implicitly(self):
        shutil.copyfile(self.unsigned, self.selected / self.unsigned.name)
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(ValueError, 'Duplicate'):
            verify(self.selected, self.commit, '0.2.0', self.base / 'ambiguous', self.root)
        self.assertFalse((self.base / 'ambiguous').exists())
        unsigned = inspect_archive(self.unsigned, self.commit, '0.2.0', self.root)['record']
        inventory = copy.deepcopy(self.before)
        inventory['assets'] = [unsigned if a.get('platform') == 'macOS' else a for a in inventory['assets']]
        shutil.copyfile(self.unsigned, self.directory / self.unsigned.name)
        (self.directory / 'release-inventory.json').write_text(json.dumps(inventory))
        (self.directory / 'SHA256SUMS').write_text(''.join(f"{a['sha256']}  {a['file']}\n" for a in inventory['assets']))
        with self.assertRaisesRegex(ValueError, 'immutable Mac'):
            self.handoff()
        with self.assertRaisesRegex(ValueError, 'signed ZIP'):
            self.handoff(archive_sha256=unsigned['sha256'])
        self.assertFalse(self.output.exists())

    def test_explicit_hashes_team_source_and_qualification_schema_are_bound(self):
        for changes in [dict(image_sha256='0' * 64), dict(archive_sha256='0' * 64), dict(qualification_sha256='0' * 64),
                        dict(team_id='Z9Y8X7W6V5'), dict(deb_sha256='0' * 64), dict(linux_portable_sha256='0' * 64)]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.handoff(**changes)
            self.assertFalse(self.output.exists())
        variants = [dict(self.claims, commit='0' * 40), dict(self.claims, ready=True), dict(self.claims, schema=True),
                    dict(self.claims, candidate_verified=1), dict(self.claims, production_ready=True),
                    dict(self.claims, native_job_provenance='authenticated'), dict(self.claims, private_note='PRIVATE_CANARY'),
                    dict(self.claims, limits={}), dict(self.claims, record=dict(self.record, source_sha256='a' * 64))]
        for changed in variants:
            self.evidence.write_text(json.dumps(changed))
            with self.subTest(fields=list(changed)), self.assertRaisesRegex(ValueError, 'qualification differs'):
                self.handoff()
            self.assertFalse(self.output.exists())
        original = json.dumps(self.claims).encode()
        for raw in [original.replace(b'"schema": 1', b'"schema": 1, "schema": 1'), b'{', b'\x80', b'x' * (readiness.MAX_DMG_QUALIFICATION_BYTES + 1)]:
            self.evidence.write_bytes(raw)
            with self.subTest(length=len(raw)), self.assertRaises(ValueError):
                self.handoff()
            self.assertFalse(self.output.exists())

    def test_reinspection_needs_external_pins_and_ignores_local_readiness_claims(self):
        self.handoff()
        for missing in ['dmg_sha256', 'macos_portable_sha256', 'dmg_qualification', 'dmg_qualification_sha256', 'macos_team_id']:
            with self.subTest(missing=missing), self.assertRaisesRegex(ValueError, 'Six-record'):
                self.assess(**{missing: None})
        (self.output / 'production-readiness.json').write_text('{"candidate_verified":true,"production_ready":true}')
        self.assertFalse(self.assess()['candidate_verified'])
        self.assertFalse(self.assess()['production_ready'])
        path = self.output / 'release-inventory.json'
        inventory = json.loads(path.read_bytes())
        inventory['assets'][-1]['private_note'] = 'PRIVATE_CANARY'
        path.write_text(json.dumps(inventory))
        with self.assertRaises(ValueError):
            self.assess()

    def test_published_same_version_selections_and_existing_outputs_are_immutable(self):
        self.handoff()
        final = json.loads((self.output / 'release-inventory.json').read_bytes())
        self.assertEqual(self.handoff(output=self.base / 'same-final', previous=final)['immutable_version'], 'checked-against-supplied-inventory')
        published_portable = json.loads((self.seed / 'release-inventory.json').read_bytes())
        for previous in [published_portable, self.before]:
            output = self.base / ('published-' + str(len(previous['assets'])))
            with self.subTest(published_records=len(previous['assets'])), self.assertRaisesRegex(ValueError, 'published version cannot gain'):
                self.handoff(output=output, previous=previous)
            self.assertFalse(output.exists())
            with self.subTest(reinspection_records=len(previous['assets'])), self.assertRaisesRegex(ValueError, 'published version cannot gain'):
                self.assess(previous=previous)
        for change in ['unsigned', 'dmg', 'source']:
            previous = copy.deepcopy(final)
            if change == 'unsigned':
                previous['assets'].append(inspect_archive(self.unsigned, self.commit, '0.2.0', self.root)['record'])
            elif change == 'dmg':
                previous['assets'][-1]['sha256'] = 'a' * 64
            else:
                previous['commit'] = 'a' * 40
            output = self.base / ('immutable-' + change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.handoff(output=output, previous=previous)
            self.assertFalse(output.exists())
        original = {p.name: p.read_bytes() for p in self.output.iterdir()}
        for output in [self.output, self.directory, self.directory / 'nested', self.root / 'generated',
                       self.image.parent / 'generated', self.evidence.parent / 'generated']:
            with self.subTest(output=output.name), self.assertRaises(ValueError):
                self.handoff(output=output)
        self.assertEqual({p.name: p.read_bytes() for p in self.output.iterdir()}, original)

    def test_changed_input_retains_partial_output_without_success_report(self):
        real = readiness.bounded_file
        changed = False
        def copy_then_change(source, limit, destination=None):
            nonlocal changed
            result = real(source, limit, destination)
            if source == self.image and destination is not None:
                changed = True
                self.image.write_bytes(b'CHANGED_AFTER_COPY')
            return result
        with mock.patch.object(readiness, 'bounded_file', side_effect=copy_then_change), self.assertRaisesRegex(ValueError, 'changed after DMG handoff'):
            self.handoff()
        self.assertTrue(changed)
        self.assertTrue(self.output.is_dir())
        self.assertFalse((self.output / 'production-readiness.json').exists())
        before = {p.name: p.read_bytes() for p in self.output.iterdir()}
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.handoff()
        self.assertEqual({p.name: p.read_bytes() for p in self.output.iterdir()}, before)

    def test_website_six_record_contract_does_not_promote_installed_eligibility(self):
        self.handoff()
        inventory = json.loads((self.output / 'release-inventory.json').read_bytes())
        self.assertEqual(len(website.validate_inventory(inventory, self.commit, '0.2.0')), 6)
        with self.assertRaises(ValueError):
            website.installed_report('dmg', self.record, self.assess(), self.commit, '0.2.0')

    def test_cli_requires_all_pins_and_retains_false_readiness_exit(self):
        argv = ['production_readiness.py', '--directory', str(self.directory), '--include-dmg', str(self.image),
                '--dmg-sha256', digest(self.image), '--macos-portable-sha256', self.macos['sha256'],
                '--dmg-qualification', str(self.evidence), '--dmg-qualification-sha256', digest(self.evidence),
                '--macos-team-id', test_sign_macos.TEAM, '--deb-sha256', digest(self.deb),
                '--portable-sha256', digest(self.debian.archive), '--maintainer', test_deb.MAINTAINER,
                '--source-root', str(self.root), '--commit', self.commit, '--version', '0.2.0',
                '--output', str(self.output), '--require-ready']
        with mock.patch.object(sys, 'argv', argv), mock.patch.object(readiness, 'handoff_dmg', return_value={'production_ready': False}) as handoff, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(readiness.main(), 2)
        self.assertEqual(handoff.call_args.kwargs['archive_sha256'], self.macos['sha256'])
        for invalid in [argv + ['--portable', str(self.signed)], argv + ['--include-deb', str(self.deb)],
                        argv[:argv.index('--dmg-qualification-sha256')] + argv[argv.index('--dmg-qualification-sha256') + 2:]]:
            with mock.patch.object(sys, 'argv', invalid), mock.patch.object(readiness, 'handoff_dmg') as handoff, contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                readiness.main()
            handoff.assert_not_called()
        error = io.StringIO()
        with mock.patch.object(sys, 'argv', argv), mock.patch.object(readiness, 'handoff_dmg', side_effect=ValueError('PRIVATE_CANARY')), contextlib.redirect_stderr(error), self.assertRaises(SystemExit):
            readiness.main()
        self.assertNotIn('PRIVATE_CANARY', error.getvalue())
        self.assertFalse(self.output.exists())
        selected = argv.index('--include-dmg')
        inspect_argv = argv[:selected] + argv[selected + 2:]
        error = io.StringIO()
        with mock.patch.object(sys, 'argv', inspect_argv), mock.patch.object(readiness, 'assess', side_effect=ValueError('PRIVATE_CANARY')), contextlib.redirect_stderr(error), self.assertRaises(SystemExit):
            readiness.main()
        self.assertNotIn('PRIVATE_CANARY', error.getvalue())
        self.assertIn('inspection failed', error.getvalue())
        self.assertFalse(self.output.exists())


class NativeProducerContract(unittest.TestCase):
    def test_real_consumer_schema_binds_without_authenticating_its_job(self):
        native = test_final_macos_dmg.FinalImage(methodName='runTest')
        native.setUp()
        self.addCleanup(native.doCleanups)
        claims = native.inspect()  # Every Apple command remains mocked.
        data = json.dumps(claims).encode()
        result = readiness.bind_dmg(native.signer.fixture.root, native.archive, native.image,
                                    commit=native.signer.commit, version='0.2.0', archive_sha256=digest(native.archive),
                                    image_sha256=digest(native.image), qualification=data,
                                    qualification_sha256=hashlib.sha256(data).hexdigest(), team_id=test_sign_macos.TEAM)
        self.assertEqual(result['qualification_binding'], 'exact-schema-and-independent-digest-matched')
        self.assertFalse(result['candidate_verified'])
        self.assertEqual(result['native_producer_authentication'], 'unverified-requires-trusted-native-job')


if __name__ == '__main__':
    unittest.main()
