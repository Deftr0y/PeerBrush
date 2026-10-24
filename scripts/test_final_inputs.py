"""Real composition/transport consumers with fake process and public GET seams only."""
import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import unittest
from unittest import mock
import zipfile

import acquire_final_inputs as acquisition
import assemble_final_inputs as upstream
import acquire_macos_qualification_inputs as transport
import final_release as final
import prepare_final_inputs as inputs
import prepare_site_release as website
import test_final_release
import test_macos_qualification_attestation as crypto
import test_macos_qualification_job as jobs
import test_macos_qualification_producer as wrappers
import test_windows_qualification
import test_linux_qualification
import qualify_linux_deb
import verify_release
import verify_macos_qualification as native


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Handoff(unittest.TestCase):
    def setUp(self):
        self.fixture = test_final_release.Composition(methodName='runTest')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.service = jobs.Readback(methodName='runTest')
        self.service.setUp()
        self.addCleanup(self.service.doCleanups)
        f, service = self.fixture, self.service
        service.run['head_sha'] = service.run['head_commit']['id'] = f.fixture.commit
        for job in service.jobs['jobs']:
            job['head_sha'] = f.fixture.commit
        selected = copy.deepcopy(f.options)
        selected['macos']['trust'] = dict(service.trust.values, workflow_commit=f.fixture.commit, readback_job=True)
        linux = f.fixture.base / 'selected-linux'
        linux.mkdir()
        receipt = linux / 'installation.json'
        receipt.write_bytes(f.receipt.read_bytes())
        selected['linux_receipt'] = receipt
        selectors = f.fixture.base / 'selectors'
        selectors.mkdir()
        self.selection = selectors / 'selection.json'
        self.selection.write_bytes(json.dumps(selected, default=str, sort_keys=True).encode())
        self.selection_sha256 = sha(self.selection.read_bytes())
        self.output, self.work = f.fixture.base / 'data-artifact', f.fixture.base / 'producer-work'
        self.failed_crypto = False
        self.commands = []

    @contextlib.contextmanager
    def seams(self):
        def command(executable, arguments, work, label):
            self.commands.append((arguments, label))
            if label == 'version':
                return b'gh version 2.102.0 (2026-09-30)\nhttps://github.com/cli/cli/releases/tag/v2.102.0\n'
            if self.failed_crypto:
                raise ValueError('Synthetic cryptographic verifier refusal')
            return json.dumps(crypto.verified(self.fixture.fixture.commit,
                self.fixture.options['macos']['qualification_sha256'])).encode()
        with mock.patch.object(native.sys, 'platform', 'linux'), \
                mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(native, '_command', side_effect=command), \
                mock.patch.object(website, 'public_get', side_effect=self.service.get):
            yield

    def prepare(self):
        with self.seams():
            return inputs.prepare(self.fixture.root, self.fixture.directory, self.selection,
                                  self.selection_sha256, self.output, self.work)

    def artifact(self, index):
        body = {name: (self.output / name).read_bytes() for name in [inputs.ARCHIVE, inputs.INDEX]}
        wrapper = wrappers.zipped([(name, data, stat.S_IFREG | 0o400) for name, data in body.items()])
        repo = dict(id=transport.REPOSITORY_ID, full_name=transport.REPOSITORY,
                    fork=False, private=False, owner=dict(id=transport.OWNER_ID))
        commit = self.fixture.fixture.commit
        run = dict(id=202, run_attempt=1, status='completed', conclusion='success', event='workflow_dispatch',
                   head_branch='main', head_sha=commit, path=inputs.WORKFLOW, pull_requests=[],
                   repository=copy.deepcopy(repo), head_repository=copy.deepcopy(repo), actor='PRIVATE_CANARY')
        predecessor = dict(run, id=101, path='.github/workflows/release.yml')
        artifact = dict(id=303, name='final-inputs-202-1', expired=False, digest='sha256:' + sha(wrapper),
            size_in_bytes=len(wrapper), url='https://api.github.com' + transport.API + 'artifacts/303',
            archive_download_url='https://api.github.com' + transport.API + 'artifacts/303/zip',
            workflow_run=dict(id=202, repository_id=transport.REPOSITORY_ID,
                              head_repository_id=transport.REPOSITORY_ID, head_branch='main', head_sha=commit))
        values = dict(commit=commit, candidate_run_id='101', run_id='202', artifact_id='303',
            artifact_sha256=sha(wrapper), bundle_sha256=sha(body[inputs.ARCHIVE]),
            index_sha256=sha(body[inputs.INDEX]), selection_sha256=index['selection_sha256'])
        calls = []
        def request(host, target, limit, **options):
            calls.append((host, target, options.get('token')))
            if options.get('redirect'):
                self.assertEqual(host, 'api.github.com')
                return 'https://results-receiver.actions.githubusercontent.com/selected-data'
            if options.get('destination') is not None:
                self.assertIsNone(options.get('token'))
                options['destination'].write_bytes(wrapper)
                return options['destination']
            if target == transport.API + 'runs/101': value = predecessor
            elif target == transport.API + 'runs/202': value = run
            elif target == transport.API + 'artifacts/303': value = artifact
            else: raise AssertionError('Unexpected data acquisition GET')
            return json.dumps(value).encode()
        return values, run, artifact, calls, request

    def test_actual_producer_acquisition_and_relocated_consumers_keep_all_native_and_readiness_gates(self):
        for parent in [self.fixture.directory, self.fixture.direct, self.selection.parent]:
            (parent / 'private-recovery.json').write_bytes(b'PRIVATE_CANARY')
        index = self.prepare()
        self.assertEqual({path.name for path in self.output.iterdir()}, {inputs.ARCHIVE, inputs.INDEX})
        with zipfile.ZipFile(self.output / inputs.ARCHIVE) as archive:
            self.assertEqual(set(archive.namelist()), {record['path'] for record in index['files']})
            for name in archive.namelist():
                self.assertNotIn(b'PRIVATE_CANARY', archive.read(name))
            portable = json.loads(archive.read('selection.json'))
        self.assertEqual(portable['macos']['trust']['work'], '@consumer/macos')
        self.assertNotIn(str(self.fixture.fixture.base), json.dumps(portable))
        values, _, _, _, request = self.artifact(index)
        owned = self.fixture.fixture.base / 'acquired'
        with mock.patch.object(transport, 'request', side_effect=request):
            actual = acquisition.acquire(self.fixture.root, owned, **values, token='SYNTHETIC_JOB_READ_TOKEN')
        self.assertEqual(actual, index)
        count = len(self.commands)
        with self.seams():
            candidate = inputs.consume(self.fixture.root, owned / 'data', self.fixture.fixture.base / 'consumer-final',
                self.fixture.fixture.base / 'consumer-work', index, index['selection_sha256'])
        self.assertGreater(len(self.commands), count, 'Acquired metadata cannot replace native verification')
        self.assertIn('--deny-self-hosted-runners', self.commands[-1][0])
        self.assertFalse(candidate.assessment['production_ready'])
        self.assertTrue(candidate.blockers)
        self.assertIn('linux-installed-evidence-producer-not-authenticated', candidate.blockers)
        self.assertEqual(candidate.manifest['channels']['microsoft-store'], final.store_channel())
        self.assertTrue(all(channel['ready'] is False for channel in candidate.manifest['channels'].values()))
        for path in candidate.directory.iterdir():
            self.assertNotIn(b'PRIVATE_CANARY', path.read_bytes())

    def test_missing_extra_tampered_aliased_and_oversized_data_refuse_before_native_work(self):
        index = self.prepare()
        data = self.work / 'data'
        before_commands = len(self.commands)
        selected = data / 'linux/installation.json'
        original = selected.read_bytes()
        selected.chmod(0o600)
        selected.unlink()
        with self.assertRaises(ValueError): inputs.verify_data(self.fixture.root, data, index, index['selection_sha256'])
        selected.write_bytes(original)
        extra = data / 'private.txt'
        extra.write_bytes(b'PRIVATE_CANARY')
        with self.assertRaises(ValueError): inputs.verify_data(self.fixture.root, data, index, index['selection_sha256'])
        extra.unlink()
        selected.write_bytes(b'changed evidence')
        with self.assertRaises(ValueError): inputs.verify_data(self.fixture.root, data, index, index['selection_sha256'])
        selected.write_bytes(original)
        # Keep the closed inventory intact so rejection exercises nlink itself.
        linked = self.fixture.fixture.base / 'outside-data-hardlink'
        os.link(selected, linked)
        try:
            with self.assertRaises(ValueError): inputs.verify_data(self.fixture.root, data, index, index['selection_sha256'])
        finally:
            linked.unlink()
        changed = copy.deepcopy(index)
        changed['files'][0]['bytes'] = final.LIMIT + 1
        with self.assertRaises(ValueError): inputs.checked_index(changed, index['commit'], index['selection_sha256'])
        for name in ['../escape', 'base/stream:private', 'base/NUL', 'base/alias.', 'base/file/../escape']:
            changed = copy.deepcopy(index)
            changed['files'][0]['path'] = name
            with self.subTest(path=name), self.assertRaises(ValueError):
                inputs.checked_index(changed, index['commit'], index['selection_sha256'])
        self.assertEqual(len(self.commands), before_commands)

    def test_actual_producer_reserves_selection_and_refuses_crossing_copy_before_destination(self):
        self.prepare()
        selection_bytes = (self.work / 'data/selection.json').stat().st_size
        first = self.fixture.directory / 'release-inventory.json'
        crossing = self.fixture.directory / 'SHA256SUMS'
        budget = selection_bytes + first.stat().st_size + crossing.stat().st_size - 1
        self.work = self.fixture.fixture.base / 'small-budget-work'
        self.output = self.fixture.fixture.base / 'small-budget-output'
        before_commands = len(self.commands)
        with mock.patch.object(inputs, 'MAX_TOTAL', budget), self.assertRaises(ValueError):
            self.prepare()
        retained = self.work / 'data/base/release-inventory.json'
        self.assertEqual(retained.read_bytes(), first.read_bytes())
        self.assertFalse(retained.stat().st_mode & stat.S_IWUSR)
        self.assertFalse((self.work / 'data/base/SHA256SUMS').exists())
        self.assertFalse((self.work / 'data/selection.json').exists())
        self.assertFalse((self.work / 'composed').exists())
        self.assertFalse(self.output.exists())
        self.assertEqual(len(self.commands), before_commands)
        original = retained.read_bytes()
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertEqual(retained.read_bytes(), original)

    def test_inner_zip_requires_exact_regular_unmodified_selected_members(self):
        index = self.prepare()
        with zipfile.ZipFile(self.output / inputs.ARCHIVE) as archive:
            members = [(name, archive.read(name), stat.S_IFREG | 0o400) for name in archive.namelist()]
        changes = [members[:-1], members + [('private.txt', b'PRIVATE_CANARY', stat.S_IFREG | 0o400)],
                   [(name, data, stat.S_IFLNK | 0o777) for name, data, _ in members],
                   [(name, b'changed' if n == 0 else data, mode) for n, (name, data, mode) in enumerate(members)],
                   members + [members[0]]]
        before_commands = len(self.commands)
        for number, changed in enumerate(changes):
            archive = self.fixture.fixture.base / ('invalid-' + str(number) + '.zip')
            archive.write_bytes(wrappers.zipped(changed))
            with self.subTest(case=number), self.assertRaises(ValueError):
                acquisition.extract(self.fixture.root, archive, self.fixture.fixture.base / ('extract-' + str(number)),
                                    index, index['selection_sha256'])
        self.assertEqual(len(self.commands), before_commands)

    def test_unknown_private_selection_fields_are_rejected_before_output_and_cli_success_stays_two(self):
        original = self.selection.read_bytes()
        selected = json.loads(original)
        selected['private_configuration'] = 'PRIVATE_CANARY'
        self.selection.write_bytes(final.encode(selected))
        with self.assertRaises(ValueError):
            inputs.prepare(self.fixture.root, self.fixture.directory, self.selection,
                sha(self.selection.read_bytes()), self.output, self.work)
        self.assertFalse(self.work.exists())
        self.assertFalse(self.output.exists())
        self.selection.write_bytes(original)
        index = self.prepare()
        with self.seams(), contextlib.redirect_stdout(io.StringIO()) as output:
            result = inputs.main(['consume', '--source-root', str(self.fixture.root),
                '--directory', str(self.work / 'data'), '--selection', str(self.work / 'data/selection.json'),
                '--selection-sha256', index['selection_sha256'], '--index', str(self.output / inputs.INDEX),
                '--index-sha256', sha((self.output / inputs.INDEX).read_bytes()),
                '--work', str(self.fixture.fixture.base / 'cli-work'),
                '--output', str(self.fixture.fixture.base / 'cli-output')])
        self.assertEqual(result, 2)
        self.assertIn('readiness remain blocked', output.getvalue())

    def test_failed_wrong_source_or_wrong_producer_metadata_cannot_start_download(self):
        index = self.prepare()
        values, run, artifact, calls, request = self.artifact(index)
        for field, value in [('conclusion', 'failure'), ('path', '.github/workflows/release.yml'),
                             ('head_sha', '0' * 40), ('run_attempt', 2), ('id', True)]:
            original = run[field]
            run[field] = value
            calls.clear()
            try:
                with mock.patch.object(transport, 'request', side_effect=request), self.assertRaises(ValueError):
                    acquisition.acquire(self.fixture.root, self.fixture.fixture.base / ('refused-' + field), **values, token='SYNTHETIC_JOB_READ_TOKEN')
            finally:
                run[field] = original
            self.assertFalse(any(host != 'api.github.com' for host, _, _ in calls))
        artifact['workflow_run']['id'] = 101
        with mock.patch.object(transport, 'request', side_effect=request), self.assertRaises(ValueError):
            acquisition.acquire(self.fixture.root, self.fixture.fixture.base / 'wrong-artifact-run', **values, token='SYNTHETIC_JOB_READ_TOKEN')

    def test_failed_actual_crypto_cannot_create_a_bundle_or_trigger_job_readback(self):
        self.failed_crypto = True
        self.service.calls.clear()
        with self.assertRaises(ValueError): self.prepare()
        self.assertEqual(self.service.calls, [])
        self.assertFalse(self.output.exists())

    def test_owned_runtime_mutation_during_native_verification_refuses_before_artifact_output(self):
        original = self.fixture.runtime.read_bytes()
        changed = False
        with self.seams():
            real = native._command
            def tamper(executable, arguments, work, label):
                nonlocal changed
                result = real(executable, arguments, work, label)
                if label == 'verification' and not changed:
                    path = self.work / 'data/windows' / self.fixture.runtime.name
                    path.chmod(0o600)
                    path.write_bytes(b'changed owned runtime')
                    changed = True
                return result
            with mock.patch.object(native, '_command', side_effect=tamper), self.assertRaises(ValueError):
                inputs.prepare(self.fixture.root, self.fixture.directory, self.selection,
                    self.selection_sha256, self.output, self.work)
        self.assertTrue(changed)
        self.assertEqual(self.fixture.runtime.read_bytes(), original)
        self.assertFalse(self.output.exists())

    def test_original_mutation_after_owned_copy_is_detected_and_partial_outputs_are_not_reused(self):
        real = inputs.bounded_file
        selected = self.fixture.runtime
        original = selected.read_bytes()
        changed = False
        def copy_file(path, limit, destination=None):
            nonlocal changed
            result = real(path, limit, destination)
            if path == selected and destination is not None and not changed:
                selected.write_bytes(b'changed original media')
                changed = True
            return result
        try:
            with mock.patch.object(inputs, 'bounded_file', side_effect=copy_file), self.assertRaises(ValueError):
                self.prepare()
            self.assertTrue(changed)
            self.assertFalse(self.output.exists())
        finally:
            selected.write_bytes(original)
        with self.assertRaises(ValueError): self.prepare()


