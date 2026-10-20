"""Offline Store journal/reconciliation tests; no authentication or mutations."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import store_release_state as state
import submit_store_release as submit
from test_submit_store_release import FakeStore, fixture


class Tests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-store-state-')
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name).resolve() / 'release.json'
        self.journal = state.Journal(self.path)
        self.journal.bind('v0.26.0', fixture()[0], fixture()[2])

    def observed_store(self, status):
        store = FakeStore()
        store.submission = dict(id='456', applicationPackages=[dict(
            fileName=fixture()[0], fileStatus='Uploaded', version='1.26.0.0', architecture='x64')])
        original = store.call
        def call(path='', method='GET', data=None):
            if path == '/submissions/456':
                store.calls.append((path, method, data))
                return copy.deepcopy(store.submission)
            if path == '/submissions/456/status':
                store.calls.append((path, method, data))
                return {'status': status}
            return original(path, method, data)
        store.call = call
        self.journal.phase('created', '456')
        return store

    def test_certification_publication_and_availability_remain_separate(self):
        for status, category in state.STATUSES.items():
            store = self.observed_store(status)
            result = state.reconcile(store, self.journal)
            self.assertEqual(result['service_state'], category)
            self.assertEqual(result['public_availability'], 'unverified')
            self.assertFalse(result['can_submit'])
            self.assertEqual(state.Journal(self.path).state['service_status'], status)
            self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    def test_no_duplicate_downgrade_or_unrelated_pending_replacement(self):
        store = FakeStore()
        self.assertTrue(state.reconcile(store, self.journal)['can_submit'])
        store.app['pendingApplicationSubmission'] = {'id': '999'}
        self.assertFalse(state.reconcile(store, self.journal)['can_submit'])
        del store.app['pendingApplicationSubmission']
        for live in ['1.26.0.0', '2.0.0.0']:
            store.current['applicationPackages'][0]['version'] = live
            self.assertFalse(state.reconcile(store, self.journal)['can_submit'])
        self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    def test_uncertain_create_blocks_retry_even_before_service_discovery(self):
        failed = FakeStore()
        original = failed.call
        def disconnect(path='', method='GET', data=None):
            if method == 'POST':
                failed.calls.append((path, method, data))
                raise submit.RemoteError(None)
            return original(path, method, data)
        failed.call = disconnect
        with self.assertRaises(submit.RemoteError):
            submit.submit(failed, *fixture(), execute=True, journal=self.journal)
        self.assertEqual(sum(method == 'POST' for _, method, _ in failed.calls), 1)
        resumed = state.Journal(self.path)
        self.assertEqual(state.reconcile(FakeStore(), resumed)['service_state'], 'create-outcome-uncertain')
        store = FakeStore()
        with self.assertRaises(submit.SafeError):
            submit.submit(store, *fixture(), execute=True, journal=resumed)
        self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    def test_every_mutating_caller_requires_matching_durable_bytes(self):
        store = FakeStore()
        with self.assertRaises(submit.SafeError):
            submit.submit(store, *fixture(), execute=True)
        name, package, report = fixture()
        with self.assertRaises(submit.SafeError):
            submit.submit(store, name, package + b'changed', report, execute=True, journal=self.journal)
        with self.assertRaises(submit.SafeError):
            submit.submit(store, name, package, dict(report, commit='b' * 40), execute=True, journal=self.journal)
        self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    @patch.object(submit, 'request', return_value=b'')
    def test_writer_saves_ids_and_intent_before_remote_mutations(self, upload):
        store = FakeStore()
        original = store.call
        def call(path='', method='GET', data=None):
            saved = state.Journal(self.path).state
            if path == '/submissions' and method == 'POST':
                self.assertEqual(saved['phase'], 'create-requested')
            if method == 'PUT':
                self.assertEqual(saved['submission_id'], '456')
                self.assertEqual(saved['phase'], 'created')
            if path.endswith('/commit'):
                self.assertEqual(saved['phase'], 'commit-requested')
            return original(path, method, data)
        store.call = call
        submit.submit(store, *fixture(), execute=True, journal=self.journal)
        saved = state.Journal(self.path).state
        self.assertEqual(saved['phase'], 'committed')
        self.assertEqual(saved['service_status'], 'PreProcessing')

    def test_transient_get_retries_are_bounded_and_auth_failure_is_not_retried(self):
        store = FakeStore()
        for status, attempts in [(503, 3), (None, 3), (401, 1), (404, 1)]:
            with patch.object(store, 'call', side_effect=submit.RemoteError(status)) as request:
                sleeps = []
                with self.assertRaises(submit.RemoteError):
                    state.get(store, sleep=sleeps.append)
                self.assertEqual(request.call_count, attempts)
                self.assertEqual(sleeps, [1, 2] if attempts == 3 else [])
        with self.assertRaises(submit.SafeError):
            state.get(store, attempts=4)

    def test_identity_packages_status_and_immutable_bytes_fail_closed(self):
        store = self.observed_store('Published')
        store.submission['applicationPackages'][0]['version'] = '1.27.0.0'
        with self.assertRaises(submit.SafeError):
            state.reconcile(store, self.journal)
        store = self.observed_store('UnexpectedStatus')
        with self.assertRaises(submit.SafeError):
            state.reconcile(store, self.journal)
        report = dict(fixture()[2], sha256='b' * 64)
        with self.assertRaises(submit.SafeError):
            self.journal.bind('v0.26.0', fixture()[0], report)
        saved = self.path.read_bytes()
        store = self.observed_store('Published')
        store.app['publisherName'] = 'other'
        with self.assertRaises(submit.SafeError):
            state.reconcile(store, self.journal)
        self.assertEqual(self.path.read_bytes(), saved)

    def test_private_lock_releases_and_corrupt_receipts_are_preserved(self):
        with state.locked(self.path):
            with self.assertRaises(submit.SafeError):
                with state.locked(self.path):
                    pass
        with state.locked(self.path) as journal:
            self.assertEqual(journal.state['phase'], 'new')
        self.path.write_text('{"private": "not a receipt"}')
        with self.assertRaises(submit.SafeError):
            state.Journal(self.path)
        self.assertIn('private', self.path.read_text())

    def test_private_receipts_and_locks_cannot_enter_any_source_checkout(self):
        for root in state.source_roots():
            path = root / 'scripts' / 'synthetic-private-state.json'
            self.assertFalse(path.exists())
            with self.assertRaises(submit.SafeError):
                state.Journal(path)
            with self.assertRaises(submit.SafeError):
                with state.locked(path):
                    pass
            self.assertFalse(path.exists())
            self.assertFalse(path.with_name(path.name + '.lock').exists())

    def test_symlink_or_junction_parent_is_rejected_before_lock_creation(self):
        directory = self.path.parent / 'private'
        directory.mkdir()
        alias = self.path.parent / 'alias'
        if os.name == 'nt':
            subprocess.run(['cmd', '/c', 'mklink', '/J', str(alias), str(directory)], check=True, capture_output=True)
        else:
            alias.symlink_to(directory, target_is_directory=True)
        try:
            for path in [alias / 'receipt.json', alias / 'nested' / 'receipt.json']:
                if path.parent.name == 'nested':
                    (directory / 'nested').mkdir()
                with self.assertRaises(submit.SafeError):
                    state.Journal(path)
                with self.assertRaises(submit.SafeError):
                    with state.locked(path):
                        pass
            self.assertFalse(list(directory.glob('*.lock')))
        finally:
            alias.rmdir() if os.name == 'nt' else alias.unlink()

    def test_independent_checkout_and_worktree_markers_reject_receipts_without_writing(self):
        original = self.path.read_bytes()
        for kind in ['clone', 'worktree']:
            checkout = self.path.parent / kind
            checkout.mkdir()
            marker = checkout / '.git'
            if kind == 'clone':
                marker.mkdir()
            else:
                marker.write_text('gitdir: /synthetic/elsewhere\n')
            nested = checkout / 'nested'
            nested.mkdir()
            path = nested / 'receipt.json'
            path.write_bytes(original)
            self.assertNotIn(checkout, state.source_roots())
            for action in [state.private_path, state.Journal]:
                with self.subTest(kind=kind, action=action.__name__), self.assertRaises(submit.SafeError):
                    action(path)
            with self.assertRaises(submit.SafeError):
                with state.locked(path):
                    self.fail('Receipt lock must not open in an independent checkout')
            self.assertEqual(path.read_bytes(), original)
            self.assertEqual(sorted(p.name for p in nested.iterdir()), ['receipt.json'])
        private = self.path.parent / 'adjacent-private'
        private.mkdir()
        with state.locked(private / 'receipt.json') as journal:
            journal.bind('v0.26.0', fixture()[0], fixture()[2])
        self.assertEqual(state.Journal(private / 'receipt.json').state['phase'], 'new')

    def test_submit_rejects_selected_source_state_before_download_or_authentication(self):
        selected = self.path.parent / 'selected-release-source'
        selected.mkdir()
        (selected / '.git').mkdir()
        nested = selected / 'private'
        nested.mkdir()
        path = nested / 'receipt.json'
        for execute in [False, True]:
            args = ['submit', '--tag', 'v0.26.0', '--source-root', str(selected), '--state', str(path)]
            if execute:
                args.append('--submit')
            with patch.object(submit.sys, 'argv', args), patch.object(submit, 'released_assets') as remote, patch.object(submit, 'Store') as authentication:
                self.assertEqual(submit.main(), 1)
                remote.assert_not_called()
                authentication.assert_not_called()
            self.assertEqual(list(nested.iterdir()), [])

    def test_receipt_fields_require_strings_and_canonical_bounded_identifiers(self):
        original = self.path.read_bytes()
        initial = json.loads(original)
        for field, value in [('tag', 26), ('commit', 0), ('sha256', 0), ('package', 123),
                             ('version', 12600), ('submission_id', 456),
                             ('submission_id', '0456'), ('submission_id', '1' * 33),
                             ('version', '1.026.0.0'), ('tag', 'v0.026.0'),
                             ('phase', []), ('service_status', {})]:
            altered = dict(initial, **{field: value})
            if field == 'submission_id':
                altered['phase'] = 'created'
            self.path.write_text(json.dumps(altered))
            with self.subTest(field=field, value=value), self.assertRaises(submit.SafeError):
                state.Journal(self.path)
            self.assertEqual(json.loads(self.path.read_text()), altered)
        self.path.write_bytes(original)

    def test_submit_cli_requires_private_state_before_any_remote_access(self):
        with patch.object(submit.sys, 'argv', ['submit', '--tag', 'v0.26.0', '--submit']), patch.object(submit, 'released_assets') as remote:
            self.assertEqual(submit.main(), 1)
            remote.assert_not_called()


if __name__ == '__main__':
    unittest.main()
