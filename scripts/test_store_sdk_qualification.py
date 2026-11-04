"""Real content/SDK tests and explicitly fake origin transport; no Store or installation."""
import copy
import contextlib
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import zipfile

from package import digest
import qualify_store_sdk as qualifier
import test_store_msix_preflight as content_fixture
import test_macos_qualification_attestation as crypto_fixture
import test_macos_qualification_job as service_fixture
import verify_store_qualification as adapter
import verify_macos_qualification as shared
import final_release as final


class SdkContent(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='peerbrush-store-sdk-test-')
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.seed = content_fixture.StorePreflight('runTest')
        self.addCleanup(self.seed.doCleanups)
        self.seed.setUp()
        self.root = self.seed.root
        self.tool = self.base / 'MakeAppx.exe'
        self.tool.write_bytes(b'SYNTHETIC_SDK_TOOL_NEVER_EXECUTED')
        self.serial = 0
        self.options = dict(self.seed.inputs, portable=self.seed.fixture.portable,
            package=self.seed.package, report=self.seed.report, makeappx=self.tool,
            makeappx_sha256=digest(self.tool), makeappx_version='10.0.26100.1')

    def fake_sdk(self, executable, args, work, label, **kwargs):
        self.assertEqual(executable, self.tool)
        self.assertEqual(label, 'makeappx-unpack')
        self.assertEqual(args[0], 'unpack')
        self.assertEqual(args[-1], '/no')
        self.assertNotIn('/nv', args)
        self.assertEqual(kwargs, {'_windows_host': True})
        output = Path(args[args.index('/d') + 1])
        # Simulated Windows extraction still obeys the real host's private-work guard.
        output.mkdir(mode=0o700)
        expected = dict(self.seed.fixture.payload)
        with zipfile.ZipFile(Path(args[args.index('/p') + 1])) as archive:
            expected['AppxBlockMap.xml'] = archive.read('AppxBlockMap.xml')
        for name, data in expected.items():
            path = output / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

    def selected_sdk(self, value, checksum, version):
        if value != self.tool or checksum != digest(self.tool) or version != '10.0.26100.1':
            raise ValueError('Synthetic SDK selection changed.')
        return self.tool

    def qualify(self, **changes):
        self.serial += 1
        output = self.base / ('sdk-evidence-' + str(self.serial))
        work = self.base / ('sdk-work-' + str(self.serial))
        with mock.patch.object(qualifier.sys, 'platform', 'win32'), \
                mock.patch.object(qualifier.platform, 'machine', return_value='AMD64'), \
                mock.patch.object(qualifier, 'checked_sdk', side_effect=self.selected_sdk), \
                mock.patch.object(qualifier, 'run_native', side_effect=self.fake_sdk):
            return qualifier.qualify(self.root, output, work, **dict(self.options, **changes))

    def test_source_derived_inventory_and_limited_sdk_result_keep_every_authority_hold(self):
        result = self.qualify()
        self.assertEqual(result['format'], qualifier.FORMAT)
        self.assertEqual(result['sdk'], dict(tool='MakeAppx.exe', sha256=digest(self.tool), version='10.0.26100.1',
            command='unpack', semantic_validation='enabled-limited-sdk-validation', payload='exact-source-derived-bytes'))
        self.assertFalse(result['ready'] or result['production_ready'])
        self.assertEqual(result['native_producer_authentication'], 'unverified-requires-trusted-native-job')
        self.assertEqual(result['installed_acceptance'], 'not-performed')
        self.assertEqual(result['public_availability'], 'unverified')
        self.assertNotIn(str(self.root), json.dumps(result))

    def test_input_and_tool_pins_cannot_be_replaced_with_report_claims(self):
        for key, value in [('package_sha256', 'a' * 64), ('report_sha256', 'b' * 64),
                ('portable_sha256', 'c' * 64), ('makeappx_sha256', 'd' * 64), ('makeappx_version', '10.0.26100.2')]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.qualify(**{key: value})

    def test_native_failure_or_foreign_extraction_never_produces_a_report(self):
        original = self.fake_sdk
        for change in ['missing', 'foreign', 'changed', 'linked', 'error']:
            def run(*args, **kwargs):
                if change == 'error': raise ValueError('PRIVATE_SDK_SENTINEL')
                original(*args, **kwargs)
                output = Path(args[1][args[1].index('/d') + 1])
                if change == 'missing': (output / 'peerbrush.exe').unlink()
                elif change == 'changed': (output / 'peerbrush.exe').write_bytes(b'foreign')
                elif change == 'linked': os.link(output / 'peerbrush.exe', self.base / ('linked-payload-' + str(self.serial)))
                else: (output / 'unselected.txt').write_bytes(b'foreign')
            with self.subTest(change=change), mock.patch.object(self, 'fake_sdk', side_effect=run), self.assertRaises(ValueError):
                self.qualify()
            self.assertFalse((self.base / ('sdk-evidence-' + str(self.serial))).exists())

    def test_sdk_tool_hardlinks_and_source_boundary_outputs_are_refused(self):
        checksum = digest(self.tool)
        linked = self.base / 'linked-tool'
        os.link(self.tool, linked)
        try:
            with mock.patch.object(qualifier, 'tool_version') as version, self.assertRaises(ValueError):
                qualifier.checked_sdk(self.tool, checksum, '10.0.26100.1')
            version.assert_not_called()
        finally:
            linked.unlink()
        output = self.root / 'forbidden-sdk-output'
        with mock.patch.object(qualifier.sys, 'platform', 'win32'), \
                mock.patch.object(qualifier.platform, 'machine', return_value='AMD64'), \
                mock.patch.object(qualifier, 'run_native') as process, self.assertRaises(ValueError):
            qualifier.qualify(self.root, output, self.base / 'bad-boundary-work', **self.options)
        process.assert_not_called()
        self.assertFalse(output.exists())

    def test_late_package_tool_and_inventory_changes_are_rechecked(self):
        original = qualifier.verify_unpacked
        for change in ['package', 'tool', 'inventory']:
            package_bytes, tool_bytes = self.seed.package.read_bytes(), self.tool.read_bytes()
            calls = 0
            def verify(*args):
                nonlocal calls
                original(*args)
                calls += 1
                if calls == 2:
                    if change == 'package': self.seed.package.write_bytes(package_bytes + b'changed')
                    elif change == 'tool': self.tool.write_bytes(tool_bytes + b'changed')
                    else: (args[0] / 'late-extra.txt').write_bytes(b'changed')
            with self.subTest(change=change), mock.patch.object(qualifier, 'verify_unpacked', side_effect=verify), self.assertRaises(ValueError):
                self.qualify()
            self.seed.package.write_bytes(package_bytes)
            self.tool.write_bytes(tool_bytes)

    def test_versions_are_canonical_bounded_and_non_native_hosts_refuse_before_execution(self):
        for value in ['10.0.26100.1', '0.0.0.0', '65535.65535.65535.65535']:
            self.assertTrue(qualifier.sdk_version(value))
        for value in [True, None, '', '10.0.26100', '10.00.26100.1', '10.0.26100.65536']:
            self.assertFalse(qualifier.sdk_version(value))
        with mock.patch.object(qualifier.sys, 'platform', 'linux'), mock.patch.object(qualifier, 'run_native') as process, self.assertRaises(ValueError):
            qualifier.qualify(self.root, self.base / 'host-output', self.base / 'host-work', **self.options)
        process.assert_not_called()

    @unittest.skipUnless(os.name == 'nt', 'Requires actual Windows SDK')
    def test_actual_sdk_unpacks_the_exact_selected_store_package_without_installation(self):
        msix = qualifier.content.store.msix
        produced = self.base / 'actual-sdk-package'
        report = msix.build(self.root, self.seed.fixture.portable, produced, self.seed.fixture.report['identity'],
            previous_version='1.25.0.0', expected_sha256=self.options['portable_sha256'],
            expected_package_family=qualifier.content.store.FAMILY)
        sdk = msix.find_makeappx()
        package = produced / report['package']
        producer = package.with_name(package.name + '.json')
        options = dict(self.options, package=package, package_sha256=digest(package), report=producer,
            report_sha256=digest(producer), makeappx=sdk, makeappx_sha256=digest(sdk), makeappx_version=qualifier.tool_version(sdk))
        result = qualifier.qualify(self.root, self.base / 'actual-sdk-evidence', self.base / 'actual-sdk-work', **options)
        self.assertEqual(result['sdk']['sha256'], digest(sdk))
        self.assertFalse(result['ready'] or result['production_ready'])