class OptionalWindowsHandoff(unittest.TestCase):
    def test_both_actual_native_adapters_rerun_from_owned_portable_selection(self):
        fixture = test_windows_qualification.WindowsFacts(methodName='runTest')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        fixture.qualify()
        windows, selection = fixture.trust()
        windows.run['id'] = 54321
        windows.base = windows.base.replace('/12345/', '/54321/')
        for job in windows.jobs['jobs']:
            job['run_id'] = 54321
        selection['trust']['run_id'] = '54321'
        mac = jobs.Readback(methodName='runTest')
        mac.setUp()
        self.addCleanup(mac.doCleanups)
        f = fixture.f
        mac.run['head_sha'] = mac.run['head_commit']['id'] = f.fixture.commit
        for job in mac.jobs['jobs']:
            job['head_sha'] = f.fixture.commit
        selected = copy.deepcopy(f.options)
        selected['macos']['trust'] = dict(mac.trust.values, workflow_commit=f.fixture.commit, readback_job=True)
        selected['windows']['native'] = selection
        selectors = f.fixture.base / 'selected-final'
        selectors.mkdir()
        receipt_dir = f.fixture.base / 'selected-linux'
        receipt_dir.mkdir()
        receipt = receipt_dir / 'installation.json'
        receipt.write_bytes(f.receipt.read_bytes())
        selected['linux_receipt'] = receipt
        selection_path = selectors / 'selection.json'
        selection_path.write_bytes(json.dumps(selected, default=str).encode())
        identities = []
        def command(executable, arguments, work, label):
            if label == 'version':
                return b'gh version 2.102.0 (2026-09-30)\nhttps://github.com/cli/cli/releases/tag/v2.102.0\n'
            identity = arguments[arguments.index('--cert-identity') + 1]
            identities.append(identity)
            if identity == native._policy(True)[1]:
                value = json.loads(fixture.crypto_command(executable, arguments, work, label))
                value[0]['verificationResult']['signature']['certificate']['runInvocationURI'] = \
                    native.REPOSITORY_URI + '/actions/runs/54321/attempts/1'
            else:
                value = crypto.verified(f.fixture.commit, f.options['macos']['qualification_sha256'])
            return json.dumps(value).encode()
        def get(url, limit, *, api=False):
            return (windows if '/54321/' in url else mac).get(url, limit, api=api)
        work, output = f.fixture.base / 'bundle-work', f.fixture.base / 'bundle-artifact'
        with mock.patch.object(native.sys, 'platform', 'linux'), \
                mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(native, '_command', side_effect=command), \
                mock.patch.object(website, 'public_get', side_effect=get):
            index = inputs.prepare(f.root, f.directory, selection_path, sha(selection_path.read_bytes()), output, work)
            candidate = inputs.consume(f.root, work / 'data', f.fixture.base / 'both-final',
                f.fixture.base / 'both-native', index, index['selection_sha256'])
        self.assertEqual(identities.count(native._policy(False)[1]), 2)
        self.assertEqual(identities.count(native._policy(True)[1]), 2)
        self.assertEqual(candidate.assessment['windows_native_signature'], 'verified-by-authenticated-native-qualification')
        self.assertNotIn('windows-native-final-producer-not-authenticated', candidate.blockers)
        self.assertTrue(candidate.blockers)
        self.assertFalse(candidate.assessment['production_ready'])
        self.assertTrue(all(channel['ready'] is False for channel in candidate.manifest['channels'].values()))
        self.assertEqual((candidate.directory / f.descriptor.name).read_bytes(), f.descriptor.read_bytes())


