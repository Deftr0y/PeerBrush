"""Real content consumers with fake crypto/service/transport; no native trust is granted."""
import ast
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import textwrap
import unittest
from unittest import mock

import acquire_linux_qualification_inputs as acquisition
import final_release as final
import prepare_site_release as website
import qualify_linux_deb as qualifier
import test_deb_version_upgrade as upgrade
import test_deb_installation as installation
import test_final_release
import test_macos_qualification_attestation as crypto_fixtures
import test_macos_qualification_job as service_fixtures
import test_macos_qualification_producer as transport_fixtures
import verify_linux_qualification as native
import verify_macos_qualification as shared

ROOT = Path(__file__).resolve().parents[1]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def driver_receipt(expected):
    # Evaluate only the actual existing driver's receipt expression, not its
    # installer/native operations; this independently binds the consumer schema.
    tree = ast.parse(Path(upgrade.__file__).read_text(encoding='utf-8'))
    check = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'check')
    value = next(node.value for node in check.body if isinstance(node, ast.Assign)
                 and any(isinstance(target, ast.Name) and target.id == 'receipt' for target in node.targets))
    expression = ast.fix_missing_locations(ast.Expression(value))
    constants = {name: getattr(upgrade, name) for name in ['OLD_VERSION', 'OLD_COMMIT', 'OLD_SHA256']}
    return eval(compile(expression, '<actual-portable-to-deb-receipt>', 'eval'),
        {'__builtins__': {}, 'dict': dict, **constants},
        {'expected': expected, 'old': {'manifest': {'executable_sha256': 'c' * 64, 'source_sha256': 'd' * 64}}})


