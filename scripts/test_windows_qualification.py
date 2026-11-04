"""Offline real-byte callers with fake native/crypto/service boundaries; no trust granted."""
import copy
import hashlib
import json
from pathlib import Path
import textwrap
import unittest
from unittest import mock

import acquire_windows_qualification_inputs as acquisition
import final_release as final
import prepare_site_release as website
import qualify_windows_installer as qualifier
import test_appinstaller
import test_final_release
import test_macos_qualification_attestation as crypto_fixtures
import test_macos_qualification_job as service_fixtures
import test_macos_qualification_producer as transport_fixtures
import test_windows_executable_signing as signing_fixtures
import verify_macos_qualification as shared
import verify_windows_qualification as native
import produce_windows_installer as producer

ROOT = Path(__file__).resolve().parents[1]


def sha(data):
    return hashlib.sha256(data).hexdigest()


class WindowsFacts(unittest.TestCase):
    def setUp(self):
        self.f = test_final_release.Composition(methodName='runTest')
        self.addCleanup(self.f.doCleanups)
        self.f.setUp()
        f = self.f
        portable = next(row for row in f.inventory['assets'] if row.get('platform') == 'Windows')
        self.options = {key: value for key, value in f.windows.items()
                        if key not in ['identity_sha256', 'descriptor', 'descriptor_sha256']}
        self.options.update(commit=f.fixture.commit, version='0.2.0', portable=f.directory / portable['file'],
            portable_sha256=portable['sha256'], runtime_license=f.direct / 'runtime-license.txt',
            runtime_license_sha256=final.signed.msix.portable.digest(f.direct / 'runtime-license.txt'),
            certificate_sha256='a' * 64, runtime_certificate_sha256='c' * 64,
            signtool=f.direct / 'signtool.exe', powershell=f.direct / 'pwsh.exe',
            signtool_sha256=final.signed.msix.portable.digest(f.direct / 'signtool.exe'),
            powershell_sha256=final.signed.msix.portable.digest(f.direct / 'pwsh.exe'),
            timestamp_certificate_sha256='b' * 64, executable_timestamp_certificate_sha256='b' * 64,
            runtime_timestamp_certificate_sha256='b' * 64)
        self.calls = []
        self.serial = 0

    def command(self, tool, arguments, work, label, **values):
        selected = values.get('package') if label == 'certificate' else Path(arguments[-1])
        self.calls.append((label, selected.name))
        if label == 'signtool':
            self.assertEqual(arguments[:5], ['verify', '/pa', '/all', '/tw', '/q'])
            return b'NOT REAL NATIVE AUTHORITY'
        runtime = selected.name == self.f.runtime.name
        return json.dumps(dict(schema=1, powershell=final.signed.POWERSHELL_VERSION, status=0, kind=1,
            subject=test_appinstaller.PUBLISHER if runtime else self.f.identity['publisher'],
            certificate_sha256='c' * 64 if runtime else 'a' * 64,
            timestamp_certificate_sha256='b' * 64, signers=1, timestamp_attributes=1, digest='sha256')).encode()

    def qualify(self, **changes):
        self.serial += 1
        self.report_dir = self.f.fixture.base / ('windows-report-' + str(self.serial))
        options = dict(self.options, work=self.f.fixture.base / ('windows-work-' + str(self.serial)),
                       output=self.f.fixture.base / ('windows-descriptor-' + str(self.serial)), **changes)
        with mock.patch.object(final.signed.sys, 'platform', 'win32'), \
                mock.patch.object(final.signed, '_command', side_effect=self.command):
            result = qualifier.qualify(self.f.root, self.report_dir, **options)
        self.report_path = self.report_dir / qualifier.REPORT
        return result

    def test_actual_shared_exe_msix_runtime_descriptor_checks_emit_only_unready_facts(self):
        report = self.qualify()
        self.assertEqual(self.calls, [('signtool', 'peerbrush.exe'), ('certificate', 'peerbrush.exe'),
            ('signtool', self.f.package.name), ('certificate', self.f.package.name),
            ('signtool', self.f.runtime.name), ('certificate', self.f.runtime.name)])
        self.assertEqual(report['signed_package']['executable']['unsigned_sha256'], sha(self.f.original_executable))
        self.assertEqual(report['descriptor']['sha256'], sha(self.f.descriptor.read_bytes()))
        self.assertEqual(report['runtime']['certificate_sha256'], 'c' * 64)
        self.assertFalse(report['ready'] or report['production_ready'])
        self.assertEqual(report['installed_acceptance'], 'not-performed')
        self.assertEqual(report['runtime_redistribution_approval'], 'not-authenticated')
        self.assertNotIn(str(self.f.fixture.base), self.report_path.read_text())

    def test_timestamp_native_failure_and_output_reuse_cannot_emit_a_qualification(self):
        for key in qualifier.TIMESTAMPS:
            with self.subTest(pin=key), self.assertRaisesRegex(ValueError, 'timestamp leaves'):
                self.qualify(**{key: 'd' * 64})
            self.assertFalse(self.report_dir.exists())
        with mock.patch.object(self, 'command', side_effect=ValueError('PRIVATE_NATIVE_FAILURE')):
            with self.assertRaises(ValueError):
                self.qualify()
        self.assertFalse(self.report_dir.exists())
        self.qualify()
        before = self.report_path.read_bytes()
        with self.assertRaises(ValueError):
            qualifier.qualify(self.f.root, self.report_dir, **dict(self.options,
                work=self.f.fixture.base / 'reuse-work', output=self.f.fixture.base / 'reuse-output'))
        self.assertEqual(self.report_path.read_bytes(), before)

    def trust(self):
        fixture = service_fixtures.Readback(methodName='runTest')
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        fixture.run['head_sha'] = self.f.fixture.commit
        fixture.run['head_commit']['id'] = self.f.fixture.commit
        fixture.run['path'] = '.github/workflows/windows-qualification.yml'
        for job in fixture.jobs['jobs']:
            job['head_sha'] = self.f.fixture.commit
            job['labels'] = ['windows-2025']
            for step in job.get('steps', []):
                if step['name'] == 'Qualify selected final DMG':
                    step['name'] = 'Qualify selected final Windows installer'
        self.crypto_commands = []
        def command(executable, arguments, work, label):
            self.crypto_commands.append(arguments)
            if label == 'version':
                return b'gh version 2.102.0 (2026-09-30)\nhttps://github.com/cli/cli/releases/tag/v2.102.0\n'
            value = crypto_fixtures.verified(self.f.fixture.commit, sha(self.report_path.read_bytes()))
            result = value[0]['verificationResult']
            identity = native.native._policy(True)[1]
            for key in ['subjectAlternativeName', 'buildSignerURI', 'buildConfigURI']:
                result['signature']['certificate'][key] = identity
            result['statement']['subject'][0]['name'] = qualifier.REPORT
            return json.dumps(value).encode()
        self.crypto_command = command
        values = dict(fixture.trust.values, workflow_commit=self.f.fixture.commit, readback_job=True,
                      work=self.f.fixture.base / 'windows-native-origin')
        selection = {key: self.options[key] for key in native.FIELDS - {'qualification', 'qualification_sha256', 'trust'}}
        selection.update(qualification=self.report_path, qualification_sha256=sha(self.report_path.read_bytes()), trust=values)
        return fixture, selection

    def compose(self, service, selection):
        self.serial += 1
        options = copy.deepcopy(self.f.options)
        options['macos']['trust']['work'] = self.f.fixture.base / ('windows-compose-mac-' + str(self.serial))
        options['windows'] = dict(self.f.windows, native=copy.deepcopy(selection))
        options['windows']['native']['trust']['work'] = self.f.fixture.base / ('windows-compose-origin-' + str(self.serial))
        real = shared.authenticate
        def route(qualification, **inputs):
            if inputs.get('_windows') is True:
                return real(qualification, **inputs)
            work = Path(inputs['work'])
            work.mkdir(mode=0o700)
            for key, name in [('bundle', 'bundle.jsonl'), ('trusted_root', 'trusted_root.jsonl'), ('cli', 'gh')]:
                (work / name).write_bytes(Path(inputs[key]).read_bytes())
            (work / 'dmg-qualification.json').write_bytes(qualification)
            return self.f.origin
        with mock.patch.object(shared, 'authenticate', side_effect=route), \
                mock.patch.object(shared.sys, 'platform', 'linux'), mock.patch.object(shared.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(shared, '_command', side_effect=self.crypto_command), \
                mock.patch.object(website, 'public_get', side_effect=service.get):
            return final.compose(self.f.root, self.f.directory, self.f.output, **options)

    def test_full_composition_authenticates_its_actual_native_job_and_preserves_all_installed_public_gates(self):
        self.qualify()
        service, selection = self.trust()
        candidate = self.compose(service, selection)
        self.assertEqual(len(service.calls), 2)
        self.assertIn('--deny-self-hosted-runners', self.crypto_commands[-1])
        self.assertEqual(self.crypto_commands[-1][self.crypto_commands[-1].index('--cert-identity') + 1], native.native._policy(True)[1])
        self.assertEqual(candidate.assessment['windows_native_signature'], 'verified-by-authenticated-native-qualification')
        self.assertEqual(candidate.assessment['windows_native_origin']['native_service']['runner_label'], 'windows-2025')
        self.assertNotIn('windows-native-final-producer-not-authenticated', candidate.blockers)
        self.assertIn('windows-installed-acceptance-not-established', candidate.blockers)
        self.assertIn('windows-runtime-redistribution-approval-not-authenticated', candidate.blockers)
        self.assertFalse(candidate.assessment['production_ready'])
        self.assertFalse(candidate.manifest['channels']['windows-direct']['ready'])
        self.assertEqual(candidate.manifest['channels']['microsoft-store'], final.store_channel())
        candidate.verify()

    def test_wrong_report_native_job_or_crypto_failure_cannot_prepare_final_output(self):
        report = self.qualify()
        service, selection = self.trust()
        before = self.report_path.read_bytes()
        for key in ['certificate_sha256', 'timestamp_certificate_sha256', 'signtool_sha256', 'runtime_certificate_sha256']:
            changed = dict(selection, **{key: 'd' * 64})
            with self.subTest(pin=key), self.assertRaisesRegex(ValueError, 'actual selected bytes'):
                self.compose(service, changed)
            self.assertFalse(self.f.output.exists())
        changed = copy.deepcopy(report)
        changed['ready'] = True
        self.report_path.write_text(json.dumps(changed))
        try:
            with self.assertRaisesRegex(ValueError, 'actual selected bytes'):
                self.compose(service, dict(selection, qualification_sha256=sha(self.report_path.read_bytes())))
        finally:
            self.report_path.write_bytes(before)
        # Both origin and exact job success are mandatory even for matching report bytes.
        real = self.crypto_command
        self.crypto_command = mock.Mock(side_effect=ValueError('PRIVATE_CRYPTO_FAILURE'))
        with self.assertRaises(ValueError):
            self.compose(service, selection)
        self.assertEqual(service.calls, [])
        self.crypto_command = real
        service.jobs['jobs'][1]['labels'] = ['self-hosted', 'windows-2025']
        # Use a new immutable verification work directory after each refused attempt.
        selection['trust']['work'] = self.f.fixture.base / 'windows-wrong-runner'
        with self.assertRaisesRegex(ValueError, 'successful exact-attempt'):
            self.compose(service, selection)
        self.assertFalse(self.f.output.exists())

    def test_actual_two_artifact_handoff_binds_trusted_portable_source_and_all_six_inner_files(self):
        q = {key: str(value) if isinstance(value, Path) else value for key, value in self.options.items()}
        source_name = 'PeerBrush-0.2.0-source.zip'
        bodies = {}
        for key in ['portable', 'unsigned', 'package', 'runtime', 'identity', 'runtime_license']:
            path = Path(self.options[key])
            name = 'direct-identity.json' if key == 'identity' else 'runtime-license.txt' if key == 'runtime_license' else path.name
            q[key] = name
            bodies[name] = path.read_bytes()
        original = {q['portable']: bodies[q['portable']], source_name: (self.f.directory / source_name).read_bytes()}
        zipped = lambda rows: transport_fixtures.zipped([(name, value, 0o100644) for name, value in rows.items()])
        wrappers = {'201': zipped(original), '202': zipped(bodies)}
        repository = dict(id=acquisition.transport.REPOSITORY_ID, full_name=acquisition.transport.REPOSITORY,
                          owner=dict(id=acquisition.transport.OWNER_ID), fork=False, private=False)
        runs, artifacts = {}, {}
        for run_id, artifact_id, workflow, name in [
            ('101', '201', '.github/workflows/release.yml', 'candidate-Windows'),
            ('102', '202', '.github/workflows/windows-release-signing.yml', 'windows-signed-inputs-102-1')]:
            runs[run_id] = dict(id=int(run_id), run_attempt=1, status='completed', conclusion='success',
                event='workflow_dispatch', path=workflow, head_branch='main', head_sha=self.f.fixture.commit,
                pull_requests=[], repository=repository, head_repository=copy.deepcopy(repository))
            artifacts[artifact_id] = dict(id=int(artifact_id), name=name, expired=False,
                digest='sha256:' + sha(wrappers[artifact_id]), size_in_bytes=len(wrappers[artifact_id]),
                url='https://api.github.com' + acquisition.transport.API + 'artifacts/' + artifact_id,
                archive_download_url='https://api.github.com' + acquisition.transport.API + 'artifacts/' + artifact_id + '/zip',
                workflow_run=dict(id=int(run_id), repository_id=acquisition.transport.REPOSITORY_ID,
                    head_repository_id=acquisition.transport.REPOSITORY_ID, head_branch='main', head_sha=self.f.fixture.commit))
        calls = []
        def get(host, target, limit, **options):
            calls.append((host, target))
            if host == 'api.github.com':
                self.assertEqual(options['token'], 'SYNTHETIC_READ_TOKEN')
                if target.endswith('/zip'):
                    return 'https://fixture.blob.core.windows.net/' + target.split('/')[-2]
                identifier = target.rsplit('/', 1)[1]
                return json.dumps(runs[identifier] if '/runs/' in target else artifacts[identifier]).encode()
            self.assertNotIn('token', options)
            with options['destination'].open('xb') as stream:
                stream.write(wrappers[target[1:]])
        selected = dict(commit=self.f.fixture.commit, qualification=q, transport=dict(
            portable_run_id='101', portable_artifact_id='201', portable_artifact_sha256=sha(wrappers['201']),
            signing_run_id='102', signing_artifact_id='202', signing_artifact_sha256=sha(wrappers['202']),
            source_sha256=sha(original[source_name]), files={name: sha(data) for name, data in bodies.items()}))
        environment = dict(GITHUB_REPOSITORY=acquisition.transport.REPOSITORY, GITHUB_REF='refs/heads/main',
            GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=acquisition.WORKFLOW,
            GITHUB_SHA=self.f.fixture.commit, GITHUB_WORKFLOW_SHA=self.f.fixture.commit, GITHUB_RUN_ATTEMPT='1',
            RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Windows', RUNNER_ARCH='X64', PEERBRUSH_WINDOWS_QUALIFICATION_ENABLED='true')
        output = self.f.fixture.base / 'windows-acquired'
        with mock.patch.object(acquisition.transport, 'request', side_effect=get):
            digest = acquisition.acquire(self.f.root, output, selected, token='SYNTHETIC_READ_TOKEN', environment=environment)
        self.assertEqual(sha((output / 'qualification-selection.json').read_bytes()), digest)
        for name, data in bodies.items():
            self.assertEqual((output / 'media' / name).read_bytes(), data)
        self.assertEqual(len(calls), 12, 'Each outer artifact is read and reconciled independently')
        with mock.patch.object(acquisition.transport, 'request') as request, self.assertRaises(ValueError):
            acquisition.acquire(self.f.root, self.f.fixture.base / 'not-created', selected, token='SYNTHETIC_READ_TOKEN', environment={})
        request.assert_not_called()
        self.assertFalse((self.f.fixture.base / 'not-created').exists())
        runs['102']['path'] = '.github/workflows/macos-release-signing.yml'
        with mock.patch.object(acquisition.transport, 'request', side_effect=get), self.assertRaises(ValueError):
            acquisition.acquire(self.f.root, self.f.fixture.base / 'wrong-predecessor', selected,
                                token='SYNTHETIC_READ_TOKEN', environment=environment)


class WindowsProducer(unittest.TestCase):
    def test_actual_existing_signer_calls_and_six_file_handoff_are_preserved(self):
        fixture = signing_fixtures.WindowsProducer(methodName='runTest')
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        f = fixture.fixture
        selected = {key: value for key, value in fixture.options.items() if key not in ['identity', 'approve_existing_certificate_signing']}
        media = f.external / 'handoff-public-media'
        media.mkdir(mode=0o700)
        selected['identity'] = media / 'direct-identity.json'
        selected['identity'].write_text(json.dumps(f.identity))
        selected['runtime'] = media / 'Microsoft.VCLibs.x64.Desktop.appx'
        selected['runtime'].write_bytes(test_appinstaller.runtime_bytes())
        selected['runtime_license'] = media / 'runtime-license.txt'
        selected['runtime_license'].write_text('Synthetic selected public license')
        for key in ['runtime', 'runtime_license']:
            selected[key + '_sha256'] = sha(selected[key].read_bytes())
        output = f.external / 'six-file-handoff'
        with mock.patch.object(producer.signer.sys, 'platform', 'win32'), \
                mock.patch.object(producer.signer.qualified, '_command', side_effect=fixture.native):
            result = producer.produce(f.root, f.portable, selected, work=f.external / 'bundle-work', output=output,
                                      approve_existing_certificate_signing=True)
        self.assertEqual(len(result), 6)
        self.assertEqual(set(result), {path.name for path in output.iterdir()})
        self.assertIn('sign-executable', fixture.calls)
        self.assertIn('sign-package', fixture.calls)
        self.assertNotIn('windows-signing-candidate.json', result, 'Signer JSON is never handed off as authority')
        with self.assertRaises(ValueError):
            producer.produce(f.root, f.portable, selected, work=f.external / 'another-work', output=output)


class WindowsWorkflow(unittest.TestCase):
    def test_both_producers_are_literal_disabled_and_guards_precede_checkout(self):
        for name in ['windows-release-signing.yml', 'windows-qualification.yml']:
            text = (ROOT / '.github/workflows' / name).read_text()
            self.assertEqual(text.count('if: ${{ false }}'), 2)
            sections = text.split('    steps:\n')[1:]
            self.assertEqual(len(sections), 2)
            for index, section in enumerate(sections):
                self.assertLess(section.index('Require reviewed main invocation'), section.index('Checkout reviewed main source'))
                inline = section.split("python -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
                program = compile(textwrap.dedent(inline), '<fixed-windows-source-guard>', 'exec')
                flag = 'PEERBRUSH_WINDOWS_SIGNING_ENABLED' if name == 'windows-release-signing.yml' else 'PEERBRUSH_WINDOWS_QUALIFICATION_ENABLED'
                environment = dict(SELECTED_COMMIT='a' * 40, GITHUB_REPOSITORY='Deftr0y/PeerBrush',
                    GITHUB_REPOSITORY_ID='1407648785', GITHUB_REPOSITORY_OWNER_ID='21202494',
                    GITHUB_REF='refs/heads/main', GITHUB_EVENT_NAME='workflow_dispatch',
                    GITHUB_WORKFLOW_REF='Deftr0y/PeerBrush/.github/workflows/' + name + '@refs/heads/main',
                    GITHUB_SHA='a' * 40, GITHUB_WORKFLOW_SHA='a' * 40, GITHUB_RUN_ATTEMPT='1',
                    RUNNER_OS='Windows', RUNNER_ARCH='X64',
                    RUNNER_ENVIRONMENT='self-hosted' if name == 'windows-release-signing.yml' and index == 1 else 'github-hosted',
                    **{flag: 'true'})
                with mock.patch.dict(acquisition.os.environ, environment, clear=True):
                    exec(program, {})
                for key in environment:
                    with self.subTest(workflow=name, job=index, context=key), \
                            mock.patch.dict(acquisition.os.environ, dict(environment, **{key: 'wrong'}), clear=True), \
                            self.assertRaises(SystemExit):
                        exec(program, {})
            if name == 'windows-qualification.yml':
                self.assertIn('runs-on: windows-2025', text)
                self.assertNotIn('approve-existing-certificate-signing', text)
                self.assertIn('subject-name: ' + qualifier.REPORT, text)
                first = text.index('      - name: Qualify selected final Windows installer')
                next_step = text.index('      - name: Attest generated native qualification', first)
                self.assertNotIn('\n      - ', text[first + 10:next_step])
            else:
                self.assertIn('runs-on: [self-hosted, Windows, X64, peerbrush-signing]', text)
                self.assertNotIn('id-token: write', text)
                self.assertNotIn('attestations: write', text)
                self.assertNotIn('PFX', text)



class WindowsDescriptorRetention(unittest.TestCase):
    def test_actual_retention_keeps_original_descriptor_and_rejects_missing_or_changed_media(self):
        fixture = WindowsFacts(methodName='runTest')
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        fixture.qualify()
        f = fixture.f
        workflow = (ROOT / '.github/workflows/windows-qualification.yml').read_text()
        section = workflow.split('      - name: Retain qualification and bundle\n', 1)[1].split('      - name: Upload qualification evidence\n', 1)[0]
        inline = section.split("python -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
        program = compile(textwrap.dedent(inline), '<actual-windows-descriptor-retention>', 'exec')
        descriptor_source = f.fixture.base / ('windows-descriptor-' + str(fixture.serial)) / f.descriptor.name
        original = descriptor_source.read_bytes()
        report = fixture.report_path.read_bytes()
        for case in ['original', 'missing', 'changed']:
            base = f.fixture.base / ('retention-' + case)
            base.mkdir(mode=0o700)
            reports, descriptors = base / 'peerbrush-windows-report', base / 'peerbrush-windows-descriptor'
            reports.mkdir(mode=0o700)
            descriptors.mkdir(mode=0o700)
            (reports / qualifier.REPORT).write_bytes(report)
            bundle = base / 'attestation.json'
            bundle.write_bytes(b'SYNTHETIC_UNTRUSTED_ATTESTATION')
            if case != 'missing':
                (descriptors / f.descriptor.name).write_bytes(original if case == 'original' else original + b' ')
            environment = dict(GITHUB_WORKSPACE=str(f.root), RUNNER_TEMP=str(base), SELECTED_COMMIT=f.fixture.commit,
                               SELECTED_BUNDLE=str(bundle))
            with self.subTest(case=case), mock.patch.dict(acquisition.os.environ, environment), \
                    mock.patch.object(acquisition.sys, 'path', list(acquisition.sys.path)):
                if case == 'original':
                    exec(program, {})
                    output = base / 'peerbrush-windows-evidence'
                    self.assertEqual({path.name for path in output.iterdir()},
                        {qualifier.REPORT, 'windows-installer-qualification.sigstore.json', f.descriptor.name})
                    self.assertEqual((output / f.descriptor.name).read_bytes(), original)
                    self.assertEqual((output / qualifier.REPORT).read_bytes(), report)
                    self.assertEqual((output / 'windows-installer-qualification.sigstore.json').read_bytes(), bundle.read_bytes())
                else:
                    with self.assertRaises(SystemExit):
                        exec(program, {})
            self.assertEqual(descriptor_source.read_bytes(), original)
            self.assertEqual(fixture.report_path.read_bytes(), report)


if __name__ == '__main__':
    unittest.main()
