"""Real synthetic MSIX content checks; no SDK, network, authentication or installation."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from package import digest
import qualify_store_msix as preflight
from test_msix import synthetic_package
import test_submit_store_release as submission_tests


class StorePreflight(unittest.TestCase):
    def setUp(self):
        self.fixture = submission_tests.PackageTests(methodName='runTest')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        fixture = self.fixture
        self.root = fixture.source
        self.package = self.root / fixture.name
        self.package.write_bytes(fixture.package)
        self.report = self.root / (fixture.name + '.json')
        self.report.write_text(json.dumps(fixture.report), encoding='utf-8')
        self.inputs = dict(commit=fixture.commit, version='0.26.0', previous_version='1.25.0.0',
                           portable_sha256=fixture.portable_sha, package_sha256=digest(self.package),
                           report_sha256=digest(self.report))

    def qualify(self, **overrides):
        with mock.patch.object(preflight.store, 'Store', side_effect=AssertionError('No authentication')), \
                mock.patch.object(preflight.store, 'request', side_effect=AssertionError('No network')):
            return preflight.qualify(self.root, self.fixture.portable, self.package, self.report,
                                     **dict(self.inputs, **overrides))

    def test_real_content_is_verified_without_granting_native_or_store_authority(self):
        result = self.qualify()
        self.assertTrue(result['content_verified'])
        self.assertFalse(result['ready'])
        self.assertFalse(result['production_ready'])
        self.assertEqual(result['native_sdk_validation'], 'not-performed-by-consumer')
        self.assertEqual(result['native_producer_authentication'], 'unverified-requires-trusted-native-job')
        self.assertEqual(result['previous_version_authority'], 'operator-selected-not-service-reconciled')
        self.assertEqual(result['store_submission'], 'not-performed')
        self.assertEqual(result['public_availability'], 'unverified')
        self.assertEqual(result['package']['sha256'], digest(self.package))
        self.assertNotIn(str(self.root), json.dumps(result))
        self.assertNotIn('validation', result, 'Producer SDK assertion must not become consumer authority')


    @unittest.skipUnless(preflight.store.msix.os.name == 'nt', 'Actual Windows SDK package requires Windows')
    def test_native_sdk_store_identity_package_passes_the_same_independent_preflight(self):
        report = preflight.store.msix.build(self.root, self.fixture.portable, self.root / 'sdk-final',
            self.fixture.report['identity'], previous_version='1.25.0.0',
            expected_sha256=self.inputs['portable_sha256'], expected_package_family=preflight.store.FAMILY)
        package = self.root / 'sdk-final' / report['package']
        producer = package.with_name(package.name + '.json')
        result = preflight.qualify(self.root, self.fixture.portable, package, producer,
            **dict(self.inputs, package_sha256=digest(package), report_sha256=digest(producer)))
        self.assertTrue(result['content_verified'])
        self.assertFalse(result['ready'])
        self.assertEqual(result['native_sdk_validation'], 'not-performed-by-consumer')

    def test_explicit_hashes_stable_identity_and_monotonic_version_are_required(self):
        for key, value in [('commit', 'a' * 40), ('commit', True), ('version', '0.26.0-dev.1'),
                           ('version', '00.26.0'), ('previous_version', '1.26.0.0'),
                           ('previous_version', '1.27.0.0'), ('previous_version', '01.25.0.0'),
                           ('portable_sha256', 'b' * 64), ('package_sha256', 'c' * 64),
                           ('report_sha256', 'd' * 64), ('report_sha256', '')]:
            with self.subTest(field=key, value=value), self.assertRaises(ValueError):
                self.qualify(**{key: value})

    def test_updated_hashes_and_report_do_not_hide_a_foreign_payload(self):
        payload = dict(self.fixture.payload, **{'peerbrush.exe': b'foreign binary'})
        self.package.write_bytes(synthetic_package(payload))
        report = dict(self.fixture.report, sha256=digest(self.package))
        self.report.write_text(json.dumps(report), encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'payload differs'):
            self.qualify(package_sha256=digest(self.package), report_sha256=digest(self.report))

    def test_producer_metadata_cannot_add_private_fields_change_identity_or_claim_signing(self):
        for change in [dict(private_note='PRIVATE_PREFLIGHT_SENTINEL'), dict(signed=True),
                       dict(installed_tested=True), dict(package_family_name='foreign_family'),
                       dict(validation='verified'), dict(identity=dict(self.fixture.report['identity'], name='Other'))]:
            self.report.write_text(json.dumps(dict(self.fixture.report, **change)), encoding='utf-8')
            with self.subTest(change=list(change)), self.assertRaisesRegex(ValueError, 'report mismatch') as failure:
                self.qualify(report_sha256=digest(self.report))
            self.assertNotIn('PRIVATE_PREFLIGHT_SENTINEL', str(failure.exception))

    def test_duplicate_or_oversized_report_is_rejected_and_cannot_be_a_signature(self):
        original = json.dumps(self.fixture.report)
        for data in [original.replace('"schema": 1', '"schema": 1, "schema": 1').encode(),
                     b' ' * (64 * 1024 + 1), b'[]', b'null', b'\xff']:
            self.report.write_bytes(data)
            with self.subTest(size=len(data)), self.assertRaises(ValueError):
                self.qualify(report_sha256=digest(self.report))
        self.report.write_text(original, encoding='utf-8')
        payload = dict(self.fixture.payload, **{'AppxSignature.p7x': b'untrusted signature'})
        self.package.write_bytes(synthetic_package(payload))
        self.report.write_text(json.dumps(dict(self.fixture.report, sha256=digest(self.package))), encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'entries|signature'):
            self.qualify(package_sha256=digest(self.package), report_sha256=digest(self.report))

    def test_source_and_input_changes_during_check_reject_success(self):
        original = preflight.store.validate_package
        def changed(*args, **kwargs):
            original(*args, **kwargs)
            self.package.write_bytes(self.package.read_bytes() + b'changed')
        with mock.patch.object(preflight.store, 'validate_package', side_effect=changed), \
                self.assertRaisesRegex(ValueError, 'changed during preflight'):
            self.qualify()

    def test_output_is_exclusive_outside_git_and_cli_hides_exception_details(self):
        result = self.qualify()
        with tempfile.TemporaryDirectory(prefix='peerbrush-store-report-') as temporary:
            parent = Path(temporary).resolve()
            output = parent / 'qualified'
            preflight.write_report(result, output, self.root)
            self.assertEqual(json.loads((output / 'store-msix-qualification.json').read_bytes()), result)
            with self.assertRaises(ValueError):
                preflight.write_report(result, output, self.root)
        with self.assertRaises(ValueError):
            preflight.write_report(result, self.root / 'unsafe-output', self.root)
        args = ['preflight', '--source-root', str(self.root), '--portable', str(self.fixture.portable),
                '--package', str(self.package), '--report', str(self.report), '--output', str(self.root / 'unsafe'),
                *[word for name, value in self.inputs.items() for word in ['--' + name.replace('_', '-'), value]]]
        error = io.StringIO()
        with mock.patch.object(preflight.sys, 'argv', args), mock.patch.object(preflight, 'qualify') as check, \
                contextlib.redirect_stderr(error):
            self.assertEqual(preflight.main(), 1)
            check.assert_not_called()
        self.assertNotIn(str(self.root), error.getvalue())
        self.assertIn('no authentication', error.getvalue())


if __name__ == '__main__':
    unittest.main()
