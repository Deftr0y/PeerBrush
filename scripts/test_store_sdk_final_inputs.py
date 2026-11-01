"""Real six-role acquisition/relocation with explicit fake Store content/crypto seams.

The historical transport fixture is below the Store version floor. Actual source,
MSIX and native SDK checks run separately in test_store_sdk_qualification.py.
"""
import contextlib
import copy
import json
from pathlib import Path
import unittest
from unittest import mock
import zipfile

import final_release as final
import prepare_final_inputs as inputs
import qualify_store_sdk as qualifier
import test_final_inputs as handoff
import verify_macos_qualification as shared
import verify_store_qualification as adapter


class StoreNativeHandoff(unittest.TestCase):
    def setUp(self):
        fixture = handoff.StoreUpstreamHandoff('runTest')
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        self.f = f = fixture.legacy
        self.value = fixture.value
        self.value['schema'] = 3
        self.policy = shared._policy(False, False, True)
        self.checked = dict(commit=f.f.fixture.commit, version='0.2.0',
            scope='synthetic-transport-only', installed_acceptance='not-performed',
            public_availability='unverified', production_ready=False)
        raw = qualifier.encode(qualifier.report_for(self.checked,
            makeappx_sha256='b' * 64, makeappx_version='10.0.26100.1'))
        service = handoff.jobs.Readback('runTest')
        self.addCleanup(service.doCleanups)
        service.setUp()
        identifier, artifact_id = '107', '307'
        service.base = 'https://api.github.com/repos/' + shared.REPOSITORY + '/actions/runs/' + identifier + '/attempts/1'
        service.run.update(id=int(identifier), path=self.policy[0], head_sha=self.checked['commit'])
        service.run['head_commit']['id'] = self.checked['commit']
        for job in service.jobs['jobs']:
            job.update(run_id=int(identifier), head_sha=self.checked['commit'], labels=[self.policy[5]])
            for step in job.get('steps', []):
                if step['name'] == 'Acquire selected signed assets': step['name'] = 'Acquire selected Store assets'
                elif step['name'] == 'Qualify selected final DMG': step['name'] = self.policy[6]
        directory = f.authorities / 'store'
        directory.mkdir(mode=0o700)
        for key, name in [('trusted_root', 'trusted_root.jsonl'), ('cli', 'gh')]:
            (directory / name).write_bytes(Path(service.trust.values[key]).read_bytes())
        trust = dict(service.trust.values, workflow_commit=self.checked['commit'], run_id=identifier,
                     readback_job=True, work='@consumer/store')
        self.selected = dict(qualification='native/store/' + qualifier.REPORT, qualification_sha256=handoff.sha(raw),
            makeappx_sha256='b' * 64, makeappx_version='10.0.26100.1', trust=trust)
        for key, name in [('bundle', 'bundle.jsonl'), ('trusted_root', 'trusted_root.jsonl'), ('cli', 'gh')]:
            trust[key] = 'native/store/' + name
        self.value['final']['store']['native'] = self.selected
        self.value['artifacts']['store_native'] = dict(run_id=identifier, artifact_id=artifact_id, artifact_sha256='', files={})
        f.files['store_native'] = {qualifier.REPORT: raw,
            'store-sdk-qualification.sigstore.json': Path(service.trust.values['bundle']).read_bytes()}
        f.runs['store_native'] = copy.deepcopy(f.runs['macos_native'])
        f.runs['store_native'].update(id=int(identifier), path=self.policy[0])
        f.artifacts['store_native'] = copy.deepcopy(f.artifacts['macos_native'])
        f.artifacts['store_native'].update(id=int(artifact_id), name='store-native-qualification-107-1',
            url='https://api.github.com' + handoff.transport.API + 'artifacts/' + artifact_id,
            archive_download_url='https://api.github.com' + handoff.transport.API + 'artifacts/' + artifact_id + '/zip')
        f.artifacts['store_native']['workflow_run']['id'] = int(identifier)
        f.services['store'] = service
        original = f.command
        def command(executable, arguments, work, label):
            if label == 'version' or arguments[arguments.index('--cert-identity') + 1] != self.policy[1]:
                # The legacy fixture knows only its original platform policies.
                store = f.services.pop('store')
                try: return original(executable, arguments, work, label)
                finally: f.services['store'] = store
            f.commands.append('store')
            if f.fail_crypto == 'store': raise ValueError('Synthetic Store crypto failure')
            verified = handoff.crypto.verified(self.checked['commit'], self.selected['qualification_sha256'])
            result = verified[0]['verificationResult']
            for key in ['subjectAlternativeName', 'buildSignerURI', 'buildConfigURI']:
                result['signature']['certificate'][key] = self.policy[1]
            result['signature']['certificate']['runInvocationURI'] = shared.REPOSITORY_URI + '/actions/runs/107/attempts/1'
            result['statement']['subject'][0]['name'] = qualifier.REPORT
            return json.dumps(verified).encode()
        f.command = command
        f.refresh()

    def binding(self, root, inventory, directory, selected):
        # Only the historical .2 content floor is a seam. The actual fixed SDK
        # origin adapter still verifies the selected report, independent roots,
        # pinned CLI/OIDC claims and successful exact native job at each stage.
        package, report = Path(selected['package']), Path(selected['report'])
        self.assertEqual(handoff.sha(package.read_bytes()), selected['package_sha256'])
        self.assertEqual(handoff.sha(report.read_bytes()), selected['report_sha256'])
        origin, records = adapter.bind(root, selected['native'], self.checked)
        checked = dict(self.checked, native_producer_authentication=
            'verified-pinned-workflow-oidc-attestation-and-successful-job', native_origin=origin)
        return ([package], final.store_package_channel(package.name),
            [(package, final.LIMIT, selected['package_sha256']), *records], checked,
            {report.name: report.read_bytes(), package.name + '.sha256':
             (selected['package_sha256'] + '  ' + package.name + '\n').encode()})

    @contextlib.contextmanager
    def seams(self):
        with mock.patch.object(final, 'store_binding', side_effect=self.binding):
            yield

    def test_six_role_data_relocates_and_reauthenticates_without_installed_or_public_authority(self):
        f = self.f
        with self.seams():
            index = f.produce()
            with f.seams():
                candidate = inputs.consume(f.f.root, f.work / 'bundle/data', f.f.fixture.base / 'sdk-final',
                    f.f.fixture.base / 'sdk-native', index, index['selection_sha256'])
        self.assertEqual(f.commands.count('store'), 2)
        self.assertNotIn('store-native-sdk-producer-not-authenticated', candidate.blockers)
        self.assertIn('store-installed-upgrade-acceptance-not-established', candidate.blockers)
        self.assertIn('store-submission-automation-not-qualified', candidate.blockers)
        self.assertEqual(candidate.assessment['store_package']['native_origin']['native_service']['run_id'], '107')
        self.assertFalse(candidate.assessment['production_ready'])
        self.assertTrue(all(channel['ready'] is False for channel in candidate.manifest['channels'].values()))
        with zipfile.ZipFile(f.artifact / inputs.ARCHIVE) as archive:
            names = set(archive.namelist())
            self.assertTrue({'native/store/' + name for name in
                [qualifier.REPORT, 'bundle.jsonl', 'trusted_root.jsonl', 'gh']} <= names)
            self.assertFalse(any(name.startswith('native/windows/') or 'sdk-work' in name for name in names))
        candidate.verify()

    def test_old_schema_cannot_silently_acquire_sdk_authority_and_new_schema_requires_exact_six_roles(self):
        for change in ['old', 'missing', 'extra', 'same-run', 'wrong-report']:
            selected = copy.deepcopy(self.value)
            if change == 'old': selected['schema'] = 2
            elif change == 'missing': del selected['artifacts']['store_native']
            elif change == 'extra': selected['artifacts']['unknown'] = selected['artifacts']['store_native']
            elif change == 'same-run': selected['artifacts']['store_native']['run_id'] = selected['artifacts']['candidate']['run_id']
            else: selected['artifacts']['store_native']['files'][qualifier.REPORT] = 'e' * 64
            with self.subTest(change=change), self.assertRaises(ValueError): handoff.upstream.selections(selected)
        self.assertEqual(self.f.gets, [])
        self.assertFalse(self.f.work.exists())

    def test_missing_independent_store_authority_refuses_before_network_or_work(self):
        (self.f.authorities / 'store/trusted_root.jsonl').unlink()
        with self.seams(), self.assertRaises((ValueError, OSError)): self.f.produce()
        self.assertEqual(self.f.gets, [])
        self.assertFalse(self.f.work.exists())
        self.assertFalse(self.f.artifact.exists())

    def test_wrong_native_workflow_rerun_expiry_and_origin_refuse_before_storage_download(self):
        f = self.f
        run, artifact = copy.deepcopy(f.runs['store_native']), copy.deepcopy(f.artifacts['store_native'])
        for change in ['workflow', 'rerun', 'expired', 'source']:
            f.runs['store_native'], f.artifacts['store_native'] = copy.deepcopy(run), copy.deepcopy(artifact)
            if change == 'workflow': f.runs['store_native']['path'] = '.github/workflows/windows-qualification.yml'
            elif change == 'rerun': f.runs['store_native']['run_attempt'] = 2
            elif change == 'expired': f.artifacts['store_native']['expired'] = True
            else: f.artifacts['store_native']['workflow_run']['head_sha'] = 'e' * 40
            with self.subTest(change=change), self.assertRaises(ValueError): f.produce()
            self.assertFalse(any(host != 'api.github.com' for host, _ in f.gets))
            self.assertFalse(f.work.exists())
            self.assertFalse(f.artifact.exists())
            f.gets.clear()

    def test_store_crypto_failure_cannot_emit_data_or_reconcile_native_job(self):
        self.f.fail_crypto = 'store'
        with self.seams(), self.assertRaises(ValueError): self.f.produce()
        self.assertIn('store', self.f.commands)
        self.assertEqual(self.f.services['store'].calls, [])
        self.assertFalse(self.f.artifact.exists())


if __name__ == '__main__':
    unittest.main()
