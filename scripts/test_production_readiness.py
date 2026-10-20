"""Production gates inspect real synthetic archives, without signing or publishing."""
import copy
import contextlib
import io
import json
from pathlib import Path
import sys
import unittest
from unittest import mock

import test_package
import test_deb
from package import digest, git
import production_readiness as readiness
from verify_release import verify


class Tests(unittest.TestCase):
    def setUp(self):
        self.fixture = test_package.Packages(methodName='runTest')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.commit = git(self.fixture.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in ['Windows', 'macOS', 'Linux']:
            self.fixture.build(platform)
        self.directory = self.fixture.root / 'verified'
        with contextlib.redirect_stdout(io.StringIO()):
            verify(self.fixture.output, self.commit, '0.2.0', self.directory, self.fixture.root)

    def assess(self, previous=None):
        return readiness.assess(self.directory, self.commit, '0.2.0', previous, self.fixture.root)

    def test_exact_source_candidates_never_claim_installed_or_signed_readiness(self):
        report = self.assess()
        self.assertTrue(report['candidate_verified'])
        self.assertFalse(report['production_ready'])
        self.assertEqual(set(report['platforms']), {'Windows', 'macOS', 'Linux'})
        for platform in report['platforms'].values():
            self.assertTrue(platform['candidate_verified'])
            self.assertFalse(platform['ready'])
            self.assertEqual(platform['public_availability'], 'unverified')

    def test_changed_missing_or_relabelled_bytes_fail(self):
        inventory_path = self.directory / 'release-inventory.json'
        original = inventory_path.read_bytes()
        inventory = json.loads(original)
        for field, value in [('commit', 'b' * 40), ('binary_source_commit', 'b' * 40), ('packaging_correction', '-repacked-20261019')]:
            inventory_path.write_text(json.dumps(dict(inventory, **{field: value})))
            with self.assertRaises(ValueError):
                self.assess()
        inventory_path.write_bytes(original)
        asset = self.directory / inventory['assets'][0]['file']
        asset.write_bytes(asset.read_bytes() + b'changed')
        with self.assertRaises(ValueError):
            self.assess()

    def test_checksums_and_immutable_release_version_are_enforced(self):
        inventory = json.loads((self.directory / 'release-inventory.json').read_text())
        self.assertEqual(self.assess(inventory)['immutable_version'], 'checked-against-supplied-inventory')
        for change in ['commit', 'hash', 'missing']:
            previous = copy.deepcopy(inventory)
            if change == 'commit':
                previous['commit'] = 'b' * 40
            elif change == 'hash':
                previous['assets'][0]['sha256'] = 'b' * 64
            else:
                previous['assets'].append(dict(file='old-release-file.zip', sha256='b' * 64))
            with self.assertRaises(ValueError):
                self.assess(previous)
        (self.directory / 'SHA256SUMS').write_text('changed')
        with self.assertRaises(ValueError):
            self.assess()

    def test_stable_inventory_cannot_be_downgraded_to_its_prerelease(self):
        previous = json.loads((self.directory / 'release-inventory.json').read_text())
        candidate = dict(previous, version='0.2.0-dev.1')
        with self.assertRaisesRegex(ValueError, 'downgrade'):
            readiness.immutable(previous, candidate)
        for bad in ['00.2.0', '0.02.0', '0.2.00']:
            with self.subTest(version=bad), self.assertRaises(ValueError):
                readiness.immutable(dict(previous, version=bad), candidate)


class DebianQualification(unittest.TestCase):
    def setUp(self):
        self.fixture = test_deb.DebianPackage(methodName='runTest')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.package = self.fixture.final_package()
        self.commit = git(self.fixture.root, 'rev-parse', 'HEAD').decode().strip()
        stage = self.fixture.root / 'final-deb-stage'
        self.receipt = dict(schema=1, format='PeerBrushDebInstallationV1', product='PeerBrush', version='0.2.0', commit=self.commit,
                            package=dict(file=self.package.name, sha256=digest(self.package)),
                            executable_sha256=digest(stage / 'usr/bin/peerbrush'),
                            source_sha256=digest(stage / 'usr/share/doc/peerbrush/PeerBrush-0.2.0-source.zip'),
                            target='Ubuntu 24.04 amd64', architecture='amd64',
                            checks=dict(readiness.DEB_INSTALLATION_CHECKS), limits=dict(readiness.DEB_INSTALLATION_LIMITS),
                            release_ready=False, production_readiness='not-assessed')

    def assess(self, receipt=None, **overrides):
        inputs = dict(commit=self.commit, version='0.2.0', portable_sha256=digest(self.fixture.archive),
                      package_sha256=digest(self.package), maintainer=test_deb.MAINTAINER,
                      installation_receipt=json.dumps(self.receipt if receipt is None else receipt).encode())
        return readiness.assess_deb(self.fixture.root, self.fixture.archive, self.package, **dict(inputs, **overrides))

    def test_private_matching_evidence_qualifies_content_without_granting_production_or_public_availability(self):
        report = self.assess()
        self.assertTrue(report['candidate_verified'])
        self.assertEqual(report['installation_receipt_binding'], 'verified')
        self.assertEqual(report['installation_receipt_provenance'], 'requires-trusted-same-job-producer')
        self.assertEqual(report['record']['sha256'], self.receipt['package']['sha256'])
        self.assertEqual(report['record']['source_sha256'], self.receipt['source_sha256'])
        self.assertEqual(report['installation_checks'], readiness.DEB_INSTALLATION_CHECKS)
        self.assertFalse(report['ready'])
        self.assertFalse(report['production_ready'])
        self.assertEqual(report['public_availability'], 'unverified')
        self.assertEqual(report['native_dependencies'], 'syntax-and-required-runtime-only')
        self.assertEqual(report['limits'], readiness.DEB_INSTALLATION_LIMITS)
        self.assertNotIn(str(self.fixture.root), json.dumps(report))
        self.assertNotIn('installation.json', json.dumps(report))
        self.assertNotIn('receipt', report, 'Raw private evidence must not become a public report field')

    def test_stale_incomplete_forged_or_overclaiming_receipts_are_rejected(self):
        variants = []
        for field, value in [('version', '0.26.0'), ('commit', 'a' * 40), ('schema', True), ('release_ready', 0),
                             ('release_ready', True), ('production_readiness', 'ready'), ('architecture', 'arm64'),
                             ('executable_sha256', 'a' * 64), ('source_sha256', 'a' * 64),
                             ('target', 'another baseline'), ('format', 'PeerBrushDebInstallationV2')]:
            variants.append(dict(self.receipt, **{field: value}))
        variants += [dict(self.receipt, package=dict(self.receipt['package'], sha256='a' * 64)),
                     dict(self.receipt, package=dict(self.receipt['package'], file='other.deb')),
                     dict(self.receipt, checks=dict(self.receipt['checks'], installation='not-performed')),
                     dict(self.receipt, checks={'installation': 'passed'}),
                     dict(self.receipt, limits=dict(self.receipt['limits'], **{'native-gui': 'passed'})),
                     dict(self.receipt, limits={}), dict(self.receipt, private_note='SYNTHETIC_PRIVATE_VALUE')]
        for index, receipt in enumerate(variants):
            with self.subTest(mutation=index), self.assertRaisesRegex(ValueError, 'selected package, source, checks or explicit limits') as caught:
                self.assess(receipt)
            self.assertNotIn('SYNTHETIC_PRIVATE_VALUE', str(caught.exception))

    def test_receipt_json_is_bounded_unambiguous_and_cannot_substitute_for_real_package_inspection(self):
        original = json.dumps(self.receipt).encode()
        for data in [b'', b'not-json', b'\xff', b'[]', b'null', b' ' * (readiness.MAX_INSTALLATION_RECEIPT_BYTES + 1),
                     original.replace(b'"schema": 1', b'"schema": 1, "schema": 1'),
                     original.replace(b'"installation": "passed"', b'"installation": "passed", "installation": "passed"')]:
            with self.subTest(json=data[:30]), self.assertRaises(ValueError):
                self.assess(installation_receipt=data)
        self.package.write_bytes(self.package.read_bytes() + b'changed')
        receipt = copy.deepcopy(self.receipt)
        receipt['package']['sha256'] = digest(self.package)
        with self.assertRaisesRegex(ValueError, 'trailing ar'):
            self.assess(receipt)

    def test_cli_selects_deb_mode_requires_explicit_inputs_and_keeps_only_public_report_immutable(self):
        evidence = self.fixture.root / 'installation.json'
        raw = json.dumps(self.receipt).encode()
        evidence.write_bytes(raw)
        output = self.fixture.root / (self.package.name + '.qualification.json')
        argv = ['production_readiness.py', '--deb', str(self.package), '--deb-sha256', digest(self.package),
                '--portable', str(self.fixture.archive), '--portable-sha256', digest(self.fixture.archive),
                '--maintainer', test_deb.MAINTAINER, '--installation-receipt', str(evidence),
                '--commit', self.commit, '--version', '0.2.0', '--output', str(output), '--require-ready']
        report = self.assess()
        with mock.patch.object(sys, 'argv', argv), mock.patch.object(readiness, 'assess_deb', return_value=report) as inspect, \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(readiness.main(), 2)
            inspect.assert_called_once()
            self.assertEqual(inspect.call_args.kwargs['installation_receipt'], raw)
            self.assertEqual(inspect.call_args.kwargs['maintainer'], test_deb.MAINTAINER)
            before = output.read_bytes()
            self.assertEqual(readiness.main(), 2)
            self.assertEqual(output.read_bytes(), before)
            inspect.return_value = dict(report, candidate_verified=False)
            with self.assertRaisesRegex(ValueError, 'replace an existing'):
                readiness.main()
        self.assertEqual(evidence.read_bytes(), raw)
        self.assertEqual(output.read_bytes(), before)
        for invalid in [[value for value in argv if value != '--require-ready'] + ['--directory', str(self.fixture.root)],
                        argv[:argv.index('--maintainer')] + argv[argv.index('--maintainer') + 2:],
                        argv + ['--previous-inventory', str(evidence)]]:
            with self.subTest(argv=invalid), mock.patch.object(sys, 'argv', invalid), \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
                readiness.main()
            self.assertEqual(caught.exception.code, 2)


class DebianHandoff(unittest.TestCase):
    def setUp(self):
        self.fixture = test_deb.DebianPackage(methodName='runTest')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.commit = git(self.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in ['Windows', 'macOS']:
            self.fixture.fixture.build(platform)
        self.package = self.fixture.final_package()
        self.package_sha256 = digest(self.package)
        self.portable_sha256 = digest(self.fixture.archive)
        self.directory = self.root / 'verified'
        with contextlib.redirect_stdout(io.StringIO()):
            verify(self.fixture.fixture.output, self.commit, '0.2.0', self.directory, self.root)
        self.before = json.loads((self.directory / 'release-inventory.json').read_bytes())
        self.output = self.root / 'with-deb'

    def handoff(self, **overrides):
        inputs = dict(package_sha256=self.package_sha256, portable_sha256=self.portable_sha256,
                      maintainer=test_deb.MAINTAINER, source_root=self.root)
        output = overrides.pop('output', self.output)
        return readiness.handoff_deb(self.directory, self.package, output, self.commit, '0.2.0', **dict(inputs, **overrides))

    def assess(self, **overrides):
        inputs = dict(deb_sha256=self.package_sha256, linux_portable_sha256=self.portable_sha256,
                      maintainer=test_deb.MAINTAINER)
        return readiness.assess(self.output, self.commit, '0.2.0', source_root=self.root, **dict(inputs, **overrides))

    def test_real_deb_handoff_preserves_records_bytes_and_does_not_copy_or_trust_private_reports(self):
        portable_report = readiness.assess(self.directory, self.commit, '0.2.0', source_root=self.root)
        originals = {asset['file']: (self.directory / asset['file']).read_bytes() for asset in self.before['assets']}
        original_inventory = (self.directory / 'release-inventory.json').read_bytes()
        for parent in [self.directory, self.package.parent]:
            (parent / 'installation.json').write_text('SYNTHETIC_PRIVATE_RECEIPT_NEVER_COPY')
            (parent / (self.package.name + '.qualification.json')).write_text('{"production_ready":true,"installation_checks":{"installation":"passed"}}')
        result = self.handoff()
        inventory = json.loads((self.output / 'release-inventory.json').read_bytes())
        self.assertEqual(inventory['assets'][:-1], self.before['assets'])
        self.assertEqual(inventory['assets'][-1]['file'], self.package.name)
        self.assertEqual(inventory['assets'][-1]['sha256'], self.package_sha256)
        self.assertEqual(inventory['assets'][-1]['source_sha256'], next(a['sha256'] for a in self.before['assets'] if 'platform' not in a))
        self.assertEqual((self.output / 'SHA256SUMS').read_text(), ''.join(f"{a['sha256']}  {a['file']}\n" for a in inventory['assets']))
        self.assertEqual({p.name for p in self.output.iterdir()}, {a['file'] for a in inventory['assets']} | {'release-inventory.json', 'SHA256SUMS', 'production-readiness.json'})
        for name, data in originals.items():
            self.assertEqual((self.directory / name).read_bytes(), data)
            self.assertEqual((self.output / name).read_bytes(), data)
        self.assertEqual((self.directory / 'release-inventory.json').read_bytes(), original_inventory)
        self.assertEqual((self.output / self.package.name).read_bytes(), self.package.read_bytes())
        self.assertEqual(result['platforms']['Linux'], portable_report['platforms']['Linux'])
        installed = result['installed_candidates']['Linux']
        self.assertTrue(installed['candidate_verified'])
        self.assertFalse(installed['ready'])
        self.assertEqual(installed['installed_acceptance'], 'not-established-by-aggregate')
        self.assertEqual(installed['producer_provenance'], 'requires-successful-same-workflow-native-job')
        self.assertFalse(result['production_ready'])
        self.assertEqual(self.assess(), result)
        (self.output / 'production-readiness.json').write_text('{"production_ready":true}')
        self.assertFalse(self.assess()['production_ready'], 'Existing report JSON must never grant native authority')

    def test_selected_hash_control_and_corresponding_source_fail_before_output(self):
        for changes in [dict(package_sha256='a' * 64), dict(portable_sha256='a' * 64), dict(maintainer='Other <other@example.invalid>')]:
            with self.subTest(inputs=changes), self.assertRaises(ValueError):
                self.handoff(**changes)
            self.assertFalse(self.output.exists())
        payload = test_deb.tar_bytes(self.fixture.final_data)
        control = self.fixture.final_control
        test_deb.write_deb(self.package, payload, test_deb.tar_bytes([('.', 0o755, None), ('control', 0o644, control.replace('Architecture: amd64', 'Architecture: arm64').encode())]))
        with self.assertRaises(ValueError):
            self.handoff(package_sha256=digest(self.package))
        self.assertFalse(self.output.exists())
        changed = [(name, mode, b'wrong source' if name.endswith('-source.zip') else data) for name, mode, data in self.fixture.final_data]
        test_deb.write_deb(self.package, test_deb.tar_bytes(changed), test_deb.tar_bytes([('.', 0o755, None), ('control', 0o644, control.encode())]))
        with self.assertRaises(ValueError):
            self.handoff(package_sha256=digest(self.package))
        self.assertFalse(self.output.exists())

    def test_private_inventory_fields_are_rejected_before_any_public_output(self):
        path = self.directory / 'release-inventory.json'
        original = path.read_bytes()
        for target in ['root', 'source']:
            changed = copy.deepcopy(self.before)
            destination = changed if target == 'root' else next(asset for asset in changed['assets'] if 'platform' not in asset)
            destination['private_note'] = 'SYNTHETIC_PRIVATE_METADATA_NEVER_COPY'
            path.write_text(json.dumps(changed))
            with self.subTest(target=target), self.assertRaises(ValueError) as caught:
                self.handoff()
            self.assertNotIn('SYNTHETIC_PRIVATE_METADATA_NEVER_COPY', str(caught.exception))
            self.assertFalse(self.output.exists())
        path.write_bytes(original)
        for raw in [original.replace(b'"schema": 1', b'"schema": 1, "schema": 1'),
                    original.replace(b'"bytes":', b'"bytes": 1, "bytes":', 1)]:
            path.write_bytes(raw)
            with self.subTest(duplicate=raw[:40]), self.assertRaisesRegex(ValueError, 'duplicate fields'):
                self.handoff()
            self.assertFalse(self.output.exists())
        path.write_bytes(original)
        poisoned = json.dumps(dict(self.before, private_note='SYNTHETIC_PRIVATE_METADATA_NEVER_COPY')).encode()
        real_open = Path.open
        reads = 0
        def alternating_inventory(candidate, mode='r', *args, **kwargs):
            nonlocal reads
            if candidate == path and mode == 'rb':
                reads += 1
                return io.BytesIO(poisoned if reads % 2 else original)
            return real_open(candidate, mode, *args, **kwargs)
        with mock.patch.object(Path, 'open', autospec=True, side_effect=alternating_inventory), self.assertRaises(ValueError):
            self.handoff()
        self.assertFalse(self.output.exists(), 'Captured bytes themselves must be validated before any output')

    def test_missing_or_foreign_source_row_rejects_before_opening_unlisted_source_bytes(self):
        path = self.directory / 'release-inventory.json'
        source = next(asset for asset in self.before['assets'] if 'platform' not in asset)
        source_path = self.directory / source['file']
        without = copy.deepcopy(self.before)
        without['assets'] = [asset for asset in without['assets'] if asset['file'] != source['file']]
        foreign = copy.deepcopy(self.before)
        next(asset for asset in foreign['assets'] if asset['file'] == source['file'])['file'] = 'foreign-source.zip'
        real_open = Path.open
        def reject_unlisted_source(candidate, mode='r', *args, **kwargs):
            if candidate == source_path:
                self.fail('Unlisted source bytes were opened before the exact source row was established')
            return real_open(candidate, mode, *args, **kwargs)
        for changed in [without, foreign]:
            path.write_text(json.dumps(changed))
            with self.subTest(assets=len(changed['assets'])), \
                    mock.patch.object(Path, 'open', autospec=True, side_effect=reject_unlisted_source), self.assertRaises(ValueError):
                self.handoff()
            self.assertFalse(self.output.exists())

    def test_five_records_require_explicit_inputs_exact_linux_record_and_no_claim_fields(self):
        self.handoff()
        for missing in ['deb_sha256', 'linux_portable_sha256', 'maintainer']:
            with self.subTest(missing=missing), self.assertRaisesRegex(ValueError, 'explicit DEB/portable'):
                self.assess(**{missing: None})
        with self.assertRaisesRegex(ValueError, 'Linux portable inventory'):
            self.assess(linux_portable_sha256='a' * 64)
        path = self.output / 'release-inventory.json'
        inventory = json.loads(path.read_bytes())
        variants = [dict(inventory, production_ready=True)]
        for field, value in [('architecture', 'x64'), ('release_ready', True), ('source_sha256', 'a' * 64)]:
            changed = copy.deepcopy(inventory)
            changed['assets'][-1][field] = value
            variants.append(changed)
        wrong_linux = copy.deepcopy(inventory)
        next(a for a in wrong_linux['assets'] if a.get('platform') == 'Linux' and a['file'].endswith('.zip'))['executable_sha256'] = 'a' * 64
        variants.append(wrong_linux)
        duplicate = copy.deepcopy(inventory)
        duplicate['assets'][-1]['file'] = duplicate['assets'][0]['file'].upper()
        variants.append(duplicate)
        for index, changed in enumerate(variants):
            with self.subTest(mutation=index):
                path.write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    self.assess()
        path.write_text(json.dumps(inventory))
        with self.assertRaisesRegex(ValueError, 'four-record candidates'):
            readiness.assess(self.directory, self.commit, '0.2.0', source_root=self.root,
                             deb_sha256=self.package_sha256, linux_portable_sha256=self.portable_sha256, maintainer=test_deb.MAINTAINER)

    def test_previous_final_inventory_is_compared_after_addition_and_outputs_are_never_replaced(self):
        self.handoff()
        previous = json.loads((self.output / 'release-inventory.json').read_bytes())
        result = self.handoff(output=self.root / 'with-previous', previous=previous)
        self.assertEqual(result['immutable_version'], 'checked-against-supplied-inventory')
        wrong = copy.deepcopy(previous)
        wrong['assets'][-1]['sha256'] = 'a' * 64
        refused = self.root / 'wrong-previous'
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.handoff(output=refused, previous=wrong)
        self.assertFalse(refused.exists())
        canary = self.root / 'canary'
        canary.write_bytes(b'keep')
        original = {p.name: p.read_bytes() for p in self.output.iterdir()}
        for target in [self.output, self.directory, self.directory / 'inside', self.directory / '../verified/inside', canary]:
            with self.subTest(target=target.name), self.assertRaisesRegex(ValueError, 'fresh, nonoverlapping'):
                self.handoff(output=target)
        self.assertEqual(canary.read_bytes(), b'keep')
        self.assertEqual({p.name: p.read_bytes() for p in self.output.iterdir()}, original)
        with mock.patch.object(Path, 'lstat') as metadata:
            metadata.return_value.st_mode = 0o040755
            metadata.return_value.st_file_attributes = 0x400
            with self.assertRaisesRegex(ValueError, 'links or reparse'):
                self.handoff(output=self.root / 'reparse')

    def test_input_change_preserves_partial_output_without_success_report_or_automatic_retry(self):
        linux = self.directory / self.fixture.archive.name
        real_open = Path.open
        changed = False
        def open(path, mode='r', *args, **kwargs):
            nonlocal changed
            stream = real_open(path, mode, *args, **kwargs)
            if path == linux and mode == 'rb' and self.output.exists() and not changed:
                changed = True
                with real_open(linux, 'ab') as mutate:
                    mutate.write(b'changed-during-copy')
            return stream
        with mock.patch.object(Path, 'open', autospec=True, side_effect=open), self.assertRaisesRegex(ValueError, 'grew during handoff'):
            self.handoff()
        self.assertTrue(changed)
        self.assertTrue(self.output.is_dir())
        self.assertFalse((self.output / 'production-readiness.json').exists())
        partial = {p.name: p.read_bytes() for p in self.output.iterdir()}
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.handoff()
        self.assertEqual({p.name: p.read_bytes() for p in self.output.iterdir()}, partial)

    def test_cli_handoff_selection_is_explicit_and_mixed_modes_fail_without_writes(self):
        argv = ['production_readiness.py', '--directory', str(self.directory), '--include-deb', str(self.package),
                '--deb-sha256', self.package_sha256, '--portable-sha256', self.portable_sha256,
                '--maintainer', test_deb.MAINTAINER, '--commit', self.commit, '--version', '0.2.0',
                '--output', str(self.output), '--require-ready']
        with mock.patch.object(sys, 'argv', argv), mock.patch.object(readiness, 'handoff_deb', return_value={'production_ready': False}) as handoff, \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(readiness.main(), 2)
            handoff.assert_called_once()
            self.assertEqual(handoff.call_args.args[:3], (self.directory, self.package, self.output))
            self.assertEqual(handoff.call_args.kwargs['package_sha256'], self.package_sha256)
            self.assertEqual(handoff.call_args.kwargs['portable_sha256'], self.portable_sha256)
        invalid = [argv + ['--portable', str(self.fixture.archive)], argv + ['--installation-receipt', str(self.root / 'private.json')],
                   argv + ['--deb', str(self.package)],
                   argv[:argv.index('--maintainer')] + argv[argv.index('--maintainer') + 2:]]
        for arguments in invalid:
            with self.subTest(arguments=arguments), mock.patch.object(sys, 'argv', arguments), \
                    mock.patch.object(readiness, 'handoff_deb') as handoff, contextlib.redirect_stderr(io.StringIO()), \
                    self.assertRaises(SystemExit) as caught:
                readiness.main()
            self.assertEqual(caught.exception.code, 2)
            handoff.assert_not_called()
        self.assertFalse(self.output.exists())


if __name__ == '__main__':
    unittest.main()
