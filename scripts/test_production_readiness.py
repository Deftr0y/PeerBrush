"""Production gates inspect real synthetic archives, without signing or publishing."""
import copy
import contextlib
import io
import json
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


if __name__ == '__main__':
    unittest.main()