class LinuxFacts(unittest.TestCase):
    def setUp(self):
        self.f = test_final_release.Composition(methodName='runTest')
        self.f.setUp()
        self.addCleanup(self.f.doCleanups)
        f = self.f
        self.serial = 0
        portable = next(row for row in f.inventory['assets'] if row.get('platform') == 'Linux' and row['file'].endswith('.zip'))
        deb = next(row for row in f.inventory['assets'] if row['file'].endswith('.deb'))
        self.q = dict(commit=f.fixture.commit, version='0.2.0', portable=f.directory / portable['file'],
            portable_sha256=portable['sha256'], package=f.directory / deb['file'], package_sha256=deb['sha256'],
            maintainer=f.options['maintainer'])
        self.upgrade = f.fixture.base / 'private-portable-to-deb.json'
        self.upgrade.write_bytes(qualifier.encode(driver_receipt(self.q)) + b'\n')
        self.report_dir = f.fixture.base / 'linux-report'
        self.report_path = self.report_dir / qualifier.REPORT

    def qualify(self):
        return qualifier.qualify(self.f.root, self.q, self.f.receipt, sha(self.f.receipt.read_bytes()),
                                 self.upgrade, sha(self.upgrade.read_bytes()), self.report_dir)

    def trust(self):
        service = service_fixtures.Readback(methodName='runTest')
        service.setUp()
        self.addCleanup(service.doCleanups)
        workflow, identity, filename, _, _, runner, step, _ = shared._policy(False, True)
        service.run['head_sha'] = self.f.fixture.commit
        service.run['head_commit']['id'] = self.f.fixture.commit
        service.run['path'] = workflow
        for job in service.jobs['jobs']:
            job['head_sha'] = self.f.fixture.commit
            job['labels'] = [runner]
            for row in job.get('steps', []):
                if row['name'] == 'Qualify selected final DMG':
                    row['name'] = step
                if row['name'] == 'Acquire selected signed assets':
                    row['name'] = 'Acquire selected Linux assets'
        self.crypto_calls = []
        def command(executable, arguments, work, label):
            self.crypto_calls.append(arguments)
            if label == 'version':
                return b'gh version 2.102.0 (2026-09-30)\nhttps://github.com/cli/cli/releases/tag/v2.102.0\n'
            values = crypto_fixtures.verified(self.f.fixture.commit, sha(self.report_path.read_bytes()))
            result = values[0]['verificationResult']
            for key in ['subjectAlternativeName', 'buildSignerURI', 'buildConfigURI']:
                result['signature']['certificate'][key] = identity
            result['statement']['subject'][0]['name'] = filename
            return json.dumps(values).encode()
        self.command = command
        trust = dict(service.trust.values, workflow_commit=self.f.fixture.commit, readback_job=True)
        selected = dict(qualification=self.report_path, qualification_sha256=sha(self.report_path.read_bytes()),
                        upgrade=self.upgrade, upgrade_sha256=sha(self.upgrade.read_bytes()), trust=trust)
        return service, selected

    def compose(self, service, selected, *, via_selection=False):
        self.serial += 1
        options = copy.deepcopy(self.f.options)
        options['macos']['trust']['work'] = self.f.fixture.base / ('linux-compose-mac-' + str(self.serial))
        options['linux_native'] = copy.deepcopy(selected)
        options['linux_native']['trust']['work'] = self.f.fixture.base / ('linux-compose-origin-' + str(self.serial))
        self.output = self.f.fixture.base / ('linux-final-' + str(self.serial))
        real = shared.authenticate
        def route(qualification, **inputs):
            if inputs.get('_linux') is True:
                return real(qualification, **inputs)
            work = Path(inputs['work'])
            work.mkdir(mode=0o700)
            for key, name in [('bundle', 'bundle.jsonl'), ('trusted_root', 'trusted_root.jsonl'), ('cli', 'gh')]:
                (work / name).write_bytes(Path(inputs[key]).read_bytes())
            (work / 'dmg-qualification.json').write_bytes(qualification)
            return self.f.origin
        with mock.patch.object(shared, 'authenticate', side_effect=route), \
                mock.patch.object(shared.sys, 'platform', 'linux'), mock.patch.object(shared.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(shared, '_command', side_effect=self.command), \
                mock.patch.object(website, 'public_get', side_effect=service.get):
            if via_selection:
                path = self.f.fixture.base / ('linux-final-selection-' + str(self.serial) + '.json')
                path.write_text(json.dumps(options, default=str), encoding='utf-8')
                return final.from_selection(self.f.root, self.f.directory, self.output, path, sha(path.read_bytes()))
            return final.compose(self.f.root, self.f.directory, self.output, **options)

    def test_actual_two_receipt_qualification_and_authenticated_composition_keep_every_installed_gate(self):
        value = self.qualify()
        self.assertEqual(value['installation']['receipt']['limits'], final.readiness.DEB_INSTALLATION_LIMITS)
        self.assertEqual(value['portable_to_deb']['receipt'], driver_receipt(self.q))
        self.assertEqual(value['installation']['receipt']['limits']['real-application-version-migration'], 'not-performed')
        self.assertIn('opt-in-per-user-migration-and-default-restart', value['portable_to_deb']['receipt']['checks'])
        self.assertNotIn(str(self.f.fixture.base), self.report_path.read_text())
        service, selection = self.trust()
        candidate = self.compose(service, selection, via_selection=True)
        self.assertEqual(len(service.calls), 2)
        self.assertEqual(self.crypto_calls[-1][self.crypto_calls[-1].index('--cert-identity') + 1], shared._policy(False, True)[1])
        self.assertIn('--deny-self-hosted-runners', self.crypto_calls[-1])
        self.assertNotIn('linux-installed-evidence-producer-not-authenticated', candidate.blockers)
        self.assertEqual(candidate.assessment['linux_native_origin']['native_service']['runner_label'], 'ubuntu-24.04')
        self.assertEqual(candidate.assessment['linux_installation_provenance'], 'verified-pinned-workflow-oidc-attestation-and-successful-job')
        self.assertIn('linux-installed-distribution-and-update-owner-not-verified', candidate.blockers)
        self.assertIn('installed-upgrade-interruption-and-visible-native-acceptance-not-established', candidate.blockers)
        for gate in ['native-shlibdeps-requires-trusted-native-build', 'publisher-signature-not-verified',
                     'update-repository-not-configured', 'native-gui-not-verified']:
            self.assertIn(gate, candidate.assessment['linux_bound_installation']['blockers'])
        self.assertFalse(candidate.assessment['production_ready'])
        for channel in candidate.manifest['channels'].values():
            self.assertFalse(channel['ready'])
        self.assertEqual(candidate.manifest['channels']['microsoft-store'], final.store_channel())
        self.assertEqual(candidate.manifest['channels']['linux-direct']['installed_acceptance'], 'unverified')
        names = {row['file'] for row in candidate.manifest['files']}
        self.assertFalse(names & {self.upgrade.name, self.report_path.name, self.f.receipt.name})
        candidate.verify()
        client = mock.Mock()
        self.assertEqual(final.store_channel()['submission_enabled'], False)
        import publish_release
        self.assertEqual(publish_release.publish(candidate, client, 'fixture', execute=True)['phase'], 'blocked')
        client.assert_not_called()

    def test_distinct_receipt_limits_and_extra_private_fields_cannot_be_relabelled(self):
        raw = self.upgrade.read_bytes()
        changes = [('old_commit', 'b' * 40), ('old_portable_sha256', 'e' * 64), ('package_sha256', 'e' * 64),
                   ('commit', 'b' * 40), ('ready', True), ('schema', True), ('old_source_sha256', 'not-a-hash'),
                   ('limits', {}), ('PRIVATE_CANARY', 'DO_NOT_COPY')]
        for key, value in changes:
            with self.subTest(field=key):
                self.upgrade.write_bytes(qualifier.encode(dict(json.loads(raw), **{key: value})))
                with self.assertRaises(ValueError):
                    self.qualify()
                self.assertFalse(self.report_dir.exists())
        self.upgrade.write_bytes(raw)
        installed = self.f.receipt.read_bytes()
        changed = json.loads(installed)
        changed['limits']['real-application-version-migration'] = 'passed'
        self.f.receipt.write_bytes(qualifier.encode(changed))
        with self.assertRaises(ValueError):
            self.qualify()
        self.assertFalse(self.report_dir.exists())

    def test_actual_report_and_receipt_pins_are_required_before_crypto_and_output(self):
        self.qualify()
        self.report_path.chmod(0o600)
        service, selection = self.trust()
        original = self.report_path.read_bytes()
        for field, value in [('ready', True), ('schema', True), ('maintainer', 'PRIVATE_CANARY'), ('portable_sha256', 'e' * 64)]:
            changed = dict(json.loads(original), **{field: value})
            self.report_path.write_bytes(qualifier.encode(changed))
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.compose(service, dict(selection, qualification_sha256=sha(self.report_path.read_bytes())))
            self.assertFalse(self.output.exists())
        self.report_path.write_bytes(original)
        with self.assertRaises(ValueError):
            self.compose(service, dict(selection, upgrade_sha256='0' * 64))
        self.assertEqual(self.crypto_calls, [])
        self.assertEqual(service.calls, [])

    def test_source_package_pins_and_bounded_strict_json_refuse_before_a_report(self):
        checked = qualifier.inputs(self.f.root, self.q)
        self.assertEqual(checked['record']['sha256'], self.q['package_sha256'])
        for key, value in [('package_sha256', '0' * 64), ('portable_sha256', '0' * 64),
                           ('commit', 'b' * 40), ('PRIVATE_CANARY', 'unreviewed')]:
            with self.subTest(field=key), self.assertRaises(ValueError):
                qualifier.inputs(self.f.root, dict(self.q, **{key: value}))
        package = Path(self.q['package'])
        raw = package.read_bytes()
        package.write_bytes(raw + b'CHANGED_CODE_OR_SOURCE')
        with self.assertRaises(ValueError): self.qualify()
        self.assertFalse(self.report_dir.exists())
        package.write_bytes(raw)
        raw = self.upgrade.read_bytes()
        for data in [b'x' * (final.readiness.MAX_INSTALLATION_RECEIPT_BYTES + 1), b'\xff', b'{',
                     raw.replace(b'"schema":', b'"schema":1,"schema":')]:
            self.upgrade.write_bytes(data)
            with self.assertRaises((ValueError, UnicodeError)): self.qualify()
            self.assertFalse(self.report_dir.exists())
        self.upgrade.write_bytes(raw)
        tracked = self.f.root / 'src/lib.rs'
        raw = tracked.read_bytes()
        tracked.write_bytes(raw + b'// changed tracked input\n')
        with self.assertRaises(ValueError): self.qualify()
        self.assertFalse(self.report_dir.exists())
        tracked.write_bytes(raw)

    def test_crypto_failure_wrong_workflow_runner_attempt_and_failed_steps_never_authenticate(self):
        self.qualify()
        service, selection = self.trust()
        command = self.command
        self.command = mock.Mock(side_effect=ValueError('PRIVATE_CRYPTO_FAILURE'))
        with self.assertRaises(ValueError):
            self.compose(service, selection)
        self.assertEqual(service.calls, [])
        self.command = command
        original_run, original_jobs = copy.deepcopy(service.run), copy.deepcopy(service.jobs)
        for mutation in ['workflow', 'attempt', 'runner', 'step', 'step-order', 'missing-step']:
            service.run, service.jobs = copy.deepcopy(original_run), copy.deepcopy(original_jobs)
            if mutation == 'workflow': service.run['path'] = '.github/workflows/release.yml'
            if mutation == 'attempt': service.run['run_attempt'] = 2
            if mutation == 'runner': service.jobs['jobs'][1]['labels'] += ['self-hosted']
            steps = service.jobs['jobs'][1]['steps']
            if mutation == 'step': next(row for row in steps if row['name'] == 'Qualify selected final Linux DEB')['conclusion'] = 'skipped'
            if mutation == 'step-order': next(row for row in steps if row['name'] == 'Attest generated native qualification')['number'] += 10
            if mutation == 'missing-step': steps[:] = [row for row in steps if row['name'] != 'Upload qualification evidence']
            with self.subTest(mutation=mutation), self.assertRaisesRegex(ValueError, 'successful exact-attempt'):
                self.compose(service, selection)
            self.assertFalse(self.output.exists())

    def test_changed_original_or_owned_linux_evidence_refuses_and_preserves_private_canaries(self):
        self.qualify()
        service, selection = self.trust()
        original = self.command
        raw = self.upgrade.read_bytes()
        def changed(tool, arguments, work, label):
            result = original(tool, arguments, work, label)
            if label == 'verification':
                self.upgrade.write_bytes(b'PRIVATE_CANARY_CHANGED_RECEIPT')
            return result
        self.command = changed
        with self.assertRaisesRegex(ValueError, 'native evidence changed'):
            self.compose(service, selection)
        self.assertFalse(self.output.exists())
        self.upgrade.write_bytes(raw)
        self.command = original
        candidate = self.compose(service, selection)
        for path in candidate.directory.iterdir():
            self.assertNotIn(b'PRIVATE_CANARY', path.read_bytes())
        with self.assertRaises(ValueError):
            self.qualify()
        self.assertEqual(self.report_path.read_bytes(), qualifier.encode(json.loads(self.report_path.read_bytes())) + b'\n')

    def test_legacy_selection_remains_blocked_and_linux_adapter_cannot_select_another_platform(self):
        candidate = self.f.compose()
        self.assertIn('linux-installed-evidence-producer-not-authenticated', candidate.blockers)
        self.assertEqual(candidate.assessment['linux_installation_provenance'], 'requires-trusted-same-job-producer')
        for options in [{'_windows': True}, {'_linux': False}]:
            with mock.patch.object(shared, 'authenticate') as authenticate, self.assertRaises(ValueError):
                native.authenticate(b'{}', **options)
            authenticate.assert_not_called()
        for options in [(True, True), (False, 'true')]:
            with self.assertRaises(ValueError): shared._policy(*options)
        path = self.f.fixture.base / 'strict-final-selection.json'
        for value in [dict(self.f.options, linux_native=None), dict(self.f.options, PRIVATE_CANARY=True)]:
            path.write_text(json.dumps(value, default=str), encoding='utf-8')
            with mock.patch.object(final, 'compose') as compose, self.assertRaises(ValueError):
                final.from_selection(self.f.root, self.f.directory, self.f.fixture.base / 'not-created', path, sha(path.read_bytes()))
            compose.assert_not_called()

    def test_owned_receipt_change_during_public_copy_refuses_and_keeps_failed_partial_evidence(self):
        self.qualify()
        service, selection = self.trust()
        original = final.bounded_file
        fired = []
        def changed(path, limit, destination=None):
            result = original(path, limit, destination)
            if destination is not None and Path(destination).parent == self.output and not fired:
                owned = self.f.fixture.base / ('linux-compose-origin-' + str(self.serial)) / 'installation.json'
                owned.chmod(0o600)
                owned.write_bytes(b'PRIVATE_CANARY_OWNED_RECEIPT_CHANGED')
                fired.append(True)
            return result
        with mock.patch.object(final, 'bounded_file', side_effect=changed), self.assertRaisesRegex(ValueError, 'binding input changed'):
            self.compose(service, selection)
        self.assertEqual(fired, [True])
        self.assertTrue(self.output.is_dir())
        for path in self.output.iterdir():
            self.assertNotIn(b'PRIVATE_CANARY', path.read_bytes())

    def test_actual_acquisition_binds_two_exact_artifacts_and_corresponding_source_without_native_execution(self):
        q = {key: value.name if isinstance(value, Path) else value for key, value in self.q.items()}
        source = f"PeerBrush-{q['version']}-source.zip"
        originals = {q['portable']: Path(self.q['portable']).read_bytes(), source: (self.f.directory / source).read_bytes()}
        files = {q['package']: Path(self.q['package']).read_bytes(), **{q['package'] + suffix: b'UNTRUSTED_SIDECAR' for suffix in ['.json', '.sha256', '.qualification.json']}}
        zipped = lambda rows: transport_fixtures.zipped([(name, value, 0o100644) for name, value in rows.items()])
        wrappers = {'201': zipped(originals), '202': zipped(files)}
        repository = dict(id=acquisition.transport.REPOSITORY_ID, full_name=acquisition.transport.REPOSITORY,
                          owner=dict(id=acquisition.transport.OWNER_ID), fork=False, private=False)
        run = dict(id=101, run_attempt=1, status='completed', conclusion='success', event='workflow_dispatch',
            path='.github/workflows/release.yml', head_branch='main', head_sha=q['commit'], pull_requests=[],
            repository=repository, head_repository=copy.deepcopy(repository))
        artifacts = {}
        for identifier, name in [('201', 'candidate-Linux'), ('202', 'DEB-installation-candidate')]:
            artifacts[identifier] = dict(id=int(identifier), name=name, expired=False,
                digest='sha256:' + sha(wrappers[identifier]), size_in_bytes=len(wrappers[identifier]),
                url='https://api.github.com' + acquisition.transport.API + 'artifacts/' + identifier,
                archive_download_url='https://api.github.com' + acquisition.transport.API + 'artifacts/' + identifier + '/zip',
                workflow_run=dict(id=101, repository_id=acquisition.transport.REPOSITORY_ID,
                    head_repository_id=acquisition.transport.REPOSITORY_ID, head_branch='main', head_sha=q['commit']))
        calls = []
        def get(host, target, limit, **options):
            calls.append((host, target))
            if host == 'api.github.com':
                self.assertEqual(options['token'], 'SYNTHETIC_READ_TOKEN')
                if target.endswith('/zip'): return 'https://fixture.blob.core.windows.net/' + target.split('/')[-2]
                return json.dumps(run if '/runs/' in target else artifacts[target.rsplit('/', 1)[1]]).encode()
            self.assertNotIn('token', options)
            with options['destination'].open('xb') as stream: stream.write(wrappers[target[1:]])
        selected = dict(commit=q['commit'], qualification=q, transport=dict(run_id='101', portable_artifact_id='201',
            portable_artifact_sha256=sha(wrappers['201']), deb_artifact_id='202', deb_artifact_sha256=sha(wrappers['202']),
            source_sha256=sha(originals[source]), deb_files={name: sha(value) for name, value in files.items()}))
        environment = dict(GITHUB_REPOSITORY=acquisition.transport.REPOSITORY, GITHUB_REF='refs/heads/main',
            GITHUB_REPOSITORY_ID=str(acquisition.transport.REPOSITORY_ID), GITHUB_REPOSITORY_OWNER_ID=str(acquisition.transport.OWNER_ID),
            GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=acquisition.WORKFLOW, GITHUB_SHA=q['commit'],
            GITHUB_WORKFLOW_SHA=q['commit'], GITHUB_RUN_ATTEMPT='1', RUNNER_ENVIRONMENT='github-hosted',
            RUNNER_OS='Linux', RUNNER_ARCH='X64', PEERBRUSH_LINUX_QUALIFICATION_ENABLED='true')
        output = self.f.fixture.base / 'linux-acquired'
        with mock.patch.object(acquisition.transport, 'request', side_effect=get):
            digest = acquisition.acquire(self.f.root, output, selected, token='SYNTHETIC_READ_TOKEN', environment=environment)
        self.assertEqual(sha((output / 'qualification-selection.json').read_bytes()), digest)
        self.assertEqual(len(calls), 12)
        for name, value in originals.items(): self.assertEqual((output / 'original' / name).read_bytes(), value)
        self.assertEqual((output / 'media' / q['package']).read_bytes(), files[q['package']])
        with mock.patch.object(acquisition.transport, 'request') as request, self.assertRaises(ValueError):
            acquisition.acquire(self.f.root, self.f.fixture.base / 'not-created', selected, token='SYNTHETIC_READ_TOKEN', environment={})
        request.assert_not_called()
        artifacts['202']['name'] = 'DEB-forged-ready'
        with mock.patch.object(acquisition.transport, 'request', side_effect=get), self.assertRaises(ValueError):
            acquisition.acquire(self.f.root, self.f.fixture.base / 'wrong-artifact', selected, token='SYNTHETIC_READ_TOKEN', environment=environment)
        self.assertFalse((self.f.fixture.base / 'wrong-artifact/qualification-selection.json').exists())

    def test_disposable_workflow_commands_satisfy_actual_driver_guards_and_selected_filename(self):
        source = (ROOT / '.github/workflows/linux-qualification.yml').read_text(encoding='utf-8')
        commands = re.findall(r"docker run --rm (.*?)ubuntu:24\.04 bash -euc '(.*?)'", source, re.S)
        self.assertEqual(len(commands), 2)
        checked = qualifier.inputs(self.f.root, self.q)
        expected = dict(version=self.q['version'], commit=self.q['commit'],
            package_sha256=self.q['package_sha256'], executable_sha256=checked['executable_sha256'],
            source_sha256=checked['record']['source_sha256'])
        class GuardPassed(Exception):
            pass
        for index, (prefix, body) in enumerate(commands):
            tokens = shlex.split(prefix.replace('\\\n', ' '))
            environment = dict(tokens[i + 1].split('=', 1) for i, token in enumerate(tokens) if token == '-e')
            self.assertEqual(environment.get('PEERBRUSH_DISPOSABLE_PACKAGE_TEST'), '1')
            self.assertEqual(environment.get('PEERBRUSH_DISPOSABLE_REAL_UPGRADE_TEST'), '1' if index else None)
            self.assertNotIn('PEERBRUSH_DISPOSABLE_INSTALL_TEST', environment)
            with mock.patch.dict(os.environ, environment, clear=True), \
                    mock.patch.object(installation.sys, 'platform', 'linux'), \
                    mock.patch.object(installation.os, 'geteuid', return_value=0, create=True), \
                    mock.patch.object(installation.pathlib.Path, 'is_file', return_value=True), \
                    mock.patch.object(installation.pathlib.Path, 'read_text', return_value='ID=ubuntu\nVERSION_ID="24.04"\n'):
                installation.require_disposable()
                if index:
                    with mock.patch.object(upgrade, 'phase', side_effect=GuardPassed) as phase, self.assertRaises(GuardPassed):
                        upgrade.check(Path('/old-source'), Path('/input/unused'), expected, Path('/evidence/unused'))
                    phase.assert_called_once_with('current-package')
                    with mock.patch.dict(os.environ, {'PEERBRUSH_DISPOSABLE_REAL_UPGRADE_TEST': ''}), \
                            self.assertRaisesRegex(ValueError, 'additional explicit'):
                        upgrade.check(Path('/old-source'), Path('/input/unused'), expected, Path('/evidence/unused'))
                with mock.patch.dict(os.environ, {'PEERBRUSH_DISPOSABLE_PACKAGE_TEST': ''}), self.assertRaises(ValueError):
                    installation.require_disposable()
            mount = next(tokens[i + 1] for i, token in enumerate(tokens) if token == '--mount' and 'src=$DEB,' in tokens[i + 1])
            destination = dict(item.split('=', 1) for item in mount.split(',') if '=' in item)['dst']
            arguments = shlex.split(body.replace('\\\n', ' '))
            selected = arguments[arguments.index('--package') + 1]
            self.assertEqual(destination, selected)
            canonical = f"peerbrush_{qualifier.deb_version(self.q['version'])}_amd64.deb"
            self.assertEqual(Path(selected.replace('${VERSION}', self.q['version'])).name, canonical)
            owned = self.f.fixture.base / ('driver-command-' + str(index))
            owned.mkdir()
            package = owned / canonical
            package.write_bytes(Path(self.q['package']).read_bytes())
            fields = dict(Package='peerbrush', Architecture='amd64', Version=qualifier.deb_version(self.q['version']))
            with mock.patch.object(installation, 'run', side_effect=lambda command: fields[command[-1]]):
                installation.selected_package(package, expected)
            renamed = owned / 'peerbrush.deb'
            renamed.write_bytes(package.read_bytes())
            with mock.patch.object(installation, 'run') as native, self.assertRaisesRegex(ValueError, 'filename or bytes'):
                installation.selected_package(renamed, expected)
            native.assert_not_called()

    def test_dormant_workflow_guards_before_checkout_and_attests_only_after_both_actual_drivers(self):
        source = (ROOT / '.github/workflows/linux-qualification.yml').read_text(encoding='utf-8')
        self.assertEqual(source.count('if: ${{ false }}'), 2)
        self.assertEqual(source.count('runs-on: ubuntu-24.04'), 2)
        guards = re.findall(r"python3 -I - <<'PY'\n(.*?)\n          PY", source, re.S)
        required = dict(GITHUB_REPOSITORY='Deftr0y/PeerBrush', GITHUB_REF='refs/heads/main',
            GITHUB_REPOSITORY_ID='1407648785', GITHUB_REPOSITORY_OWNER_ID='21202494', GITHUB_EVENT_NAME='workflow_dispatch',
            GITHUB_WORKFLOW_REF=acquisition.WORKFLOW, GITHUB_SHA='a' * 40, GITHUB_WORKFLOW_SHA='a' * 40,
            GITHUB_RUN_ATTEMPT='1', RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Linux', RUNNER_ARCH='X64',
            PEERBRUSH_LINUX_QUALIFICATION_ENABLED='true', SELECTED_COMMIT='a' * 40)
        invocation = [textwrap.dedent(value) for value in guards if 'Require reviewed exact main' in value]
        self.assertEqual(len(invocation), 2)
        for guard in invocation:
            with mock.patch.dict(os.environ, required, clear=True): exec(compile(guard, '<fixed-main-guard>', 'exec'), {})
            for key, value in [('GITHUB_REF', 'refs/pull/1/merge'), ('GITHUB_WORKFLOW_SHA', 'b' * 40),
                               ('GITHUB_RUN_ATTEMPT', '2'), ('RUNNER_OS', 'Windows'), ('PEERBRUSH_LINUX_QUALIFICATION_ENABLED', 'false')]:
                with self.subTest(field=key), mock.patch.dict(os.environ, dict(required, **{key: value}), clear=True), self.assertRaises(SystemExit):
                    exec(compile(guard, '<fixed-main-guard>', 'exec'), {})
        for section in [source.split('  qualify:')[0], source.split('  qualify:')[1]]:
            self.assertLess(section.index('Require reviewed main invocation'), section.index('Checkout reviewed main source'))
        self.assertLess(source.index('/checks/test_deb_installation.py'), source.index('/checks/test_deb_version_upgrade.py'))
        self.assertLess(source.index('/checks/test_deb_version_upgrade.py'), source.index('scripts/qualify_linux_deb.py --source-root'))
        self.assertIn('      - name: Attest generated native qualification', source)
        self.assertNotIn('contents: write', source)
        inventory = json.loads((ROOT / 'scripts/package-files.json').read_bytes())['source']
        for path in ['scripts/qualify_linux_deb.py', 'scripts/acquire_linux_qualification_inputs.py',
                     'scripts/verify_linux_qualification.py', 'scripts/test_linux_qualification.py', '.github/workflows/linux-qualification.yml']:
            self.assertIn(path, inventory)


if __name__ == '__main__':
    unittest.main()