class UpstreamHandoff(unittest.TestCase):
    def setUp(self):
        self.windows = test_windows_qualification.WindowsFacts(methodName='runTest')
        self.windows.setUp()
        self.addCleanup(self.windows.doCleanups)
        self.windows.qualify()
        f = self.f = self.windows.f
        base, commit, version = f.fixture.base, f.fixture.commit, '0.2.0'
        self.work, self.output, self.artifact = base / 'upstream-work', base / 'selected-final', base / 'upstream-artifact'
        self.authorities = base / 'provisioned-authorities'
        self.authorities.mkdir(mode=0o700)
        # Build the actual ordinary unsigned-Mac five-record candidate, using the
        # same exact Windows/Linux/source/DEB inputs as the existing fixtures.
        selected = base / 'ordinary-portables'
        selected.mkdir(mode=0o700)
        for path in [f.fixture.unsigned, *[f.directory / row['file'] for row in f.inventory['assets']
                     if row.get('platform') in {'Windows', 'Linux'} and row['file'].endswith('.zip')]]:
            (selected / path.name).write_bytes(path.read_bytes())
        portable, candidate = base / 'ordinary-verified', base / 'ordinary-candidate'
        with contextlib.redirect_stdout(io.StringIO()):
            verify_release.verify(selected, commit, version, portable, f.root)
        final.readiness.handoff_deb(portable, f.fixture.deb, candidate, commit, version,
            package_sha256=sha(f.fixture.deb.read_bytes()),
            portable_sha256=next(row['sha256'] for row in f.inventory['assets'] if row['file'].endswith('-Linux-x64.zip')),
            maintainer=f.options['maintainer'], source_root=f.root)
        linux_dir = base / 'linux-qualified'
        linux_dir.mkdir(mode=0o700)
        q = dict(commit=commit, version=version, portable=f.directory / next(row['file'] for row in f.inventory['assets'] if row['file'].endswith('-Linux-x64.zip')),
            portable_sha256=next(row['sha256'] for row in f.inventory['assets'] if row['file'].endswith('-Linux-x64.zip')),
            package=f.fixture.deb, package_sha256=sha(f.fixture.deb.read_bytes()), maintainer=f.options['maintainer'])
        receipts = linux_dir / 'receipts'
        receipts.mkdir(mode=0o700)
        installation = receipts / 'installation.json'
        installation.write_bytes(f.receipt.read_bytes())
        upgrade = receipts / 'portable-to-deb.json'
        upgrade.write_bytes(qualify_linux_deb.encode(test_linux_qualification.driver_receipt(q)) + b'\n')
        qualify_linux_deb.qualify(f.root, q, installation, sha(installation.read_bytes()), upgrade, sha(upgrade.read_bytes()), linux_dir / 'report')
        self.services, self.commands, self.gets = {}, [], []
        self.fail_crypto = None
        final_selection = copy.deepcopy(f.options)
        final_selection['linux_receipt'] = installation
        _, windows_selection = self.windows.trust()
        final_selection['windows']['native'] = windows_selection
        final_selection['windows']['identity'] = 'direct-identity.json'
        final_selection['linux_native'] = dict(qualification=linux_dir / 'report/linux-deb-qualification.json',
            qualification_sha256=sha((linux_dir / 'report/linux-deb-qualification.json').read_bytes()),
            upgrade=upgrade, upgrade_sha256=sha(upgrade.read_bytes()))
        bundles = {}
        for platform, identifier in [('macos', '103'), ('windows', '105'), ('linux', '106')]:
            service = jobs.Readback(methodName='runTest')
            service.setUp()
            self.addCleanup(service.doCleanups)
            policy = native._policy(platform == 'windows', platform == 'linux')
            service.base = 'https://api.github.com/repos/' + native.REPOSITORY + '/actions/runs/' + identifier + '/attempts/1'
            service.run.update(id=int(identifier), path=policy[0], head_sha=commit)
            service.run['head_commit']['id'] = commit
            for job in service.jobs['jobs']:
                job.update(run_id=int(identifier), head_sha=commit, labels=[policy[5]])
                for step in job.get('steps', []):
                    if step['name'] == 'Qualify selected final DMG': step['name'] = policy[6]
                    if platform == 'linux' and step['name'] == 'Acquire selected signed assets': step['name'] = 'Acquire selected Linux assets'
            self.services[platform] = service
            directory = self.authorities / platform
            directory.mkdir(mode=0o700)
            for key, name in [('trusted_root', 'trusted_root.jsonl'), ('cli', 'gh')]:
                (directory / name).write_bytes(Path(service.trust.values[key]).read_bytes())
            trust = dict(service.trust.values, workflow_commit=commit, run_id=identifier, readback_job=True)
            target = final_selection['macos'] if platform == 'macos' else final_selection['windows']['native'] if platform == 'windows' else final_selection['linux_native']
            target['trust'] = trust
            bundles[platform] = Path(trust['bundle']).read_bytes()
        final_selection = json.loads(json.dumps(final_selection, default=str))
        roles, trusts = inputs.paths(final_selection)
        for keys, name, _, _ in roles: inputs.put(final_selection, keys, name)
        for keys, platform in trusts: inputs.get(final_selection, keys)['work'] = '@consumer/' + platform
        self.files = dict(candidate={path.name: path.read_bytes() for path in candidate.iterdir()},
            macos_signed={path.name: path.read_bytes() for path in [f.fixture.signed, f.fixture.image]},
            macos_native={'dmg-qualification.json': f.fixture.evidence.read_bytes(), 'dmg-qualification.sigstore.json': bundles['macos']},
            windows_signed={path.name: path.read_bytes() for path in [f.unsigned, f.package, f.runtime]},
            windows_native={'windows-installer-qualification.json': self.windows.report_path.read_bytes(),
                'windows-installer-qualification.sigstore.json': bundles['windows'], f.descriptor.name:
                    (f.fixture.base / ('windows-descriptor-' + str(self.windows.serial)) / f.descriptor.name).read_bytes()},
            linux_native={'linux-deb-qualification.json': (linux_dir / 'report/linux-deb-qualification.json').read_bytes(),
                'installation.json': installation.read_bytes(), 'portable-to-deb.json': upgrade.read_bytes(), 'linux-deb-qualification.sigstore.json': bundles['linux']})
        windows_portable = 'PeerBrush-' + version + '-Windows-x64.zip'
        self.files['windows_signed'].update({windows_portable: (f.directory / windows_portable).read_bytes(),
            'direct-identity.json': (f.direct / 'identity.json').read_bytes(), 'runtime-license.txt': (f.direct / 'runtime-license.txt').read_bytes()})
        self.value = dict(schema=1, final=final_selection, artifacts={})
        self.runs, self.artifacts, self.wrappers = {}, {}, {}
        repo = dict(id=transport.REPOSITORY_ID, full_name=transport.REPOSITORY, fork=False, private=False, owner=dict(id=transport.OWNER_ID))
        for number, role in enumerate(upstream.POLICIES, 101):
            # Fixed role insertion order assigns native IDs 103, 105, 106 above.
            record = dict(run_id=str(number), artifact_id=str(number + 200), artifact_sha256='', files={})
            workflow, name = upstream.POLICIES[role]
            self.runs[role] = dict(id=number, run_attempt=1, status='completed', conclusion='success', event='workflow_dispatch',
                head_branch='main', head_sha=commit, path='.github/workflows/' + workflow, pull_requests=[],
                repository=copy.deepcopy(repo), head_repository=copy.deepcopy(repo), actor='PRIVATE_CANARY')
            self.artifacts[role] = dict(id=number + 200, name=name if role == 'candidate' else name + str(number) + '-1', expired=False,
                url='https://api.github.com' + transport.API + 'artifacts/' + str(number + 200),
                archive_download_url='https://api.github.com' + transport.API + 'artifacts/' + str(number + 200) + '/zip',
                workflow_run=dict(id=number, repository_id=transport.REPOSITORY_ID, head_repository_id=transport.REPOSITORY_ID, head_branch='main', head_sha=commit))
            self.value['artifacts'][role] = record
        self.refresh()

    def refresh(self):
        for role, files in self.files.items():
            data = wrappers.zipped([(name, content, stat.S_IFREG | 0o400) for name, content in files.items()])
            self.wrappers[role] = data
            self.value['artifacts'][role].update(artifact_sha256=sha(data), files={name: sha(content) for name, content in files.items()})
            self.artifacts[role].update(digest='sha256:' + sha(data), size_in_bytes=len(data))

    def request(self, host, target, limit, **options):
        self.gets.append((host, target))
        if options.get('redirect'):
            return 'https://results-receiver.actions.githubusercontent.com/' + target.split('/')[-2]
        if options.get('destination') is not None:
            self.assertIsNone(options.get('token'))
            role = next(role for role, record in self.value['artifacts'].items() if record['artifact_id'] == target[1:])
            self.assertLessEqual(len(self.wrappers[role]), limit)
            options['destination'].write_bytes(self.wrappers[role])
            return options['destination']
        for role, record in self.value['artifacts'].items():
            if target == transport.API + 'runs/' + record['run_id']: return json.dumps(self.runs[role]).encode()
            if target == transport.API + 'artifacts/' + record['artifact_id']: return json.dumps(self.artifacts[role]).encode()
        raise AssertionError('Unexpected upstream request')

    def command(self, executable, arguments, work, label):
        if label == 'version':
            return b'gh version 2.102.0 (2026-09-30)\nhttps://github.com/cli/cli/releases/tag/v2.102.0\n'
        identity = arguments[arguments.index('--cert-identity') + 1]
        platform = next(name for name in self.services if native._policy(name == 'windows', name == 'linux')[1] == identity)
        self.commands.append(platform)
        if self.fail_crypto == platform: raise ValueError('Synthetic crypto failure')
        policy = native._policy(platform == 'windows', platform == 'linux')
        key = 'macos' if platform == 'macos' else 'windows' if platform == 'windows' else 'linux_native'
        selected = self.value['final'][key]
        if platform == 'windows': selected = selected['native']
        verified = crypto.verified(self.f.fixture.commit, selected['qualification_sha256'])
        result = verified[0]['verificationResult']
        for key in ['subjectAlternativeName', 'buildSignerURI', 'buildConfigURI']:
            result['signature']['certificate'][key] = identity
        result['signature']['certificate']['runInvocationURI'] = native.REPOSITORY_URI + '/actions/runs/' + selected['trust']['run_id'] + '/attempts/1'
        result['statement']['subject'][0]['name'] = policy[2]
        return json.dumps(verified).encode()

    @contextlib.contextmanager
    def seams(self):
        def get(url, limit, *, api=False):
            service = next(service for service in self.services.values() if url.startswith(service.base))
            return service.get(url, limit, api=api)
        with mock.patch.object(native.sys, 'platform', 'linux'), mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(native, '_command', side_effect=self.command), mock.patch.object(website, 'public_get', side_effect=get), \
                mock.patch.object(transport, 'request', side_effect=self.request):
            yield

    def produce(self):
        with self.seams():
            return upstream.produce(self.f.root, self.output, self.work, self.artifact, self.value,
                authority_directory=self.authorities, token='SYNTHETIC_JOB_TOKEN')

    def test_actual_raw_acquisition_signed_base_and_three_native_consumers_keep_unready_status(self):
        index = self.produce()
        inventory = json.loads((self.output / 'base/release-inventory.json').read_bytes())
        self.assertEqual(len(inventory['assets']), 6)
        self.assertIn(self.f.fixture.signed.name, {row['file'] for row in inventory['assets']})
        self.assertNotIn(self.f.fixture.unsigned.name, {row['file'] for row in inventory['assets']})
        self.assertEqual(self.commands.count('macos'), 2)
        self.assertEqual(self.commands.count('windows'), 1)
        self.assertEqual(self.commands.count('linux'), 1)
        with self.seams():
            candidate = inputs.consume(self.f.root, self.work / 'bundle/data', self.f.fixture.base / 'relocated-final',
                self.f.fixture.base / 'relocated-native', index, index['selection_sha256'])
        self.assertNotIn('linux-installed-evidence-producer-not-authenticated', candidate.blockers)
        self.assertIn('installed-upgrade-interruption-and-visible-native-acceptance-not-established', candidate.blockers)
        self.assertEqual(candidate.assessment['linux_native_origin']['native_service']['run_id'], '106')
        self.assertEqual(candidate.assessment['windows_native_signature'], 'verified-by-authenticated-native-qualification')
        self.assertEqual((candidate.directory / self.f.descriptor.name).read_bytes(), self.f.descriptor.read_bytes())
        self.assertFalse(candidate.assessment['production_ready'])
        self.assertTrue(all(channel['ready'] is False for channel in candidate.manifest['channels'].values()))
        for path in candidate.directory.iterdir(): self.assertNotIn(b'PRIVATE_CANARY', path.read_bytes())
        with zipfile.ZipFile(self.artifact / inputs.ARCHIVE) as archive:
            self.assertTrue({'native/linux/linux-deb-qualification.json', 'native/linux/portable-to-deb.json', 'linux/installation.json'} <= set(archive.namelist()))

    def test_missing_independent_authority_fails_before_network_work_or_outputs(self):
        (self.authorities / 'linux/trusted_root.jsonl').unlink()
        with self.assertRaises((ValueError, OSError)): self.produce()
        self.assertEqual(self.gets, [])
        self.assertFalse(self.work.exists())
        self.assertFalse(self.artifact.exists())

    def test_native_report_and_bundle_without_original_descriptor_cannot_substitute_for_media(self):
        del self.files['windows_native'][self.f.descriptor.name]
        self.refresh()
        with self.assertRaises(ValueError): self.produce()
        self.assertEqual(self.gets, [])
        self.assertFalse(self.work.exists())

    def test_each_wrong_fixed_upstream_workflow_refuses_before_any_storage_download(self):
        for role in upstream.POLICIES:
            original = self.runs[role]['path']
            self.runs[role]['path'] = '.github/workflows/production.yml'
            self.gets.clear()
            try:
                with self.subTest(role=role), self.assertRaises(ValueError): self.produce()
            finally:
                self.runs[role]['path'] = original
            self.assertFalse(any(host != 'api.github.com' for host, _ in self.gets))
            self.assertFalse(self.work.exists())

    def test_self_consistent_changed_linux_receipt_cannot_replace_its_distinct_native_report(self):
        value = json.loads(self.files['linux_native']['portable-to-deb.json'])
        value['old_commit'] = 'b' * 40
        data = final.encode(value)
        self.files['linux_native']['portable-to-deb.json'] = data
        self.value['final']['linux_native']['upgrade_sha256'] = sha(data)
        self.refresh()
        with self.assertRaises(ValueError): self.produce()
        self.assertEqual(self.services['linux'].calls, [])
        self.assertFalse(self.artifact.exists())

    def test_failed_actual_windows_crypto_cannot_emit_data_artifact(self):
        self.fail_crypto = 'windows'
        with self.assertRaises(ValueError): self.produce()
        self.assertIn('windows', self.commands)
        self.assertEqual(self.services['windows'].calls, [])
        self.assertFalse(self.artifact.exists())


