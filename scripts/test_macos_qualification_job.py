"""Fake public GET checks only; never contact GitHub or execute a verifier."""
import contextlib
import copy
import io
import json
import sys
import unittest
from unittest import mock

import prepare_site_release as website
import production_readiness as readiness
import test_dmg_handoff
import test_macos_qualification_attestation as fixtures
import verify_macos_qualification as native


class Readback(unittest.TestCase):
    def setUp(self):
        self.trust = fixtures.Authentication(methodName='runTest')
        self.trust.setUp()
        self.addCleanup(self.trust.doCleanups)
        self.serial = 0
        self.calls, self.raw, self.transport_error = [], {}, None
        self.base = 'https://api.github.com/repos/' + native.REPOSITORY + '/actions/runs/12345/attempts/1'
        repository = dict(id=int(native.REPOSITORY_ID), full_name=native.REPOSITORY,
                          private=False, fork=False, owner=dict(id=int(native.OWNER_ID)))
        self.run = dict(id=12345, run_attempt=1, head_sha='a' * 40, head_branch='main',
                        path=native.WORKFLOW, event='workflow_dispatch', status='completed',
                        conclusion='success', pull_requests=[], head_commit=dict(id='a' * 40),
                        repository=repository, head_repository=copy.deepcopy(repository),
                        jobs_url='https://PRIVATE_SENTINEL.invalid/latest',
                        actor=dict(login='PRIVATE_SENTINEL'))
        def job(name, identifier):
            return dict(id=identifier, name=name, run_id=12345, run_attempt=1,
                        head_sha='a' * 40, head_branch='main', status='completed',
                        conclusion='success', labels=['macos-26'], runner_name='PRIVATE_SENTINEL')
        names = ['Set up job', *native.REQUIRED_JOB_STEPS, 'Complete job']
        qualifier = job('qualify', 902)
        qualifier['steps'] = [dict(number=index + 1, name=name, status='completed', conclusion='success')
                              for index, name in enumerate(names)]
        self.jobs = dict(total_count=2, jobs=[job('gate', 901), qualifier])
        self.original_run, self.original_jobs = copy.deepcopy(self.run), copy.deepcopy(self.jobs)

    def get(self, url, limit, *, api=False):
        self.calls.append((url, limit, api))
        self.assertIn(url, [self.base, self.base + '/jobs'])
        self.assertEqual(limit, native.API_LIMIT)
        self.assertIs(api, True)
        if self.transport_error is not None:
            raise self.transport_error
        if url in self.raw:
            return self.raw[url]
        return json.dumps(self.run if url == self.base else self.jobs).encode()

    def authenticate(self, **changes):
        self.serial += 1
        values = dict(readback_job=True, work=self.trust.base / ('service-' + str(self.serial)))
        values.update(changes)
        with mock.patch.object(website, 'public_get', side_effect=self.get):
            return self.trust.authenticate(**values)

    def reject(self):
        with self.assertRaisesRegex(ValueError, '^Authenticated native qualification does not match a successful exact-attempt service readback.$'):
            self.authenticate()

    def test_default_remains_offline_and_readback_requires_a_boolean(self):
        result = self.authenticate(readback_job=False)
        self.assertEqual(self.calls, [])
        self.assertEqual(result['native_job_conclusion'], 'not-service-reconciled')
        self.assertNotIn('native_service', result)
        for value in [None, 0, 1, 'true', {}]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.authenticate(readback_job=value)
        self.assertEqual(self.calls, [])

    def test_exact_attempt_success_is_sanitized_without_installation_or_readiness(self):
        for path in [native.WORKFLOW, native.WORKFLOW + '@main']:
            self.run['path'] = path
            # The documented job example omits run_attempt. The exact-attempt
            # route is authoritative; when the field exists it must also match.
            for job in self.jobs['jobs']:
                job.pop('run_attempt', None)
            self.calls.clear()
            result = self.authenticate()
            self.assertEqual(self.calls, [(self.base, native.API_LIMIT, True),
                                         (self.base + '/jobs', native.API_LIMIT, True)])
            self.assertEqual(result['native_job_conclusion'], 'service-reconciled-success')
            self.assertEqual(result['native_service']['job_id'], '902')
            self.assertEqual(result['native_service']['run_attempt'], '1')
            self.assertEqual(result['native_service']['required_steps'], list(native.REQUIRED_JOB_STEPS))
            for key in ['candidate_verified', 'ready', 'production_ready']:
                self.assertIs(result[key], False)
            self.assertEqual(result['installed_acceptance'], 'not-performed')
            self.assertEqual(result['public_availability'], 'unverified')
            self.assertNotIn('PRIVATE_SENTINEL', json.dumps(result))

    def test_only_successful_crypto_claims_can_start_readback(self):
        with self.assertRaises(TypeError):
            self.authenticate(native_service={'conclusion': 'success'})
        self.assertEqual(self.calls, [])
        original = self.trust.command
        def rejected(executable, arguments, work, label):
            if label == 'verification':
                raise ValueError('PRIVATE_SENTINEL')
            return original(executable, arguments, work, label)
        self.trust.command = rejected
        with self.assertRaises(ValueError):
            self.authenticate()
        self.assertEqual(self.calls, [])
        self.trust.command = original
        # The dormant producer is dispatch-only, attempt 1. A signed latest
        # rerun or a signed push must not be substituted for that contract.
        def rerun(executable, arguments, work, label):
            result = original(executable, arguments, work, label)
            if label == 'verification':
                value = json.loads(result)
                value[0]['verificationResult']['signature']['certificate']['runInvocationURI'] = (
                    native.REPOSITORY_URI + '/actions/runs/12345/attempts/2')
                return json.dumps(value).encode()
            return result
        self.trust.command = rerun
        with self.assertRaises(ValueError):
            self.authenticate(run_attempt='2')
        self.assertEqual(self.calls, [])
        def push(executable, arguments, work, label):
            result = original(executable, arguments, work, label)
            if label == 'verification':
                value = json.loads(result)
                value[0]['verificationResult']['signature']['certificate']['buildTrigger'] = 'push'
                return json.dumps(value).encode()
            return result
        self.trust.command = push
        self.reject()
        self.assertEqual(self.calls, [])

    def test_wrong_run_attempt_fork_source_trigger_and_completion_are_rejected(self):
        variants = [
            (('id',), 12346), (('id',), True), (('run_attempt',), 2), (('run_attempt',), '1'),
            (('head_sha',), 'b' * 40), (('head_commit', 'id'), 'b' * 40),
            (('head_branch',), 'feature/0.3/untrusted'), (('path',), native.WORKFLOW + '@other'),
            (('event',), 'push'), (('event',), 'pull_request'), (('status',), 'in_progress'),
            (('conclusion',), 'failure'), (('pull_requests',), [{'number': 1}]),
        ]
        for field in ['repository', 'head_repository']:
            variants.extend([((field, 'id'), 2), ((field, 'id'), native.REPOSITORY_ID),
                             ((field, 'full_name'), 'fork/PeerBrush'), ((field, 'fork'), True),
                             ((field, 'private'), True), ((field, 'owner', 'id'), 3)])
        for fields, value in variants:
            self.run = copy.deepcopy(self.original_run)
            target = self.run
            for field in fields[:-1]:
                target = target[field]
            target[fields[-1]] = value
            self.calls.clear()
            with self.subTest(fields=fields, value=value):
                self.reject()
                self.assertEqual(len(self.calls), 1, 'Do not fetch jobs after a rejected run')

    def test_wrong_jobs_labels_attempts_duplicates_and_pagination_are_rejected(self):
        for field, value in [('run_id', 999), ('run_attempt', 2), ('run_attempt', True),
                             ('head_sha', 'b' * 40), ('head_branch', 'other'),
                             ('status', 'queued'), ('conclusion', 'skipped'), ('id', True),
                             ('name', 'qualify (other)'), ('labels', ['self-hosted', 'macos-26']),
                             ('labels', ['macos-25']), ('labels', ['macos-26'] * 9),
                             ('labels', ['macos-26', ['invalid']])]:
            self.jobs = copy.deepcopy(self.original_jobs)
            self.jobs['jobs'][1][field] = value
            with self.subTest(field=field, value=value):
                self.reject()
        for change in ['duplicate-id', 'duplicate-name', 'missing', 'extra', 'total', 'bool-total']:
            self.jobs = copy.deepcopy(self.original_jobs)
            if change == 'duplicate-id':
                self.jobs['jobs'][0]['id'] = self.jobs['jobs'][1]['id']
            elif change == 'duplicate-name':
                self.jobs['jobs'][0]['name'] = 'qualify'
            elif change == 'missing':
                self.jobs['jobs'].pop()
            elif change == 'extra':
                self.jobs['jobs'].append(copy.deepcopy(self.jobs['jobs'][0]))
            else:
                self.jobs['total_count'] = 3 if change == 'total' else True
            with self.subTest(change=change):
                self.reject()

    def test_required_steps_must_be_present_successful_ordered_and_adjacent(self):
        for change in ['missing', 'duplicate-name', 'order', 'skipped', 'failure', 'incomplete',
                       'duplicate-number', 'bool-number', 'gap', 'too-many', 'long-name']:
            self.jobs = copy.deepcopy(self.original_jobs)
            steps = self.jobs['jobs'][1]['steps']
            qualify = next(step for step in steps if step['name'] == 'Qualify selected final DMG')
            attest = next(step for step in steps if step['name'] == 'Attest generated native qualification')
            if change == 'missing':
                steps.remove(qualify)
            elif change == 'duplicate-name':
                steps[-1]['name'] = 'Qualify selected final DMG'
            elif change == 'order':
                qualify['name'], attest['name'] = attest['name'], qualify['name']
            elif change in ['skipped', 'failure']:
                qualify['conclusion'] = change
            elif change == 'incomplete':
                attest['status'] = 'in_progress'
            elif change == 'duplicate-number':
                attest['number'] = qualify['number']
            elif change == 'bool-number':
                qualify['number'] = True
            elif change == 'gap':
                for step in steps[steps.index(attest):]:
                    step['number'] += 1
            elif change == 'too-many':
                steps.extend([dict(number=index + 20, name='extra-' + str(index)) for index in range(65)])
            else:
                steps[-1]['name'] = 'x' * 257
            with self.subTest(change=change):
                self.reject()

    def test_invocation_guard_must_precede_checkout_of_the_authenticated_source(self):
        for change in ['missing-guard', 'checkout-first', 'other-source']:
            self.run = copy.deepcopy(self.original_run)
            self.jobs = copy.deepcopy(self.original_jobs)
            steps = self.jobs['jobs'][1]['steps']
            guard = next(step for step in steps if step['name'] == 'Require reviewed main invocation')
            checkout = next(step for step in steps if step['name'] == 'Checkout reviewed main source')
            if change == 'missing-guard':
                steps.remove(guard)
            elif change == 'checkout-first':
                guard['name'], checkout['name'] = checkout['name'], guard['name']
            else:
                self.run['head_sha'] = 'b' * 40
                self.run['head_commit']['id'] = 'b' * 40
                for job in self.jobs['jobs']:
                    job['head_sha'] = 'b' * 40
            with self.subTest(change=change):
                self.reject()

    def test_response_json_and_byte_limits_fail_closed_without_raw_metadata(self):
        duplicate = json.dumps(self.run).replace('"id": 12345', '"id": 1, "id": 12345', 1).encode()
        for raw in [b'', b'[]', b'{', b'\xff', duplicate, b'x' * (native.API_LIMIT + 1), 'not-bytes']:
            self.raw = {self.base: raw}
            with self.subTest(kind=type(raw).__name__):
                self.reject()
        self.raw = {self.base + '/jobs': json.dumps(dict(total_count=1000, jobs=[])).encode()}
        self.reject()

    def test_transport_errors_are_private_and_interruption_never_returns_success(self):
        for error in [ValueError('PRIVATE_SENTINEL'), OSError('PRIVATE_SENTINEL'),
                      TimeoutError('PRIVATE_SENTINEL'), RuntimeError('PRIVATE_SENTINEL')]:
            self.transport_error = error
            self.reject()
        self.transport_error = KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):
            self.authenticate()
        self.assertTrue((self.trust.base / ('service-' + str(self.serial))).is_dir(),
                        'Preserve owned work on interruption')

    def test_cli_opt_in_requires_complete_crypto_selection_before_readback(self):
        arguments = ['production_readiness.py', '--directory', str(self.trust.inputs),
                     '--commit', 'a' * 40, '--version', '0.2.0',
                     '--output', str(self.trust.base / 'never-created.json'), '--native-job-readback']
        with mock.patch.object(sys, 'argv', arguments), mock.patch.object(website, 'public_get') as get, \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            readiness.main()
        self.assertEqual(error.exception.code, 2)
        get.assert_not_called()
        self.assertFalse((self.trust.base / 'never-created.json').exists())

    def test_authenticated_handoff_can_reconcile_only_its_job_and_stays_unready(self):
        fixture = test_dmg_handoff.Handoff(methodName='runTest')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.run['head_sha'] = fixture.commit
        self.run['head_commit']['id'] = fixture.commit
        for job in self.jobs['jobs']:
            job['head_sha'] = fixture.commit
        values = dict(self.trust.values, workflow_commit=fixture.commit, readback_job=True,
                      work=self.trust.base / 'aggregate-work')
        original = self.trust.command
        def command(executable, arguments, work, label):
            if label == 'version':
                return original(executable, arguments, work, label)
            return json.dumps(fixtures.verified(fixture.commit, fixtures.sha(fixture.evidence.read_bytes()))).encode()
        with mock.patch.object(native.sys, 'platform', 'linux'), \
                mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(native, '_command', side_effect=command), \
                mock.patch.object(website, 'public_get', side_effect=self.get):
            result = fixture.handoff(dmg_trust=values)
        self.assertEqual(len(self.calls), 2)
        self.assertFalse(result['candidate_verified'])
        self.assertFalse(result['production_ready'])
        macos = result['installed_candidates']['macOS']
        self.assertEqual(macos['native_job_conclusion'], 'service-reconciled-success')
        self.assertFalse(macos['ready'])
        self.assertEqual(macos['installed_acceptance'], 'not-established-by-aggregate')
        self.assertEqual(macos['public_availability'], 'unverified')
        self.assertNotIn('macos-native-job-provenance-unverified', result['blockers'])
        self.assertNotIn('macos-native-job-conclusion-not-service-reconciled', result['blockers'])
        self.assertIn('macos-dmg-native-signing-and-installation-not-verified', result['blockers'])


if __name__ == '__main__':
    unittest.main()
