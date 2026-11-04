"""Store SDK acquisition and workflow policy; synthetic transport is never SDK authority."""
import copy
import hashlib
import json
from pathlib import Path
import re
import stat
import tempfile
import textwrap
import unittest
from unittest import mock
import zipfile

import acquire_store_qualification_inputs as acquire
import assemble_final_inputs as upstream
import prepare_final_inputs as inputs
import qualify_store_sdk as qualifier
import test_store_msix_preflight as fixtures
import test_macos_qualification_producer as wrappers


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Acquisition(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='peerbrush-store-sdk-acquire-')
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.seed = fixtures.StorePreflight('runTest')
        self.addCleanup(self.seed.doCleanups)
        self.seed.setUp()
        f = self.seed
        self.commit = f.inputs['commit']
        portable = f.fixture.portable
        with zipfile.ZipFile(portable) as archive:
            release_name = next(name for name in archive.namelist() if name.endswith('/release.json'))
            manifest = json.loads(archive.read(release_name))
            source_name = manifest['source']
            source = archive.read(release_name.rsplit('/', 1)[0] + '/' + source_name)
        self.files = {
            '202': {portable.name: portable.read_bytes(), source_name: source},
            '203': {f.package.name: f.package.read_bytes(), f.report.name: f.report.read_bytes(),
                    f.package.name + '.sha256': (f.inputs['package_sha256'] + '  ' + f.package.name + '\n').encode()}}
        repo = dict(id=acquire.transport.REPOSITORY_ID, full_name=acquire.transport.REPOSITORY,
                    fork=False, private=False, owner=dict(id=acquire.transport.OWNER_ID))
        self.run = dict(id=101, run_attempt=1, status='completed', conclusion='success', event='workflow_dispatch',
            head_branch='main', head_sha=self.commit, pull_requests=[], path='.github/workflows/release.yml',
            head_commit=dict(id=self.commit), repository=repo, head_repository=copy.deepcopy(repo))
        self.artifacts = {}
        for identifier, name in [('202', 'candidate-Windows'), ('203', 'unsigned-store-msix-candidate')]:
            self.artifacts[identifier] = dict(id=int(identifier), name=name, expired=False,
                url='https://api.github.com' + acquire.transport.API + 'artifacts/' + identifier,
                archive_download_url='https://api.github.com' + acquire.transport.API + 'artifacts/' + identifier + '/zip',
                workflow_run=dict(id=101, repository_id=acquire.transport.REPOSITORY_ID,
                    head_repository_id=acquire.transport.REPOSITORY_ID, head_branch='main', head_sha=self.commit))
        q = dict(f.inputs, portable=portable.name, package=f.package.name, report=f.report.name,
                 makeappx_sha256='b' * 64, makeappx_version='10.0.26100.1')
        self.selection = dict(commit=self.commit, qualification=q, transport=dict(run_id='101',
            portable_artifact_id='202', portable_artifact_sha256='', store_artifact_id='203', store_artifact_sha256='',
            source_sha256=sha(source), checksum_sha256=''))
        self.environment = dict(GITHUB_REPOSITORY=acquire.transport.REPOSITORY, GITHUB_REF='refs/heads/main',
            GITHUB_REPOSITORY_ID=str(acquire.transport.REPOSITORY_ID), GITHUB_REPOSITORY_OWNER_ID=str(acquire.transport.OWNER_ID),
            GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=acquire.WORKFLOW,
            GITHUB_SHA=self.commit, GITHUB_WORKFLOW_SHA=self.commit, GITHUB_RUN_ATTEMPT='1',
            RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Windows', RUNNER_ARCH='X64',
            PEERBRUSH_STORE_SDK_QUALIFICATION_ENABLED='true')
        self.serial = 0
        self.gets = []
        self.refresh()

    def refresh(self):
        self.blobs = {identifier: wrappers.zipped([(name, data, stat.S_IFREG | 0o400) for name, data in files.items()])
                      for identifier, files in self.files.items()}
        for identifier, blob in self.blobs.items():
            self.artifacts[identifier].update(digest='sha256:' + sha(blob), size_in_bytes=len(blob))
        self.selection['transport'].update(portable_artifact_sha256=sha(self.blobs['202']), store_artifact_sha256=sha(self.blobs['203']),
            checksum_sha256=sha(self.files['203'][self.seed.package.name + '.sha256']))

    def request(self, host, target, limit, **options):
        self.gets.append((host, target))
        if options.get('redirect'):
            return 'https://results-receiver.actions.githubusercontent.com/' + target.split('/')[-2]
        if options.get('destination') is not None:
            self.assertNotIn('token', options)
            blob = self.blobs[target[1:]]
            self.assertLessEqual(len(blob), limit)
            options['destination'].write_bytes(blob)
            return options['destination']
        self.assertEqual(host, 'api.github.com')
        self.assertEqual(options.get('token'), 'SYNTHETIC_JOB_TOKEN')
        if target == acquire.transport.API + 'runs/101': return json.dumps(self.run).encode()
        identifier = target.rsplit('/', 1)[1]
        if identifier in self.artifacts: return json.dumps(self.artifacts[identifier]).encode()
        raise AssertionError('Unexpected Store SDK acquisition request')

    def run_acquire(self):
        self.serial += 1
        output = self.base / ('inputs-' + str(self.serial))
        with mock.patch.object(acquire.transport, 'request', side_effect=self.request):
            checksum = acquire.acquire(self.seed.root, output, self.selection,
                token='SYNTHETIC_JOB_TOKEN', environment=self.environment)
        return output, checksum

    def test_same_exact_release_inputs_are_owned_and_reinspected_without_sdk_or_store_claims(self):
        output, checksum = self.run_acquire()
        raw = (output / 'qualification-selection.json').read_bytes()
        self.assertEqual(sha(raw), checksum)
        selected = json.loads(raw)
        self.assertEqual(set(selected), qualifier.INPUT_FIELDS)
        self.assertEqual(Path(selected['portable']).read_bytes(), self.seed.fixture.portable.read_bytes())
        self.assertEqual(Path(selected['package']).read_bytes(), self.seed.package.read_bytes())
        self.assertNotIn('ready', selected)
        self.assertNotIn('native_producer_authentication', selected)
        self.assertTrue(all(path.is_relative_to(output) for path in map(Path, [selected['portable'], selected['package'], selected['report']])))

    def test_invocation_extra_fields_paths_and_tool_pins_refuse_before_network_or_work(self):
        original = copy.deepcopy(self.selection)
        for change in ['environment', 'extra', 'path', 'version', 'same-artifact', 'previous-version', 'same-version']:
            self.selection = copy.deepcopy(original)
            environment = dict(self.environment)
            if change == 'environment': self.environment['GITHUB_RUN_ATTEMPT'] = '2'
            elif change == 'extra': self.selection['qualification']['ready'] = True
            elif change == 'path': self.selection['qualification']['package'] = '../foreign.msix'
            elif change == 'version': self.selection['qualification']['makeappx_version'] = '10.0.26100.65536'
            elif change == 'previous-version': self.selection['qualification']['previous_version'] = '01.25.0.0'
            elif change == 'same-version': self.selection['qualification']['previous_version'] = '1.26.0.0'
            else: self.selection['transport']['store_artifact_id'] = '202'
            with self.subTest(change=change), self.assertRaises(ValueError): self.run_acquire()
            self.assertEqual(self.gets, [])
            self.assertFalse((self.base / ('inputs-' + str(self.serial))).exists())
            self.environment = environment

    def test_wrong_store_workflow_expiry_outer_or_inner_bytes_never_emit_qualification_selection(self):
        original = copy.deepcopy(self.selection)
        artifacts = copy.deepcopy(self.artifacts)
        for change in ['workflow', 'expired', 'outer', 'inner']:
            self.selection = copy.deepcopy(original)
            self.artifacts = copy.deepcopy(artifacts)
            if change == 'workflow': self.run['path'] = '.github/workflows/windows-release-signing.yml'
            elif change == 'expired': self.artifacts['203']['expired'] = True
            elif change == 'outer': self.artifacts['203']['digest'] = 'sha256:' + 'f' * 64
            else: self.selection['qualification']['report_sha256'] = 'e' * 64
            with self.subTest(change=change), self.assertRaises(ValueError): self.run_acquire()
            self.assertFalse((self.base / ('inputs-' + str(self.serial)) / 'qualification-selection.json').exists())
            self.run['path'] = '.github/workflows/release.yml'

    def test_checksum_line_endings_remain_exact_and_late_metadata_changes_refuse(self):
        name = self.seed.package.name + '.sha256'
        original = self.files['203'][name]
        for ending in [b'\r\n', b'\n']:
            self.files['203'][name] = original.replace(b'\n', ending)
            self.refresh()
            self.run_acquire()
        request = self.request
        def changed(host, target, limit, **options):
            result = request(host, target, limit, **options)
            if host != 'api.github.com' and target == '/203': self.run['conclusion'] = 'failure'
            return result
        with mock.patch.object(self, 'request', side_effect=changed), self.assertRaises(ValueError): self.run_acquire()
        self.assertFalse((self.base / ('inputs-' + str(self.serial)) / 'qualification-selection.json').exists())

    def test_report_budget_is_checked_before_extraction_and_final_metadata_is_reconciled(self):
        original = self.files['203'][self.seed.report.name]
        self.files['203'][self.seed.report.name] = b'x' * (65536 + 1)
        self.selection['qualification']['report_sha256'] = sha(self.files['203'][self.seed.report.name])
        self.refresh()
        with self.assertRaises(ValueError): self.run_acquire()
        self.assertFalse((self.base / ('inputs-' + str(self.serial)) / 'store' / self.seed.report.name).exists())
        self.files['203'][self.seed.report.name] = original
        self.selection['qualification']['report_sha256'] = sha(original)
        self.refresh()
        qualify = qualifier.content.qualify
        def changed(*args, **kwargs):
            result = qualify(*args, **kwargs)
            self.artifacts['202']['expired'] = True
            return result
        with mock.patch.object(qualifier.content, 'qualify', side_effect=changed), self.assertRaises(ValueError): self.run_acquire()
        self.assertFalse((self.base / ('inputs-' + str(self.serial)) / 'qualification-selection.json').exists())