class DormantSource(unittest.TestCase):
    def test_two_new_jobs_and_production_composition_stay_literal_disabled(self):
        root = Path(__file__).resolve().parents[1]
        producer = (root / '.github/workflows/final-inputs.yml').read_text()
        for name in ['gate', 'prepare']:
            body = re.split(r'\n  [a-z][a-z-]*:\n', producer.split('\n  ' + name + ':\n', 1)[1], maxsplit=1)[0]
            self.assertIn('if: ${{ false }}', body)
        self.assertNotIn('id-token: write', producer)
        self.assertNotIn('contents: write', producer)
        self.assertNotIn('artifact/*', producer)
        self.assertIn('scripts/assemble_final_inputs.py', producer)
        self.assertIn('inputs.upstream_selection_sha256', producer)
        self.assertIn('peerbrush-final-trust/{macos,windows,linux}', producer)
        production = (root / '.github/workflows/production.yml').read_text()
        compose = production.split('\n  compose:\n', 1)[1].split('\n  publish:\n', 1)[0]
        self.assertIn('if: ${{ false }}', compose)
        self.assertIn('scripts/acquire_final_inputs.py', compose)
        self.assertIn('FINAL_INPUTS_RUN', compose)
        self.assertNotIn('actions/download-artifact@', compose)
        self.assertNotIn('input_artifact_name', production)
        self.assertIn('scripts/prepare_final_inputs.py consume', compose)


if __name__ == '__main__':
    unittest.main()
