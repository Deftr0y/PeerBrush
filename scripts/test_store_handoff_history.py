"""Official GET protocol doubles only; no credentials, Store writes or publication."""
import copy
import hashlib
import io
import json
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock
import zipfile

import publish_release as publisher
import store_handoff_history as history
import store_release_state as state
import submit_store_release as store
from test_submit_store_release import FakeStore, fixture


def zipped(value, name='submission.json'):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_STORED) as archive:
        entry = zipfile.ZipInfo(name)
        entry.external_attr = (stat.S_IFREG | 0o400) << 16
        archive.writestr(entry, value if isinstance(value, bytes) else json.dumps(value).encode())
    return output.getvalue()


class Protocol(unittest.TestCase):
    def setUp(self):
        self.env = dict(GITHUB_RUN_ID='102', GITHUB_SHA='b' * 40, GITHUB_WORKFLOW_SHA='b' * 40,
            GITHUB_REPOSITORY=history.transport.REPOSITORY, GITHUB_REPOSITORY_ID=str(history.transport.REPOSITORY_ID),
            GITHUB_REPOSITORY_OWNER_ID=str(history.transport.OWNER_ID), GITHUB_REF='refs/heads/main',
            GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_ATTEMPT='1', RUNNER_ENVIRONMENT='github-hosted',
            RUNNER_OS='Linux', RUNNER_ARCH='X64',
            GITHUB_WORKFLOW_REF=history.transport.REPOSITORY + '/' + history.WORKFLOW + '@refs/heads/main')
        repo = dict(id=history.transport.REPOSITORY_ID, full_name=history.transport.REPOSITORY,
            fork=False, private=False, owner=dict(id=history.transport.OWNER_ID))
        self.runs = [dict(id=101, run_number=1, run_attempt=1, path=history.WORKFLOW, head_branch='main',
            head_sha='a' * 40, pull_requests=[], event='workflow_dispatch', status='completed', conclusion='success',
            head_commit=dict(id='a' * 40), repository=copy.deepcopy(repo), head_repository=copy.deepcopy(repo))]
        self.runs.append(dict(self.runs[0], id=102, run_number=2, head_sha='b' * 40,
            head_commit=dict(id='b' * 40), status='in_progress', conclusion=None))
        self.jobs = dict(total_count=4, jobs=[dict(id=201+n, name=name, run_id=101, run_attempt=1,
            head_sha='a' * 40, head_branch='main', status='completed', conclusion='skipped', steps=[])
            for n, name in enumerate(['gate', 'compose', 'publish', 'site'])])
        self.provider = FakeStore()
        self.client = mock.Mock()
        self.client.request.side_effect = self.request
        self.client.receipt_bytes.side_effect = lambda identifier, size: self.raw
        self.client.package_bytes.side_effect = lambda identifier, size: self.package_raw
        self.client.release.return_value = dict(draft=False, prerelease=False, published_at='2026-01-01T00:00:00Z',
            tag_name='v0.26.0', target_commitish='a' * 40, assets=[dict(id=901, name='PeerBrush-0.26.0-source.zip')])
        self.client.commit.return_value = dict(sha='a' * 40)
        self.client.asset_hash.return_value = fixture()[2]['sha256']
        self.calls = []
        self.selected = history.History(self.client, self.provider, self.env)

    def request(self, method, route):
        self.assertEqual(method, 'GET')
        self.calls.append(route)
        prefix = '/repos/' + history.transport.REPOSITORY + '/actions/'
        if route == prefix + 'workflows/production.yml/runs?per_page=100&page=1':
            return dict(total_count=len(self.runs), workflow_runs=copy.deepcopy(self.runs))
        if route == prefix + 'runs/101/attempts/1/jobs?per_page=100&page=1': return copy.deepcopy(self.jobs)
        if route == prefix + 'runs/101/artifacts?per_page=100&page=1':
            return dict(total_count=len(self.artifacts), artifacts=copy.deepcopy(self.artifacts))
        if route == prefix + 'artifacts/501': return copy.deepcopy(self.artifact)
        if route == prefix + 'artifacts/502': return copy.deepcopy(self.package_artifact)
        raise AssertionError('Unknown synthetic history GET')

    def record(self, phase='new'):
        for job in self.jobs['jobs'][:2]: job.update(conclusion='success', labels=['ubuntu-24.04'])
        job = self.jobs['jobs'][2]
        job.update(conclusion='success', labels=['ubuntu-24.04'], steps=[dict(name=name, number=n+1,
            status='completed', conclusion='success') for n, name in enumerate(history.STEPS)])
        report = fixture()[2]
        self.receipt = dict(schema=1, tag='v0.26.0', commit=report['commit'], package=fixture()[0], sha256=report['sha256'],
            version=report['version'], phase=phase, submission_id=None if phase in {'new', 'create-requested'} else '456',
            service_status=None)
        self.refresh()

    def refresh(self):
        self.raw = zipped(self.receipt)
        self.artifact = dict(id=501, name='store-submission-reconciliation-101-1', expired=False,
            size_in_bytes=len(self.raw), digest='sha256:' + hashlib.sha256(self.raw).hexdigest(),
            url='https://api.github.com' + history.transport.API + 'artifacts/501',
            archive_download_url='https://api.github.com' + history.transport.API + 'artifacts/501/zip',
            workflow_run=dict(id=101, repository_id=history.transport.REPOSITORY_ID,
                head_repository_id=history.transport.REPOSITORY_ID, head_branch='main', head_sha='a' * 40))
        self.artifacts = [self.artifact] + ([self.package_artifact] if hasattr(self, 'package_artifact') else [])

    def published(self):
        self.record('committed')
        self.package_raw = zipped(fixture()[1], fixture()[0])
        self.package_artifact = dict(copy.deepcopy(self.artifact), id=502, name='store-submission-input-101-1',
            size_in_bytes=len(self.package_raw), digest='sha256:' + hashlib.sha256(self.package_raw).hexdigest(),
            url='https://api.github.com' + history.transport.API + 'artifacts/502',
            archive_download_url='https://api.github.com' + history.transport.API + 'artifacts/502/zip')
        self.artifacts.append(self.package_artifact)
        self.provider.app['lastPublishedApplicationSubmission'] = {'id': '456'}
        self.submission = dict(id='456', applicationPackages=[dict(fileName=fixture()[0], fileStatus='Uploaded',
            architecture='x64', version='1.26.0.0')])
        self.status = 'Published'
        original = self.provider.call
        def get(path='', method='GET', data=None):
            self.assertEqual(method, 'GET')
            if path == '/submissions/456':
                self.provider.calls.append((path, method, data))
                return copy.deepcopy(self.submission)
            if path == '/submissions/456/status':
                self.provider.calls.append((path, method, data))
                return dict(status=self.status)
            return original(path, method, data)
        self.provider.call = get

    def test_skipped_or_explicit_no_attempt_history_never_creates_write_or_public_authority(self):
        self.assertEqual(self.selected.check()['prior_attempts'], 0)
        self.record()
        self.selected = history.History(self.client, self.provider, self.env)
        result = self.selected.check()
        self.assertEqual(result, dict(prior_attempts=1, mutation_authority=False, public_availability='unverified'))
        self.assertTrue(all(method == 'GET' for _, method, _ in self.provider.calls))

    def test_genuine_service_observation_and_actual_package_bytes_are_required_and_rechecked(self):
        self.published()
        self.selected.check()
        self.client.package_bytes.assert_called_once()
        self.selected.check()
        self.client.package_bytes.assert_called_once()
        self.client.asset_hash.assert_not_called()
        self.artifact['expired'] = True
        with self.assertRaises(store.SafeError): self.selected.check()

    def test_private_package_requires_matching_origin_retention_and_actual_inner_bytes(self):
        self.published()
        original = copy.deepcopy(self.package_artifact)
        for change in ['expired', 'source', 'missing', 'payload', 'name']:
            self.package_artifact.clear()
            self.package_artifact.update(copy.deepcopy(original))
            self.artifacts = [self.artifact, self.package_artifact]
            self.package_raw = zipped(fixture()[1], fixture()[0])
            if change == 'expired': self.package_artifact['expired'] = True
            elif change == 'source': self.package_artifact['workflow_run']['head_sha'] = 'f' * 40
            elif change == 'missing': self.artifacts.pop()
            else:
                self.package_raw = zipped(b'foreign' if change == 'payload' else fixture()[1],
                                          'foreign.msix' if change == 'name' else fixture()[0])
                self.package_artifact.update(size_in_bytes=len(self.package_raw),
                    digest='sha256:' + hashlib.sha256(self.package_raw).hexdigest())
            with self.subTest(change=change), self.assertRaises(store.SafeError):
                history.History(self.client, self.provider, self.env).check()
        self.assertTrue(all(method == 'GET' for _, method, _ in self.provider.calls))

    def test_crash_before_receipt_lost_create_response_and_expiry_hold_even_with_no_pending_draft(self):
        for phase in ['missing', 'expired', 'create-requested', 'created', 'commit-requested', 'committed']:
            self.record('new' if phase in {'missing', 'expired'} else phase)
            self.selected = history.History(self.client, self.provider, self.env)
            if phase == 'missing': self.artifacts = []
            elif phase == 'expired': self.artifact['expired'] = True
            with self.subTest(phase=phase), self.assertRaises(store.SafeError): self.selected.check()
        self.assertTrue(all(method == 'GET' for _, method, _ in self.provider.calls))

    def test_incomplete_deleted_rerun_foreign_or_competing_history_fails_closed(self):
        initial, jobs = copy.deepcopy(self.runs), copy.deepcopy(self.jobs)
        for change in ['deleted', 'rerun', 'foreign', 'source', 'step', 'running', 'workflow', 'jobs',
                'overflow', 'prerequisite', 'head', 'unknown-status', 'unknown-conclusion']:
            self.runs, self.jobs = copy.deepcopy(initial), copy.deepcopy(jobs)
            if change == 'deleted': self.runs.pop(0)
            elif change == 'rerun': self.runs[0]['run_attempt'] = 2
            elif change == 'foreign': self.runs[0]['repository']['owner']['id'] = 9
            elif change == 'source': self.jobs['jobs'][2]['head_sha'] = 'e' * 40
            elif change == 'running': self.runs[0]['status'] = 'in_progress'
            elif change == 'workflow': self.runs[0]['path'] = '.github/workflows/store-submit.yml'
            elif change == 'jobs': self.jobs['jobs'].pop()
            elif change == 'overflow': self.runs[1]['run_number'] = 1001
            elif change == 'head': self.runs[0]['head_commit']['id'] = 'e' * 40
            elif change == 'unknown-status': self.runs[0]['status'] = 'unknown'
            elif change == 'unknown-conclusion': self.runs[0]['conclusion'] = None
            elif change == 'prerequisite':
                self.record()
                self.jobs['jobs'][0]['conclusion'] = 'failure'
            else:
                self.record()
                self.jobs['jobs'][2]['steps'].append(dict(name='Unreviewed writer', number=8, status='completed', conclusion='success'))
            with self.subTest(change=change), self.assertRaises(store.SafeError):
                history.History(self.client, self.provider, self.env).check()

    def test_nonterminal_store_status_competing_draft_or_changed_package_cannot_clear_history(self):
        self.published()
        original = copy.deepcopy(self.submission)
        original_artifact = copy.deepcopy(self.package_artifact)
        for change in ['processing', 'rejected', 'pending', 'package', 'hash', 'source', 'bytes']:
            self.status, self.submission = 'Published', copy.deepcopy(original)
            self.provider.app.pop('pendingApplicationSubmission', None)
            self.client.commit.return_value = dict(sha='a' * 40)
            self.package_artifact.clear()
            self.package_artifact.update(copy.deepcopy(original_artifact))
            self.package_raw = zipped(fixture()[1], fixture()[0])
            if change == 'processing': self.status = 'Release'
            elif change == 'rejected': self.status = 'CertificationFailed'
            elif change == 'pending': self.provider.app['pendingApplicationSubmission'] = {'id': '999'}
            elif change == 'package': self.submission['applicationPackages'][0]['fileName'] = 'foreign.msix'
            elif change == 'hash': self.package_artifact['digest'] = 'sha256:' + 'f' * 64
            elif change == 'source': self.client.commit.return_value = dict(sha='f' * 40)
            else: self.package_raw += b'changed'
            with self.subTest(change=change), self.assertRaises(store.SafeError):
                history.History(self.client, self.provider, self.env).check()

    def test_late_history_change_cannot_be_ignored_and_new_queued_run_has_no_write_permit(self):
        self.record()
        self.selected.check()
        self.receipt['sha256'] = 'f' * 64
        self.refresh()
        with self.assertRaises(store.SafeError): self.selected.check()
        self.runs.append(dict(self.runs[1], id=103, run_number=3, status='queued'))
        self.assertFalse(history.History(self.client, self.provider, self.env).check()['mutation_authority'])
        self.runs[2]['status'] = 'in_progress'
        with self.assertRaises(store.SafeError): history.History(self.client, self.provider, self.env).check()

    def test_receipt_unknown_fields_and_transport_bytes_never_become_authority(self):
        self.record()
        for change in ['private', 'source', 'outer', 'extra']:
            self.record()
            if change == 'private': self.receipt['SYNTHETIC_PRIVATE_FIELD'] = 'SYNTHETIC_SECRET_MUST_NOT_ESCAPE'
            elif change == 'source': self.receipt['commit'] = 'f' * 40
            elif change == 'extra': self.artifacts.append(copy.deepcopy(self.artifact))
            else: self.raw += b'changed'
            if change in {'private', 'source'}: self.refresh()
            with self.subTest(change=change), self.assertRaises((store.SafeError, ValueError)) as error:
                history.History(self.client, self.provider, self.env).check()
            self.assertNotIn('SYNTHETIC_SECRET', str(error.exception))

    def test_actual_receipt_transport_does_not_forward_job_token_to_storage(self):
        client = publisher.GitHub('SYNTHETIC_JOB_TOKEN')
        self.record()
        with mock.patch.object(history.transport, 'request', side_effect=['https://results-receiver.actions.githubusercontent.com/selected', self.raw]) as get:
            self.assertEqual(client.receipt_bytes(501, len(self.raw)), self.raw)
        self.assertEqual(get.call_args_list[0].kwargs, dict(token='SYNTHETIC_JOB_TOKEN', redirect=True))
        self.assertNotIn('token', get.call_args_list[1].kwargs)

    def test_large_private_package_download_uses_bounded_file_transport(self):
        client = publisher.GitHub('SYNTHETIC_JOB_TOKEN')
        data = b'x' * (1024 * 1024 + 1)
        def download(host, route, limit, **kwargs):
            if host == 'api.github.com':
                return 'https://results-receiver.actions.githubusercontent.com/selected'
            self.assertNotIn('token', kwargs)
            self.assertEqual(limit, len(data))
            with kwargs['destination'].open('xb') as stream:
                stream.write(data)
        with mock.patch.object(history.transport, 'request', side_effect=download):
            self.assertEqual(client.package_bytes(502, len(data)), data)
        with mock.patch.object(history.transport, 'request', side_effect=['https://results-receiver.actions.githubusercontent.com/selected', b'archive']) as get:
            self.assertEqual(client.package_bytes(502, 100), b'archive')
        self.assertEqual(get.call_args_list[0].kwargs, dict(token='SYNTHETIC_JOB_TOKEN', redirect=True))
        self.assertNotIn('token', get.call_args_list[1].kwargs)

    def test_actions_routes_are_read_only_and_other_workflows_or_mutations_are_refused(self):
        client = publisher.GitHub('SYNTHETIC_JOB_TOKEN')
        root = '/repos/' + history.transport.REPOSITORY + '/actions/'
        for route in ['workflows/store-submit.yml/runs?per_page=100&page=1',
                'workflows/production.yml/runs?per_page=100&page=11', 'runs/101/attempts/2/jobs?per_page=100&page=1']:
            with self.subTest(route=route), self.assertRaises(ValueError): client.request('GET', root + route)
        for method in ['POST', 'PATCH', 'DELETE']:
            with self.subTest(method=method), self.assertRaises(ValueError):
                client.request(method, root + 'artifacts/501')
        for route in ['workflows/production.yml/runs?per_page=100&page=1',
                'runs/101/attempts/1/jobs?per_page=100&page=1', 'runs/101/artifacts?per_page=100&page=1', 'artifacts/501']:
            connection = mock.MagicMock()
            response = connection.getresponse.return_value.__enter__.return_value
            response.status = 200
            response.getheader.return_value = None
            response.getheaders.return_value = []
            response.read1.side_effect = [b'{"synthetic":"read-only"}', b'']
            with self.subTest(route=route), mock.patch.object(publisher.http.client, 'HTTPSConnection', return_value=connection):
                self.assertEqual(client.request('GET', root + route), {'synthetic':'read-only'})
            self.assertEqual(connection.request.call_args.args[:2], ('GET', root + route))
            connection.close.assert_called_once()

    def test_complete_pagination_is_required_and_neither_gaps_nor_changed_totals_are_accepted(self):
        rows = [dict(copy.deepcopy(self.runs[0]), id=1000+n, run_number=n) for n in range(1, 102)]
        rows[-1].update(head_sha='b' * 40, head_commit=dict(id='b' * 40), status='in_progress', conclusion=None)
        environment = dict(self.env, GITHUB_RUN_ID='1101')
        for change in ['none', 'empty', 'duplicate', 'total', 'overflow']:
            client = mock.Mock()
            def request(method, route):
                self.assertEqual(method, 'GET')
                page = int(route.rsplit('=', 1)[1])
                selected = copy.deepcopy(rows[:100] if page == 1 else rows[100:])
                total = len(rows)
                if change == 'overflow': total = 1001
                if page == 2:
                    if change == 'empty': selected = []
                    elif change == 'duplicate': selected = [rows[0]]
                    elif change == 'total': total += 1
                return dict(total_count=total, workflow_runs=selected)
            client.request.side_effect = request
            selected = history.History(client, self.provider, environment)
            selected.deadline = history.time.monotonic() + 180
            if change == 'none': self.assertEqual(len(selected.runs()), 100)
            else:
                with self.subTest(change=change), self.assertRaises(store.SafeError): selected.runs()

    def test_invalid_invocations_and_read_deadlines_fail_before_any_get_or_secret_echo(self):
        for key, value in [('GITHUB_RUN_ID','01'), ('GITHUB_RUN_ATTEMPT','2'),
                ('GITHUB_REPOSITORY_OWNER_ID','0'), ('RUNNER_ENVIRONMENT','self-hosted'),
                ('GITHUB_SHA','SYNTHETIC_PRIVATE_INPUT')]:
            with self.subTest(key=key), self.assertRaises(store.SafeError) as error:
                history.History(self.client, self.provider, dict(self.env, **{key:value}))
            self.assertNotIn('SYNTHETIC_PRIVATE_INPUT', str(error.exception))
        with mock.patch.object(history.time,'monotonic',side_effect=[0,181]), self.assertRaises(store.SafeError): self.selected.check()
        self.assertEqual(self.calls, [])

    def test_late_fence_runs_before_each_actual_writer_permission_and_can_stop_a_partial_attempt(self):
        for fail_at in [None, 1, 2, 3, 4]:
            with self.subTest(fail_at=fail_at), tempfile.TemporaryDirectory(prefix='peerbrush-store-fence-order-') as temporary:
                journal = state.Journal(Path(temporary).resolve() / 'receipt.json')
                journal.bind('v0.26.0', fixture()[0], fixture()[2])
                provider = FakeStore()
                call = provider.call
                def observed(path='', method='GET', data=None):
                    if path == '/submissions/456' and method == 'GET': return copy.deepcopy(provider.draft)
                    result = call(path, method, data)
                    if path == '/submissions' and method == 'POST': provider.app['pendingApplicationSubmission'] = {'id':'456'}
                    if method == 'PUT': provider.draft = copy.deepcopy(data)
                    return result
                provider.call = observed
                selected = publisher._StoreWrites(provider)
                selected.history = mock.Mock()
                phases = []
                def check(**kwargs):
                    phases.append(selected.phase)
                    if len(phases) == fail_at: raise store.SafeError('Prior receipt became unavailable')
                selected.history.check.side_effect = check
                with mock.patch.object(store, 'request', return_value=b'') as upload:
                    if fail_at is None:
                        store.submit(selected, *fixture(), execute=True, journal=journal)
                        self.assertEqual(phases, ['new','created','updated','uploaded'])
                    else:
                        with self.assertRaises(store.SafeError): store.submit(selected, *fixture(), execute=True, journal=journal)
                        self.assertEqual(len(phases), fail_at)
                        self.assertEqual(sum(method == 'POST' for _, method, _ in provider.calls), 0 if fail_at == 1 else 1)
                        self.assertEqual(upload.call_count, 1 if fail_at == 4 else 0)

    def test_hosted_journal_exists_before_publication_and_survives_an_early_failure_as_no_attempt(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-hosted-store-journal-') as temporary:
            base = Path(temporary).resolve()
            source, files, runner = base / 'source', base / 'files', base / 'runner'
            for directory in [source, files, runner]: directory.mkdir()
            (source / '.git').mkdir()
            name, package, report = fixture()
            raw = json.dumps(report).encode()
            (files / (name + '.json')).write_bytes(raw)
            candidate = mock.Mock(root=source, directory=files,
                manifest=dict(version='0.26.0', commit=report['commit'], channels={'microsoft-store': {'file': name}}))
            records = {name: dict(sha256=report['sha256']), name + '.json': dict(sha256=hashlib.sha256(raw).hexdigest())}
            with mock.patch.object(publisher, 'validate_manifest', return_value=records), self.assertRaises(store.RemoteError):
                with publisher._store_journal(candidate, dict(RUNNER_TEMP=str(runner))) as journal:
                    saved = state.Journal(runner / 'peerbrush-store-handoff/submission.json').state
                    self.assertEqual(saved['phase'], 'new')
                    raise store.RemoteError(None)
            self.assertEqual(state.Journal(runner / 'peerbrush-store-handoff/submission.json').state['phase'], 'new')
            with mock.patch.object(publisher, 'validate_manifest', return_value=records), self.assertRaises(ValueError):
                with publisher._store_journal(candidate, dict(RUNNER_TEMP=str(runner))): self.fail('Must not reuse partial host state')


if __name__ == '__main__':
    unittest.main()