class StoreOrigin(unittest.TestCase):
    def setUp(self):
        self.fixture = service_fixture.Readback('runTest')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.trust = self.fixture.trust
        self.trust.qualification = json.dumps(dict(format=qualifier.FORMAT, version='0.2.0', commit='a' * 40)).encode()
        policy = shared._policy(False, False, True)
        self.workflow, self.identity = policy[:2]
        self.fixture.run['path'] = self.workflow
        for job in self.fixture.jobs['jobs']:
            job['labels'] = ['windows-2025']
            for step in job.get('steps', []):
                if step['name'] == 'Acquire selected signed assets': step['name'] = 'Acquire selected Store assets'
                elif step['name'] == 'Qualify selected final DMG': step['name'] = policy[6]

    def command(self, executable, arguments, work, label):
        if label == 'version': return self.trust.command(executable, arguments, work, label)
        values = crypto_fixture.verified('a' * 40, hashlib.sha256(self.trust.qualification).hexdigest())
        result = values[0]['verificationResult']
        for name in ['subjectAlternativeName', 'buildSignerURI', 'buildConfigURI']:
            result['signature']['certificate'][name] = self.identity
        result['statement']['subject'][0]['name'] = qualifier.REPORT
        return json.dumps(values).encode()

    def authenticate(self, **changes):
        self.fixture.serial += 1
        values = dict(self.trust.values, work=self.trust.base / ('store-' + str(self.fixture.serial)), readback_job=True, **changes)
        with mock.patch.object(shared.sys, 'platform', 'linux'), \
                mock.patch.object(shared.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(shared, '_command', side_effect=self.command), \
                mock.patch.object(service_fixture.website, 'public_get', side_effect=self.fixture.get):
            return adapter.authenticate(self.trust.qualification,
                qualification_sha256=hashlib.sha256(self.trust.qualification).hexdigest(), commit='a' * 40,
                version='0.2.0', source_root=self.trust.root, **values)

    def test_fixed_origin_checks_pinned_crypto_and_exact_windows_job_without_readiness(self):
        result = self.authenticate()
        self.assertEqual(result['format'], 'PeerBrushStoreSdkNativeOriginV1')
        self.assertEqual(result['native_job_conclusion'], 'service-reconciled-success')
        self.assertEqual(result['native_service']['workflow'], self.workflow)
        self.assertEqual(result['native_service']['runner_label'], 'windows-2025')
        self.assertFalse(result['candidate_verified'] or result['ready'] or result['production_ready'])
        self.assertEqual(result['installed_acceptance'], 'not-performed')

    def test_foreign_workflow_runner_failed_step_or_rerun_cannot_grant_sdk_origin(self):
        original_run, original_jobs = copy.deepcopy(self.fixture.run), copy.deepcopy(self.fixture.jobs)
        for change in ['workflow', 'runner', 'step', 'rerun']:
            self.fixture.run, self.fixture.jobs = copy.deepcopy(original_run), copy.deepcopy(original_jobs)
            if change == 'workflow': self.fixture.run['path'] = shared.WORKFLOW
            elif change == 'runner': self.fixture.jobs['jobs'][1]['labels'] = ['self-hosted', 'windows-2025']
            elif change == 'step': self.fixture.jobs['jobs'][1]['steps'][5]['conclusion'] = 'failure'
            else: self.fixture.run['run_attempt'] = 2
            with self.subTest(change=change), self.assertRaises(ValueError): self.authenticate()

    def test_fixed_adapters_cannot_select_a_different_origin_or_setup_host(self):
        import verify_linux_qualification as linux
        import verify_windows_qualification as windows
        for function in [adapter.authenticate, linux.authenticate, windows.authenticate, windows.authenticate_setup]:
            for field in ['_store', '_windows', '_linux', '_setup_host']:
                with self.subTest(adapter=function.__module__, field=field), self.assertRaises(ValueError):
                    function(b'{}', **{field: True})


class StoreBinding(unittest.TestCase):
    def setUp(self):
        self.seed = content_fixture.StorePreflight('runTest')
        self.addCleanup(self.seed.doCleanups)
        self.seed.setUp()
        temporary = tempfile.TemporaryDirectory(prefix='peerbrush-store-bound-origin-')
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.service = service_fixture.Readback('runTest')
        self.addCleanup(self.service.doCleanups)
        self.service.setUp()
        self.commit = self.seed.inputs['commit']
        self.version = self.seed.inputs['version']
        checked = qualifier.content.qualify(self.seed.root, self.seed.fixture.portable,
            self.seed.package, self.seed.report, **self.seed.inputs)
        report = qualifier.report_for(checked, makeappx_sha256='b' * 64, makeappx_version='10.0.26100.1')
        self.report = self.base / qualifier.REPORT
        self.report.write_bytes(qualifier.encode(report))
        self.qualification = self.report.read_bytes()
        self.policy = shared._policy(False, False, True)
        self.service.run.update(path=self.policy[0], head_sha=self.commit)
        self.service.run['head_commit']['id'] = self.commit
        for job in self.service.jobs['jobs']:
            job.update(head_sha=self.commit, labels=['windows-2025'])
            for step in job.get('steps', []):
                if step['name'] == 'Acquire selected signed assets': step['name'] = 'Acquire selected Store assets'
                elif step['name'] == 'Qualify selected final DMG': step['name'] = self.policy[6]
        trust = dict(self.service.trust.values, workflow_commit=self.commit, readback_job=True, work=self.base / 'owned')
        self.native = dict(qualification=self.report, qualification_sha256=digest(self.report),
            makeappx_sha256='b' * 64, makeappx_version='10.0.26100.1', trust=trust)
        self.selection = dict(package=self.seed.package, report=self.seed.report,
            **{key: self.seed.inputs[key] for key in ['package_sha256', 'report_sha256', 'previous_version']}, native=self.native)
        self.inventory = dict(commit=self.commit, version=self.version,
            assets=[dict(platform='Windows', file=self.seed.fixture.portable.name, sha256=self.seed.inputs['portable_sha256'])])

    def command(self, executable, arguments, work, label):
        if label == 'version': return self.service.trust.command(executable, arguments, work, label)
        values = crypto_fixture.verified(self.commit, hashlib.sha256(self.qualification).hexdigest())
        result = values[0]['verificationResult']
        for name in ['subjectAlternativeName', 'buildSignerURI', 'buildConfigURI']:
            result['signature']['certificate'][name] = self.policy[1]
        result['statement']['subject'][0]['name'] = qualifier.REPORT
        return json.dumps(values).encode()

    @contextlib.contextmanager
    def seams(self):
        with mock.patch.object(shared.sys, 'platform', 'linux'), \
                mock.patch.object(shared.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(shared, '_command', side_effect=self.command), \
                mock.patch.object(service_fixture.website, 'public_get', side_effect=self.service.get):
            yield

    def bind(self):
        with self.seams():
            return final.store_binding(self.seed.root, self.inventory, self.seed.fixture.portable.parent, self.selection)

    def test_actual_content_and_origin_consumers_retain_native_sdk_proof_without_installed_or_public_authority(self):
        result = self.bind()
        self.assertEqual(result[3]['native_sdk_validation'], 'authenticated-selected-sdk-limited-validation')
        self.assertEqual(result[3]['native_origin']['native_job_conclusion'], 'service-reconciled-success')
        self.assertFalse(result[1]['ready'] or result[3]['production_ready'])
        self.assertEqual(result[3]['installed_acceptance'], 'not-performed')
        self.assertEqual(result[3]['public_availability'], 'unverified')
        self.assertEqual((self.base / 'owned' / qualifier.REPORT).read_bytes(), self.qualification)
        for path, limit, checksum in result[2]:
            self.assertEqual(qualifier.bounded_file(path, limit)[0], checksum)

    def test_report_claims_without_matching_content_tool_or_authenticated_job_refuse(self):
        original = json.loads(self.qualification)
        for change in ['tool', 'content', 'ready', 'installed', 'field']:
            value = copy.deepcopy(original)
            if change == 'tool': value['sdk']['sha256'] = 'c' * 64
            elif change == 'content': value['content']['package']['sha256'] = 'd' * 64
            elif change == 'ready': value['ready'] = True
            elif change == 'installed': value['installed_acceptance'] = 'passed'
            else: value['private_field'] = 'PRIVATE_ORIGIN_SENTINEL'
            self.report.write_bytes(qualifier.encode(value))
            self.native['qualification_sha256'] = digest(self.report)
            with self.subTest(change=change), self.assertRaises(ValueError): self.bind()
            self.assertFalse((self.base / 'owned').exists())
        self.report.write_bytes(self.qualification)
        self.native['qualification_sha256'] = digest(self.report)
        self.service.run['conclusion'] = 'failure'
        with self.assertRaises(ValueError): self.bind()

    def test_late_original_report_mutation_cannot_survive_authentication(self):
        command = self.command
        def changed(*args):
            result = command(*args)
            if args[3] == 'verification': self.report.write_bytes(self.qualification + b'changed')
            return result
        with mock.patch.object(self, 'command', side_effect=changed), self.assertRaises(ValueError): self.bind()


if __name__ == '__main__':
    unittest.main()
