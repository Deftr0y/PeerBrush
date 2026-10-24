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
from test_production_readiness import assert_publication_phases
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
        assert_publication_phases(self, result)
        self.assertEqual(result['blockers'], ['protected-publication-approval-not-configured',
            'windows-installed-distribution-and-signing-not-verified',
            'macos-dmg-native-signing-and-installation-not-verified',
            'linux-installed-distribution-and-update-owner-not-verified',
            'published-assets-and-website-readback-not-configured',
            'published-version-immutability-not-reconciled', 'macos-native-job-provenance-unverified'])
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
        expected = self.handoff()
        for missing in ['dmg_sha256', 'macos_portable_sha256', 'dmg_qualification', 'dmg_qualification_sha256', 'macos_team_id']:
            with self.subTest(missing=missing), self.assertRaisesRegex(ValueError, 'Six-record'):
                self.assess(**{missing: None})
        (self.output / 'production-readiness.json').write_text('{"candidate_verified":true,"production_ready":true,"publication_phases":{"prepublication_blockers":[]}}')
        self.assertFalse(self.assess()['candidate_verified'])
        reassessed = self.assess()
        assert_publication_phases(self, reassessed)
        self.assertEqual(reassessed, expected, 'Caller success JSON cannot waive assessed native or publication gates')
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

    def unsigned_seed(self):
        directory = self.base / 'unsigned-with-deb'
        directory.mkdir()
        unsigned = inspect_archive(self.unsigned, self.commit, '0.2.0', self.root)['record']
        inventory = copy.deepcopy(self.before)
        inventory['assets'] = [unsigned if a.get('platform') == 'macOS' else a for a in inventory['assets']]
        for asset in inventory['assets']:
            path = self.unsigned if asset == unsigned else self.directory / asset['file']
            shutil.copyfile(path, directory / asset['file'])
        (directory / 'release-inventory.json').write_text(json.dumps(inventory))
        (directory / 'SHA256SUMS').write_text(''.join(f"{a['sha256']}  {a['file']}\n" for a in inventory['assets']))
        return directory, inventory

    def native_selection(self):
        inputs = self.base / 'selected-native-trust'
        inputs.mkdir()
        result = dict(cli_version='2.102.0', workflow_commit=self.commit, run_id='123', run_attempt='1',
                      work=self.base / 'native-verification', readback_job=True)
        for key, name, data in [('bundle', 'selected-bundle.jsonl', b'OFFLINE_BUNDLE_FIXTURE\n'),
                                ('trusted_root', 'reviewed-root.jsonl', b'OFFLINE_ROOT_FIXTURE\n'),
                                ('cli', 'reviewed-gh', b'OFFLINE_EXECUTABLE_FIXTURE_NOT_RUN')]:
            path = inputs / name
            path.write_bytes(data)
            result[key], result[key + '_sha256'] = path, digest(path)
        return result

    def fake_native_authentication(self, qualification, **inputs):
        # Only the cryptographic/provider boundary is mocked. The real aggregate
        # still binds all source/archive/report bytes and rechecks owned copies.
        self.assertIs(inputs['readback_job'], True)
        self.assertEqual(qualification, self.evidence.read_bytes())
        work = inputs['work']
        work.mkdir(mode=0o700)
        for key, name in [('bundle', 'bundle.jsonl'), ('trusted_root', 'trusted_root.jsonl'), ('cli', 'gh')]:
            shutil.copyfile(inputs[key], work / name)
        (work / 'dmg-qualification.json').write_bytes(qualification)
        return dict(native_producer_authentication='verified-pinned-workflow-oidc-attestation',
                    native_job_conclusion='service-reconciled-success', installed_acceptance='not-performed',
                    public_availability='unverified', candidate_verified=False, ready=False, production_ready=False)

    def selected_handoff(self, directory, trust, output=None, **changes):
        return readiness.handoff_dmg(directory, self.image, self.evidence, output or self.output,
                    self.commit, '0.2.0', **(self.inputs() | dict(signed_archive=self.signed, dmg_trust=trust) | changes))

    def test_explicit_signed_selection_preserves_nonmac_assets_and_types_public_qualification_without_readiness(self):
        directory, original = self.unsigned_seed()
        trust = self.native_selection()
        for parent in [directory, self.image.parent, self.evidence.parent, trust['bundle'].parent]:
            (parent / 'private-journal.json').write_text('PRIVATE_CANARY_NOT_A_PUBLIC_ASSET')
        with mock.patch('verify_macos_qualification.authenticate', side_effect=self.fake_native_authentication) as authenticate:
            result = self.selected_handoff(directory, trust)
        authenticate.assert_called_once()
        self.assertIs(authenticate.call_args.kwargs['readback_job'], True)
        final = json.loads((self.output / 'release-inventory.json').read_bytes())
        nonmac = lambda value: [a for a in value['assets'] if a.get('platform') != 'macOS']
        self.assertEqual(nonmac(final), nonmac(original))
        self.assertEqual([a for a in final['assets'] if a.get('platform') == 'macOS'], [self.macos, self.record])
        self.assertNotIn(self.unsigned.name, {a['file'] for a in final['assets']})
        for asset in original['assets']:
            self.assertEqual(digest(directory / asset['file']), asset['sha256'])
        self.assertEqual((self.output / (self.image.name + '.qualification.json')).read_bytes(), self.evidence.read_bytes())
        self.assertEqual((self.output / (self.image.name + '.sha256')).read_bytes(),
                         f'{digest(self.image)}  {self.image.name}\n'.encode())
        self.assertEqual({path.name for path in self.output.iterdir()},
                         {a['file'] for a in final['assets']} | {'release-inventory.json', 'SHA256SUMS',
                          'production-readiness.json', self.image.name + '.qualification.json', self.image.name + '.sha256'})
        self.assertFalse(result['production_ready'])
        self.assertFalse(result['candidate_verified'])
        assert_publication_phases(self, result)
        macos = result['installed_candidates']['macOS']
        self.assertEqual(macos['native_job_conclusion'], 'service-reconciled-success')
        self.assertEqual(macos['installed_acceptance'], 'not-established-by-aggregate')
        self.assertFalse(macos['ready'])
        self.assertEqual(macos['public_availability'], 'unverified')
        self.assertEqual(len(self.signer.calls), self.native_calls)
        for path in self.output.iterdir():
            if path.suffix == '.json':
                self.assertNotIn('PRIVATE_CANARY', path.read_text())
                self.assertNotIn(str(self.base), path.read_text())

    def test_explicit_signed_selection_requires_all_trust_inputs_and_actual_successful_native_result(self):
        directory, _ = self.unsigned_seed()
        trust = self.native_selection()
        invalid = [None, {}, dict(trust, readback_job=False), dict(trust, readback_job=1),
                   {key: value for key, value in trust.items() if key != 'bundle'}, dict(trust, receipt={'ready': True})]
        with mock.patch('verify_macos_qualification.authenticate') as authenticate:
            for selected in invalid:
                with self.subTest(selected=list(selected) if selected else selected), self.assertRaisesRegex(ValueError, 'complete native'):
                    self.selected_handoff(directory, selected)
                self.assertFalse(self.output.exists())
        authenticate.assert_not_called()
        nested = dict(trust, work=self.output / 'private-verification')
        with mock.patch('verify_macos_qualification.authenticate') as authenticate, self.assertRaises(ValueError):
            self.selected_handoff(directory, nested)
        authenticate.assert_not_called()
        self.assertFalse(self.output.exists())
        for key, value in [('native_producer_authentication', 'caller-json-success'),
                           ('native_job_conclusion', 'not-service-reconciled')]:
            output = self.base / ('rejected-' + key)
            selected = dict(trust, work=self.base / ('work-' + key))
            def invalid_origin(qualification, **inputs):
                return dict(self.fake_native_authentication(qualification, **inputs), **{key: value})
            with mock.patch('verify_macos_qualification.authenticate', side_effect=invalid_origin), \
                    self.subTest(key=key), self.assertRaisesRegex(ValueError, 'did not authenticate'):
                self.selected_handoff(directory, selected, output)
            self.assertFalse((output / 'production-readiness.json').exists())
            self.assertFalse((output / (self.image.name + '.qualification.json')).exists())

    def test_explicit_signed_selection_keeps_all_digest_source_and_published_version_guards(self):
        directory, original = self.unsigned_seed()
        trust = self.native_selection()
        for changes in [dict(archive_sha256='0' * 64), dict(image_sha256='0' * 64),
                        dict(qualification_sha256='0' * 64), dict(team_id='Z9Y8X7W6V5'),
                        dict(previous=original), dict(previous=dict(original, version='999.0.0'))]:
            with self.subTest(changes=list(changes)), \
                    mock.patch('verify_macos_qualification.authenticate') as authenticate, self.assertRaises(ValueError):
                self.selected_handoff(directory, trust, **changes)
            authenticate.assert_not_called()
            self.assertFalse(self.output.exists())
        with self.assertRaisesRegex(ValueError, 'only an unsigned'):
            self.selected_handoff(self.directory, trust)
        self.assertFalse(self.output.exists())
        with mock.patch.object(readiness, 'inspect_archive', wraps=readiness.inspect_archive) as inspect:
            # The real selected archive inspection, not producer JSON, owns source agreement.
            inspected = inspect_archive(self.signed, self.commit, '0.2.0', self.root)
            bad = copy.deepcopy(inspected)
            bad['record']['source_sha256'] = '0' * 64
            def selected(path, *args, **kwargs):
                return bad if pathlib.Path(path) == self.signed else inspect._mock_wraps(path, *args, **kwargs)
            inspect.side_effect = selected
            with self.assertRaisesRegex(ValueError, 'exact candidate source'):
                self.selected_handoff(directory, trust)
        self.assertFalse(self.output.exists())

    def test_explicit_signed_selection_rechecks_original_and_owned_bytes_after_native_authentication(self):
        directory, original = self.unsigned_seed()
        trust = self.native_selection()
        windows = next(a for a in original['assets'] if a.get('platform') == 'Windows')
        source = next(a for a in original['assets'] if 'platform' not in a)
        cases = [('signed', self.signed), ('image', self.image), ('qualification', self.evidence),
                 ('windows', directory / windows['file']), ('source', directory / source['file']),
                 ('bundle', trust['bundle']), ('root', trust['trusted_root']), ('cli', trust['cli']),
                 ('copied-signed', None), ('copied-root', None), ('copied-inventory', None),
                 ('copied-checksums', None), ('unexpected-private', None)]
        for label, path in cases:
            output = self.base / ('changed-' + label)
            selected = dict(trust, work=self.base / ('verification-' + label))
            original_bytes = path.read_bytes() if path is not None else None
            def mutate(qualification, **inputs):
                result = self.fake_native_authentication(qualification, **inputs)
                selected_output = {'copied-signed': output / self.signed.name,
                                   'copied-root': inputs['work'] / 'trusted_root.jsonl',
                                   'copied-inventory': output / 'release-inventory.json',
                                   'copied-checksums': output / 'SHA256SUMS',
                                   'unexpected-private': output / 'private-unknown.json'}
                target = path if path is not None else selected_output[label]
                target.write_bytes(b'CHANGED_AFTER_AUTHENTICATION')
                return result
            try:
                with mock.patch('verify_macos_qualification.authenticate', side_effect=mutate), \
                        self.subTest(label=label), self.assertRaises(ValueError):
                    self.selected_handoff(directory, selected, output)
                self.assertFalse((output / 'production-readiness.json').exists())
            finally:
                if path is not None:
                    path.write_bytes(original_bytes)

    def test_cli_signed_selection_is_explicit_requires_native_readback_and_keeps_false_readiness_exit(self):
        trust = self.native_selection()
        argv = ['production_readiness.py', '--directory', str(self.directory), '--include-dmg', str(self.image),
                '--signed-macos-archive', str(self.signed), '--dmg-sha256', digest(self.image),
                '--macos-portable-sha256', self.macos['sha256'], '--dmg-qualification', str(self.evidence),
                '--dmg-qualification-sha256', digest(self.evidence), '--macos-team-id', test_sign_macos.TEAM,
                '--deb-sha256', digest(self.deb), '--portable-sha256', digest(self.debian.archive),
                '--maintainer', test_deb.MAINTAINER, '--source-root', str(self.root), '--commit', self.commit,
                '--version', '0.2.0', '--output', str(self.output), '--require-ready', '--native-job-readback']
        flags = {'bundle': 'native-attestation-bundle', 'bundle_sha256': 'native-attestation-sha256',
                 'trusted_root': 'native-trusted-root', 'trusted_root_sha256': 'native-trusted-root-sha256',
                 'cli': 'native-gh', 'cli_sha256': 'native-gh-sha256', 'cli_version': 'native-gh-version',
                 'workflow_commit': 'native-workflow-commit', 'run_id': 'native-run-id',
                 'run_attempt': 'native-run-attempt', 'work': 'native-verification-work'}
        for key, flag in flags.items():
            argv += ['--' + flag, str(trust[key])]
        output = io.StringIO()
        with mock.patch.object(sys, 'argv', argv), \
                mock.patch.object(readiness, 'handoff_dmg', return_value={'production_ready': False}) as handoff, \
                contextlib.redirect_stdout(output):
            self.assertEqual(readiness.main(), 2)
        self.assertIn('native qualification verified; installed acceptance and production publication remain blocked', output.getvalue())
        self.assertEqual(handoff.call_args.kwargs['signed_archive'], self.signed)
        self.assertIs(handoff.call_args.kwargs['dmg_trust']['readback_job'], True)
        for flag, count in [('--native-job-readback', 1), ('--native-gh', 2), ('--include-dmg', 2)]:
            at = argv.index(flag)
            invalid = argv[:at] + argv[at + count:]
            with mock.patch.object(sys, 'argv', invalid), mock.patch.object(readiness, 'handoff_dmg') as handoff, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                readiness.main()
            handoff.assert_not_called()

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
