"""Offline native producer regressions; no network, Apple tools or attestations."""
import copy
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import tempfile
import textwrap
import unittest
from unittest import mock
import zipfile

import acquire_macos_qualification_inputs as acquire

COMMIT = '1' * 40
ARCHIVE = 'PeerBrush-0.30.0-macOS-arm64-apple-signed-r2.zip'
IMAGE = 'PeerBrush-0.30.0-macOS-arm64-apple-signed-r2-dmg-signed-r3.dmg'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def zipped(rows):
    result = io.BytesIO()
    with zipfile.ZipFile(result, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data, kind in rows:
            info = zipfile.ZipInfo(name)
            info.create_system = 3
            info.external_attr = kind << 16
            archive.writestr(info, data)
    return result.getvalue()


class NativeProducer(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-native-producer-')
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name).resolve()
        self.root = self.parent / 'source'
        self.root.mkdir()
        self.body = {ARCHIVE: b'synthetic selected signed archive', IMAGE: b'synthetic selected final image'}
        self.wrapper = zipped([(name, data, 0o100644) for name, data in self.body.items()])
        self.inputs = dict(commit=COMMIT, run_id='101', artifact_id='202', artifact_sha256=sha(self.wrapper),
            archive_name=ARCHIVE, archive_sha256=sha(self.body[ARCHIVE]),
            image_name=IMAGE, image_sha256=sha(self.body[IMAGE]), team_id='ABCD123456')
        self.environment = dict(GITHUB_REPOSITORY=acquire.REPOSITORY, GITHUB_REF='refs/heads/main',
            GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=acquire.WORKFLOW,
            GITHUB_SHA=COMMIT, GITHUB_WORKFLOW_SHA=COMMIT, GITHUB_RUN_ATTEMPT='1',
            RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='macOS', RUNNER_ARCH='ARM64',
            PEERBRUSH_NATIVE_QUALIFICATION_ENABLED='true')
        self.version = '0.30.0'
        def git(root, *arguments):
            self.assertEqual(root, self.root)
            if arguments == ('rev-parse', 'HEAD'):
                return (COMMIT + '\n').encode()
            if arguments == ('status', '--porcelain', '--untracked-files=no'):
                return b''
            if arguments == ('show', 'HEAD:Cargo.toml'):
                return ('[package]\nversion="' + str(self.version) + '"\n').encode()
            raise AssertionError('Unexpected source query')
        patch = mock.patch.object(acquire, 'git', side_effect=git)
        patch.start()
        self.addCleanup(patch.stop)
        repo = dict(id=acquire.REPOSITORY_ID, full_name=acquire.REPOSITORY,
                    owner=dict(id=acquire.OWNER_ID), fork=False, private=False)
        self.run = dict(id=101, run_attempt=1, status='completed', conclusion='success',
            event='workflow_dispatch', head_branch='main', head_sha=COMMIT, pull_requests=[],
            repository=copy.deepcopy(repo), head_repository=copy.deepcopy(repo))
        self.artifact = dict(id=202, expired=False, digest='sha256:' + sha(self.wrapper), size_in_bytes=len(self.wrapper),
            url='https://api.github.com' + acquire.API + 'artifacts/202',
            archive_download_url='https://api.github.com' + acquire.API + 'artifacts/202/zip',
            workflow_run=dict(id=101, repository_id=acquire.REPOSITORY_ID,
                head_repository_id=acquire.REPOSITORY_ID, head_branch='main', head_sha=COMMIT))

    def selected(self, inputs=None, environment=None):
        return acquire.selected(self.root, **(inputs or self.inputs),
                                environment=self.environment if environment is None else environment)

    def metadata(self):
        return acquire.metadata(self.run, self.artifact, **{name: self.inputs[name]
            for name in ['commit', 'run_id', 'artifact_id', 'artifact_sha256']})

    def fake_request(self, host, target, limit, **options):
        if host == 'api.github.com':
            self.assertEqual(options.get('token'), 'synthetic-read-token')
            if target == acquire.API + 'runs/101':
                return json.dumps(self.run).encode()
            if target == acquire.API + 'artifacts/202':
                return json.dumps(self.artifact).encode()
            if target == acquire.API + 'artifacts/202/zip':
                self.assertTrue(options.get('redirect'))
                return 'https://fixture.blob.core.windows.net/artifact?sig=private-never-log'
            raise AssertionError('Unexpected official API route')
        self.assertEqual(host, 'fixture.blob.core.windows.net')
        self.assertNotIn('token', options)
        self.assertEqual(target, '/artifact?sig=private-never-log')
        with options['destination'].open('xb') as stream:
            stream.write(self.wrapper)

    def test_exact_main_stable_source_and_revisioned_filenames_are_mandatory(self):
        self.assertEqual(self.selected(), '0.30.0')
        for key in self.environment:
            changed = dict(self.environment, **{key: 'wrong'})
            with self.subTest(context=key), self.assertRaises(ValueError):
                self.selected(environment=changed)
        for version in ['0.30.0-dev.1', '00.30.0', '0.30.0+build', True]:
            self.version = version
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.selected()
        self.version = '0.30.0'
        for key, value in [('commit', '0' * 39), ('run_id', '01'), ('artifact_id', '-1'),
                           ('artifact_sha256', 'A' * 64), ('archive_name', '../' + ARCHIVE),
                           ('image_name', IMAGE.replace('-r3.dmg', '-r0.dmg')), ('team_id', 'wrong')]:
            changed = dict(self.inputs, **{key: value})
            with self.subTest(field=key), self.assertRaises(ValueError):
                self.selected(inputs=changed)

    def test_predecessor_first_success_main_and_exact_official_artifact_are_mandatory(self):
        self.assertEqual(self.metadata(), len(self.wrapper))
        original_run, original_artifact = copy.deepcopy(self.run), copy.deepcopy(self.artifact)
        for key, value in [('run_attempt', 2), ('run_attempt', True), ('event', 'pull_request'),
                           ('status', 'in_progress'), ('conclusion', 'failure'), ('head_sha', '0' * 40),
                           ('head_branch', 'feature/0.3/fork'), ('pull_requests', [{}])]:
            self.run = copy.deepcopy(original_run)
            self.run[key] = value
            with self.subTest(run_field=key), self.assertRaises(ValueError):
                self.metadata()
        for key, value in [('id', True), ('full_name', 'fork/PeerBrush'), ('fork', True), ('owner', {'id': 1})]:
            self.run = copy.deepcopy(original_run)
            self.run['head_repository'][key] = value
            with self.subTest(repository_field=key), self.assertRaises(ValueError):
                self.metadata()
        self.run = original_run
        for key, value in [('id', 203), ('expired', True), ('digest', 'sha256:' + '0' * 64),
                           ('size_in_bytes', acquire.MAX_ARTIFACT + 1),
                           ('archive_download_url', 'https://example.invalid/artifact'),
                           ('workflow_run', dict(original_artifact['workflow_run'], head_repository_id=1))]:
            self.artifact = copy.deepcopy(original_artifact)
            self.artifact[key] = value
            with self.subTest(artifact_field=key), self.assertRaises(ValueError):
                self.metadata()

    def test_actual_owned_zip_extraction_binds_outer_and_both_inner_bytes(self):
        output = self.parent / 'inputs'
        with mock.patch.object(acquire, 'request', side_effect=self.fake_request):
            self.assertEqual(acquire.acquire(self.root, output, **self.inputs,
                token='synthetic-read-token', environment=self.environment), '0.30.0')
        self.assertEqual({path.name for path in output.iterdir()}, {ARCHIVE, IMAGE, 'selected-artifact.zip'})
        for name, data in self.body.items():
            self.assertEqual((output / name).read_bytes(), data)
        with mock.patch.object(acquire, 'request') as network, self.assertRaises(ValueError):
            acquire.acquire(self.root, output, **self.inputs, token='synthetic-read-token', environment=self.environment)
        network.assert_not_called()
        wrong = dict(self.inputs, image_sha256='0' * 64)
        with mock.patch.object(acquire, 'request', side_effect=self.fake_request), self.assertRaises(ValueError):
            acquire.acquire(self.root, self.parent / 'damaged', **wrong,
                token='synthetic-read-token', environment=self.environment)

    def test_zip_inventory_links_extra_entries_and_central_directory_limits_reject(self):
        selected = {name: sha(data) for name, data in self.body.items()}
        rows = [(name, data, 0o100644) for name, data in self.body.items()]
        invalid = [
            zipped([(ARCHIVE, b'link-target', 0o120777), rows[1]]),
            zipped([('../' + ARCHIVE, rows[0][1], 0o100644), rows[1]]),
            zipped([rows[0], rows[1], ('unreviewed-receipt.json', b'false-authority', 0o100644)]),
            zipped([rows[0], rows[0]]),
            zipped([(ARCHIVE + '/', b'', 0o40755), rows[1]]),
            self.wrapper[:-1], self.wrapper + b'trailing', b'not-a-zip',
        ]
        damaged = bytearray(self.wrapper)
        damaged[-12:-8] = struct.pack('<I', 65536)  # EOCD central-directory byte budget.
        invalid.append(bytes(damaged))
        for index, data in enumerate(invalid):
            path, output = self.parent / ('invalid-' + str(index) + '.zip'), self.parent / ('out-' + str(index))
            path.write_bytes(data)
            output.mkdir()
            with self.subTest(case=index), self.assertRaises(ValueError):
                acquire.extract(path, output, selected)


    def test_metadata_and_selected_file_changes_during_handoff_reject(self):
        output = self.parent / 'changed'
        count = 0
        def changed(host, target, limit, **options):
            nonlocal count
            result = self.fake_request(host, target, limit, **options)
            if host == 'api.github.com' and target == acquire.API + 'runs/101':
                count += 1
                if count == 2:
                    path = output / IMAGE
                    path.chmod(0o600)
                    path.write_bytes(b'changed selected image')
            return result
        with mock.patch.object(acquire, 'request', side_effect=changed), self.assertRaisesRegex(ValueError, 'changed'):
            acquire.acquire(self.root, output, **self.inputs,
                token='synthetic-read-token', environment=self.environment)
        self.assertFalse((output / 'dmg-qualification.json').exists())


    def test_fixed_workflow_guard_rejects_arbitrary_source_before_checkout_or_repository_scripts(self):
        workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/macos-qualification.yml').read_text(encoding='utf-8')
        sections = workflow.split('\n  gate:\n', 1)[1].split('\n  qualify:\n', 1)
        self.assertEqual(len(sections), 2)
        for name, section in zip(['gate', 'qualify'], sections):
            with self.subTest(job=name):
                before = section.index('name: Require reviewed main invocation')
                checkout = section.index('name: Checkout reviewed main source')
                script = section.index('scripts/acquire_macos_qualification_inputs.py')
                self.assertLess(before, checkout)
                self.assertLess(checkout, script)
                self.assertIn('ref: '+ '$' + '{{ github.sha }}', section[checkout:script])
                self.assertNotIn('ref: '+ '$' + '{{ inputs.expected_commit }}', section)
                inline = section[before:checkout].split("python3 -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
                program = compile(textwrap.dedent(inline), '<fixed-workflow-invocation>', 'exec')
                environment = dict(self.environment, SELECTED_COMMIT=COMMIT)
                with mock.patch.dict(os.environ, environment, clear=True):
                    exec(program, {})
                for key, value in [('SELECTED_COMMIT', '0' * 40),
                    ('SELECTED_COMMIT', 'refs/pull/17/merge'), ('GITHUB_SHA', '0' * 40),
                    ('GITHUB_WORKFLOW_SHA', '0' * 40), ('GITHUB_REF', 'refs/heads/feature/0.3/attacker'),
                    ('GITHUB_REPOSITORY', 'fork/PeerBrush'), ('GITHUB_EVENT_NAME', 'pull_request'),
                    ('GITHUB_WORKFLOW_REF', 'Deftr0y/PeerBrush/.github/workflows/attacker.yml@refs/heads/main'),
                    ('RUNNER_OS', 'Linux'), ('RUNNER_ARCH', 'X64'),
                    ('GITHUB_RUN_ATTEMPT', '2'), ('RUNNER_ENVIRONMENT', 'self-hosted'),
                    ('PEERBRUSH_NATIVE_QUALIFICATION_ENABLED', '')]:
                    with self.subTest(context=key), mock.patch.dict(os.environ, dict(environment, **{key: value}), clear=True):
                        with self.assertRaises(SystemExit):
                            exec(program, {})

    def test_policy_and_failed_predecessor_reject_before_network_or_output(self):
        with mock.patch.object(acquire, 'request') as network, self.assertRaises(ValueError):
            acquire.acquire(self.root, self.parent / 'none', **self.inputs,
                token='synthetic-read-token', environment={})
        network.assert_not_called()
        self.assertFalse((self.parent / 'none').exists())
        self.run['run_attempt'] = 2
        with mock.patch.object(acquire, 'request', side_effect=self.fake_request), self.assertRaises(ValueError):
            acquire.acquire(self.root, self.parent / 'failed', **self.inputs,
                token='synthetic-read-token', environment=self.environment)
        self.assertFalse((self.parent / 'failed').exists())


class Response:
    def __init__(self, data=b'', *, status=200, headers=None):
        self.body, self.status = io.BytesIO(data), status
        self.headers = headers or {}
    def __enter__(self):
        return self
    def __exit__(self, *unused):
        return False
    def getheader(self, key):
        return self.headers.get(key)
    def getheaders(self):
        return list(self.headers.items())
    def read1(self, count):
        return self.body.read(count)
    def read(self, unused):
        raise AssertionError('Fill-read may postpone the total body deadline')


class BoundedHTTP(unittest.TestCase):
    def connection(self, response):
        connection = mock.Mock()
        connection.getresponse.return_value = response
        return connection

    def test_one_allowed_storage_redirect_never_forwards_authorization(self):
        location = 'https://fixture.blob.core.windows.net/artifact?sig=secret'
        first = self.connection(Response(status=302, headers={'Location': location}))
        second = self.connection(Response(b'body', headers={'Content-Length': '4'}))
        with mock.patch.object(acquire.http.client, 'HTTPSConnection', side_effect=[first, second]):
            self.assertEqual(acquire.request('api.github.com', acquire.API + 'artifacts/202/zip', 8192,
                token='synthetic-read-token', redirect=True), location)
            self.assertEqual(acquire.request('fixture.blob.core.windows.net', '/artifact?sig=secret', 16), b'body')
        self.assertEqual(first.request.call_args.kwargs['headers']['Authorization'], 'Bearer synthetic-read-token')
        self.assertNotIn('Authorization', second.request.call_args.kwargs['headers'])
        first.close.assert_called_once()
        second.close.assert_called_once()
        for url in ['http://fixture.blob.core.windows.net/file', 'https://example.invalid/file',
                    'https://fixture.blob.core.windows.net.evil.invalid/file', location + '#fragment',
                    'https://user@fixture.blob.core.windows.net/file', 'https://fixture.blob.core.windows.net:443/file']:
            with self.subTest(url=url):
                self.assertFalse(acquire.storage_url(url))
                connection = self.connection(Response(status=302, headers={'Location': url}))
                with mock.patch.object(acquire.http.client, 'HTTPSConnection', return_value=connection), self.assertRaises(ValueError):
                    acquire.request('api.github.com', acquire.API + 'artifacts/202/zip', 8192, token='synthetic', redirect=True)
        with self.assertRaises(ValueError):
            acquire.request('fixture.blob.core.windows.net', '/file', 16, token='must-not-leave-api')

    def test_read1_byte_length_status_and_total_body_deadline_fail_closed(self):
        for response, limit in [(Response(b'abc', headers={'Content-Length': '4'}), 16),
                                (Response(b'abcde'), 4),
                                (Response(b'body', status=302, headers={'Location': 'https://example.invalid'}), 16),
                                (Response(b'x', headers={'Content-Encoding': 'gzip'}), 16)]:
            connection = self.connection(response)
            with mock.patch.object(acquire.http.client, 'HTTPSConnection', return_value=connection), self.assertRaises(ValueError):
                acquire.request('api.github.com', acquire.API + 'artifacts/202', limit)
            connection.close.assert_called_once()
        connection = self.connection(Response(b'body'))
        with mock.patch.object(acquire.http.client, 'HTTPSConnection', return_value=connection), \
                mock.patch.object(acquire.time, 'monotonic', side_effect=[0., 1., 121.]), self.assertRaisesRegex(ValueError, 'deadline'):
            acquire.request('api.github.com', acquire.API + 'artifacts/202', 16)

    def test_cli_failure_output_does_not_expose_tokens_or_signed_locations(self):
        arguments = ['acquire', '--source-root', '/private', '--commit', COMMIT, '--run-id', '1',
            '--artifact-id', '2', '--artifact-sha256', 'a' * 64, '--archive-name', ARCHIVE,
            '--archive-sha256', 'b' * 64, '--image-name', IMAGE, '--image-sha256', 'c' * 64,
            '--team-id', 'ABCD123456', '--check-selection']
        error = io.StringIO()
        with mock.patch.object(acquire.sys, 'argv', arguments), \
                mock.patch.object(acquire, 'selected', side_effect=ValueError('secret-token signed-url')), \
                contextlib.redirect_stderr(error):
            self.assertEqual(acquire.main(), 1)
        self.assertNotIn('secret-token', error.getvalue())
        self.assertNotIn('signed-url', error.getvalue())


if __name__ == '__main__':
    unittest.main()