class Workflow(unittest.TestCase):
    def setUp(self):
        self.text = (Path(__file__).resolve().parents[1] / '.github/workflows/store-qualification.yml').read_text()

    def test_manual_main_gate_and_only_the_protected_qualifier_declares_attestation_permissions(self):
        gate, qualify = self.text.split('\n  gate:\n', 1)[1].split('\n  qualify:\n', 1)
        self.assertIn("if: vars.PEERBRUSH_STORE_SDK_QUALIFICATION_ENABLED == 'true' && github.repository == 'Deftr0y/PeerBrush' && github.ref == 'refs/heads/main' && github.event_name == 'workflow_dispatch'", gate)
        self.assertIn("if: needs.gate.result == 'success'", qualify)
        for body in [gate, qualify]:
            self.assertIn('runs-on: windows-2025', body)
            self.assertLess(body.index('Require reviewed main invocation'), body.index('actions/checkout@'))
            self.assertIn('persist-credentials: false', body)
        self.assertNotIn('id-token: write', gate)
        self.assertIn('environment: store-native-qualification', qualify)
        self.assertIn('id-token: write', qualify)
        self.assertNotIn('contents: write', self.text)
        self.assertNotIn('secrets.', self.text)
        self.assertNotIn('submit_store_release.py', self.text)
        self.assertNotIn('--submit', self.text)
        order = [qualify.index(name) for name in ['Acquire selected Store assets', 'Qualify selected Store SDK package',
            'Attest generated native qualification', 'Retain qualification and bundle', 'Upload qualification evidence']]
        self.assertEqual(order, sorted(order))
        self.assertIn('subject-name: store-sdk-qualification.json', qualify)
        self.assertIn('name: store-native-qualification-${{ github.run_id }}-${{ github.run_attempt }}', qualify)
        self.assertIn('peerbrush-store-sdk-evidence/*', qualify)
        self.assertNotIn('peerbrush-store-sdk-work/*', qualify)

    def test_actual_precheckout_program_requires_exact_first_main_hosted_identity(self):
        programs = re.findall(r"python -I - <<'PY'\n(.*?)\n          PY", self.text, re.S)
        self.assertGreaterEqual(len(programs), 2)
        environment = dict(SELECTED_COMMIT='a' * 40, GITHUB_REPOSITORY='Deftr0y/PeerBrush', GITHUB_REF='refs/heads/main',
            GITHUB_REPOSITORY_ID='1407648785', GITHUB_REPOSITORY_OWNER_ID='21202494', GITHUB_EVENT_NAME='workflow_dispatch',
            GITHUB_WORKFLOW_REF=acquire.WORKFLOW, GITHUB_SHA='a' * 40, GITHUB_WORKFLOW_SHA='a' * 40, GITHUB_RUN_ATTEMPT='1',
            RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Windows', RUNNER_ARCH='X64', PEERBRUSH_STORE_SDK_QUALIFICATION_ENABLED='true')
        for program in programs[:2]:
            with mock.patch.dict('os.environ', environment, clear=True): exec(textwrap.dedent(program), {})
            for key, value in [('GITHUB_RUN_ATTEMPT', '2'), ('GITHUB_REF', 'refs/heads/feature/0.3/test'),
                    ('GITHUB_WORKFLOW_SHA', 'b' * 40), ('RUNNER_ENVIRONMENT', 'self-hosted'),
                    ('GITHUB_REPOSITORY_OWNER_ID', '1'), ('PEERBRUSH_STORE_SDK_QUALIFICATION_ENABLED', 'false')]:
                with self.subTest(key=key), mock.patch.dict('os.environ', dict(environment, **{key: value}), clear=True), self.assertRaises(SystemExit):
                    exec(textwrap.dedent(program), {})


if __name__ == '__main__':
    unittest.main()
