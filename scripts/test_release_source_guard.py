"""Exercise actual release/website guards offline, without checkout, signing or deployment."""
import os
from pathlib import Path
import textwrap
import unittest
from unittest import mock


COMMIT = '1' * 40
REPOSITORY = 'Deftr0y/PeerBrush'
WORKFLOW = REPOSITORY + '/.github/workflows/release.yml'


class ReleaseInvocation(unittest.TestCase):
    def setUp(self):
        self.workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text(encoding='utf-8')
        native, rest = self.workflow.split('\n  native:\n', 1)[1].split('\n  inspect:\n', 1)
        inspect, sign = rest.split('\n  sign:\n', 1)
        self.sections = dict(native=native, inspect=inspect, sign=sign)
        self.guards, self.programs = {}, {}
        for job, section in self.sections.items():
            steps = section.split('    steps:\n', 1)[1]
            guard = steps.split('\n      - ', 1)[0]
            self.guards[job] = guard
            inline = guard.split("python -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
            self.programs[job] = compile(textwrap.dedent(inline), '<fixed-release-invocation>', 'exec')

    def environment(self, event='push', ref='refs/heads/main'):
        return dict(EXPECTED_COMMIT=COMMIT, GITHUB_SHA=COMMIT, GITHUB_WORKFLOW_SHA=COMMIT,
                    GITHUB_REPOSITORY=REPOSITORY, GITHUB_EVENT_NAME=event,
                    GITHUB_REF=ref, GITHUB_WORKFLOW_REF=WORKFLOW + '@' + ref)

    def test_fixed_isolated_guard_precedes_checkout_download_and_repository_code(self):
        for job, section in self.sections.items():
            with self.subTest(job=job):
                guard = self.guards[job]
                self.assertTrue(guard.lstrip().startswith('- name: Require exact release invocation\n'))
                self.assertNotIn('scripts/', guard)
                self.assertNotIn('uses:', guard)
                guard_position = section.index('name: Require exact release invocation')
                if job in ['native', 'inspect']:
                    checkout = section.index('uses: actions/checkout@')
                    source_gate = section.index('scripts/release_gate.py')
                    self.assertLess(guard_position, checkout)
                    self.assertLess(checkout, source_gate)
                    checkout_step = section[checkout:].split('\n      - ', 1)[0]
                    self.assertIn('ref: ' + '$' + '{{ github.sha }}', checkout_step)
                    self.assertNotIn('ref: ' + '$' + '{{ inputs.expected_commit', section)
                else:
                    self.assertLess(guard_position, section.index('uses: actions/download-artifact@'))
                    self.assertLess(guard_position, section.index('name: Sign and verify packages'))

    def test_actual_inline_guard_preserves_main_push_and_same_ref_manual_candidates(self):
        invocations = [('push', 'refs/heads/main'),
                       ('workflow_dispatch', 'refs/heads/main'),
                       ('workflow_dispatch', 'refs/heads/version/0.3'),
                       ('workflow_dispatch', 'refs/heads/feature/0.3/candidate'),
                       ('workflow_dispatch', 'refs/heads/topic@candidate'),
                       ('workflow_dispatch', 'refs/tags/v0.30.0')]
        for job, program in self.programs.items():
            for event, ref in invocations:
                with self.subTest(job=job, event=event, ref=ref), \
                        mock.patch.dict(os.environ, self.environment(event, ref), clear=True):
                    exec(program, {})

    def test_actual_inline_guard_rejects_foreign_source_workflow_repository_and_event(self):
        variants = [('EXPECTED_COMMIT', '0' * 40), ('EXPECTED_COMMIT', 'refs/pull/17/merge'),
                    ('EXPECTED_COMMIT', '1' * 39), ('EXPECTED_COMMIT', 'A' * 40), ('EXPECTED_COMMIT', ''),
                    ('GITHUB_SHA', '2' * 40), ('GITHUB_WORKFLOW_SHA', '2' * 40),
                    ('GITHUB_REPOSITORY', 'fork/PeerBrush'),
                    ('GITHUB_WORKFLOW_REF', REPOSITORY + '/.github/workflows/other.yml@refs/heads/main'),
                    ('GITHUB_WORKFLOW_REF', WORKFLOW + '@refs/heads/feature/0.3/other'),
                    ('GITHUB_EVENT_NAME', 'pull_request'), ('GITHUB_EVENT_NAME', 'pull_request_target')]
        for job, program in self.programs.items():
            for key, value in variants:
                with self.subTest(job=job, context=key, value=value), \
                        mock.patch.dict(os.environ, dict(self.environment(), **{key: value}), clear=True), \
                        self.assertRaises(SystemExit):
                    exec(program, {})
            for event, ref in [('push', 'refs/heads/feature/0.3/other'), ('push', 'refs/tags/v0.30.0'),
                               ('workflow_dispatch', 'refs/pull/17/merge'), ('workflow_dispatch', ''),
                               ('workflow_dispatch', 'refs/heads/'), ('workflow_dispatch', 'refs/tags/'),
                               ('pull_request', 'refs/heads/main')]:
                with self.subTest(job=job, event=event, ref=ref), \
                        mock.patch.dict(os.environ, self.environment(event, ref), clear=True), \
                        self.assertRaises(SystemExit):
                    exec(program, {})
            with self.subTest(job=job, context='missing'), mock.patch.dict(os.environ, {}, clear=True), \
                    self.assertRaises(SystemExit):
                exec(program, {})



class WebsiteInvocation(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parents[1]
        self.workflow = (root / '.github/workflows/pages.yml').read_text(encoding='utf-8')
        build, deploy = self.workflow.split('\n  build:\n', 1)[1].split('\n  deploy:\n', 1)
        self.sections = dict(build=build, deploy=deploy)
        self.guards, self.programs = {}, {}
        for job, section in self.sections.items():
            steps = section.split('    steps:\n', 1)[1]
            guard = steps.split('\n      - ', 1)[0]
            self.guards[job] = guard
            inline = guard.split("python -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
            self.programs[job] = compile(textwrap.dedent(inline), '<fixed-website-invocation>', 'exec')
        check = build.split('      - name: Check reviewed public output\n', 1)[1].split('\n      - ', 1)[0]
        inline = check.split("python -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
        self.check = compile(textwrap.dedent(inline), '<fixed-website-selection>', 'exec')

    def environment(self):
        return dict(EXPECTED_SITE_COMMIT='2' * 40, GITHUB_SHA=COMMIT,
                    GITHUB_WORKFLOW_SHA=COMMIT, GITHUB_REPOSITORY=REPOSITORY,
                    GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_REF='refs/heads/main',
                    GITHUB_WORKFLOW_REF=REPOSITORY + '/.github/workflows/pages.yml@refs/heads/main')

    def test_actual_guard_accepts_distinct_exact_source_and_site_but_rejects_foreign_invocations(self):
        variants = [('EXPECTED_SITE_COMMIT', ''), ('EXPECTED_SITE_COMMIT', 'site-content'),
                    ('EXPECTED_SITE_COMMIT', 'A' * 40), ('EXPECTED_SITE_COMMIT', '2' * 39),
                    ('GITHUB_SHA', ''), ('GITHUB_SHA', 'A' * 40),
                    ('GITHUB_WORKFLOW_SHA', '3' * 40), ('GITHUB_REPOSITORY', 'fork/PeerBrush'),
                    ('GITHUB_EVENT_NAME', 'push'), ('GITHUB_EVENT_NAME', 'pull_request_target'),
                    ('GITHUB_REF', 'refs/heads/site-content'), ('GITHUB_REF', 'refs/tags/v0.30.0'),
                    ('GITHUB_WORKFLOW_REF', REPOSITORY + '/.github/workflows/other.yml@refs/heads/main'),
                    ('GITHUB_WORKFLOW_REF', REPOSITORY + '/.github/workflows/pages.yml@refs/heads/topic')]
        for job, program in self.programs.items():
            with self.subTest(job=job, valid=True), mock.patch.dict(os.environ, self.environment(), clear=True):
                exec(program, {})
            for key, value in variants:
                with self.subTest(job=job, key=key, value=value), \
                        mock.patch.dict(os.environ, dict(self.environment(), **{key: value}), clear=True), \
                        self.assertRaises(SystemExit) as failure:
                    exec(program, {})
                self.assertEqual(str(failure.exception),
                                 'Website deployment requires its exact official main workflow and reviewed site commit.')
            with self.subTest(job=job, missing=True), mock.patch.dict(os.environ, {}, clear=True), \
                    self.assertRaises(SystemExit):
                exec(program, {})

    def test_guards_precede_checkout_and_privileged_actions_without_changing_manual_protections(self):
        for job, section in self.sections.items():
            guard = self.guards[job]
            self.assertTrue(guard.lstrip().startswith('- name: Require exact website invocation\n'))
            self.assertNotIn('uses:', guard)
            self.assertNotIn('scripts/', guard)
            self.assertLess(section.index('name: Require exact website invocation'), section.index('uses: actions/'))
        build = self.sections['build']
        checkout = build.split('uses: actions/checkout@', 1)[1].split('\n      - ', 1)[0]
        self.assertIn('ref: ' + '$' + '{{ inputs.expected_site_commit }}', checkout)
        self.assertNotIn('ref: site-content', checkout)
        self.assertIn('persist-credentials: false', checkout)
        self.assertLess(build.index('name: Check reviewed public output'), build.index('uses: actions/configure-pages@'))
        self.assertIn('path: public-site', build)
        self.assertIn('on:\n  workflow_dispatch:', self.workflow)
        self.assertNotIn('  push:', self.workflow)
        self.assertIn('permissions:\n  contents: read\n', self.workflow)
        self.assertIn('env:\n  EXPECTED_SITE_COMMIT: ' + '$' + '{{ inputs.expected_site_commit }}', self.workflow)
        self.assertIn('concurrency:\n  group: github-pages\n  cancel-in-progress: false', self.workflow)
        self.assertIn('    needs: build\n', self.sections['deploy'])
        self.assertIn('    permissions:\n      pages: write\n      id-token: write', self.sections['deploy'])
        self.assertIn('    environment:\n      name: github-pages\n', self.sections['deploy'])
        import json
        inventory = json.loads((Path(__file__).resolve().parents[1] / 'scripts/package-files.json').read_text())
        for path in ['.github/workflows/pages.yml', 'scripts/test_release_source_guard.py']:
            self.assertIn(path, inventory['source'])
            self.assertNotIn(path, inventory['portable'])

    def test_actual_selection_pins_checkout_and_fetched_current_output_tip(self):
        expected = self.environment()['EXPECTED_SITE_COMMIT']
        with mock.patch.dict(os.environ, self.environment(), clear=True), \
                mock.patch('subprocess.check_output', side_effect=[expected + '\n', expected + '\n']) as read, \
                mock.patch('subprocess.run') as fetch, mock.patch.object(Path, 'is_file', return_value=True) as file:
            exec(self.check, {})
        self.assertEqual(read.call_args_list,
                         [mock.call(['git', 'rev-parse', 'HEAD'], text=True),
                          mock.call(['git', 'rev-parse', 'FETCH_HEAD'], text=True)])
        fetch.assert_called_once_with(['git', 'fetch', '--no-tags', '--depth=1', 'origin',
                                       'refs/heads/site-content'], check=True)
        self.assertEqual(file.call_count, 2)

    def test_actual_selection_rejects_wrong_checkout_moved_tip_missing_output_and_failed_fetch(self):
        import subprocess
        expected = self.environment()['EXPECTED_SITE_COMMIT']
        for head, tip, present, fetch_error in [(COMMIT, expected, True, None),
                                                (expected, COMMIT, True, None),
                                                (expected, expected, False, None),
                                                (expected, expected, True, subprocess.CalledProcessError(1, 'git'))]:
            with self.subTest(head=head, tip=tip, present=present, fetch_error=fetch_error is not None), \
                    mock.patch.dict(os.environ, self.environment(), clear=True), \
                    mock.patch('subprocess.check_output', side_effect=[head, tip]), \
                    mock.patch('subprocess.run', side_effect=fetch_error) as fetch, \
                    mock.patch.object(Path, 'is_file', return_value=present), \
                    self.assertRaises((SystemExit, subprocess.CalledProcessError)):
                exec(self.check, {})
            self.assertEqual(fetch.call_count, int(head == expected))



class FinalInputInvocation(unittest.TestCase):
    def test_actual_data_workflow_guards_are_fixed_and_run_before_checkout(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / '.github/workflows/final-inputs.yml').read_text(encoding='utf-8')
        for name in ['gate', 'prepare']:
            body = workflow.split('\n  ' + name + ':\n', 1)[1]
            guard = body.split("python -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
            program = compile(textwrap.dedent(guard), '<fixed-final-input-invocation>', 'exec')
            environment = dict(EXPECTED_COMMIT=COMMIT, GITHUB_SHA=COMMIT, GITHUB_WORKFLOW_SHA=COMMIT,
                GITHUB_REPOSITORY=REPOSITORY, GITHUB_REF='refs/heads/main', GITHUB_EVENT_NAME='workflow_dispatch',
                GITHUB_WORKFLOW_REF=REPOSITORY + '/.github/workflows/final-inputs.yml@refs/heads/main', GITHUB_RUN_ATTEMPT='1')
            with mock.patch.dict(os.environ, environment, clear=True):
                exec(program, {})
            for key, value in [('GITHUB_SHA', '2' * 40), ('GITHUB_WORKFLOW_SHA', '2' * 40),
                               ('GITHUB_REPOSITORY', 'fork/PeerBrush'), ('GITHUB_EVENT_NAME', 'pull_request'),
                               ('GITHUB_REF', 'refs/pull/1/merge'), ('GITHUB_RUN_ATTEMPT', '2'),
                               ('GITHUB_WORKFLOW_REF', REPOSITORY + '/.github/workflows/release.yml@refs/heads/main')]:
                with self.subTest(job=name, field=key), mock.patch.dict(os.environ, dict(environment, **{key: value}), clear=True), self.assertRaises(SystemExit):
                    exec(program, {})
            self.assertLess(body.index('Require exact dormant data-producer invocation'), body.index('actions/checkout@'))
            self.assertIn('if: ${{ false }}', body.split('    steps:', 1)[0])

    def test_actual_production_composition_guard_precedes_checkout_and_requires_exact_main(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / '.github/workflows/production.yml').read_text(encoding='utf-8')
        body = workflow.split('\n  compose:\n', 1)[1].split('\n  publish:\n', 1)[0]
        guard = body.split("python -I - <<'PY'\n", 1)[1].split('\n          PY', 1)[0]
        program = compile(textwrap.dedent(guard), '<fixed-final-consumer-invocation>', 'exec')
        environment = dict(EXPECTED_COMMIT=COMMIT, GITHUB_SHA=COMMIT, GITHUB_WORKFLOW_SHA=COMMIT,
            GITHUB_REPOSITORY=REPOSITORY, GITHUB_REF='refs/heads/main', GITHUB_EVENT_NAME='workflow_dispatch',
            GITHUB_WORKFLOW_REF=REPOSITORY + '/.github/workflows/production.yml@refs/heads/main')
        with mock.patch.dict(os.environ, environment, clear=True):
            exec(program, {})
        for key, value in [('GITHUB_SHA', '2' * 40), ('GITHUB_WORKFLOW_SHA', '2' * 40),
                           ('GITHUB_REPOSITORY', 'fork/PeerBrush'), ('GITHUB_EVENT_NAME', 'pull_request'),
                           ('GITHUB_REF', 'refs/pull/1/merge'),
                           ('GITHUB_WORKFLOW_REF', REPOSITORY + '/.github/workflows/release.yml@refs/heads/main')]:
            with self.subTest(field=key), mock.patch.dict(os.environ, dict(environment, **{key: value}), clear=True), self.assertRaises(SystemExit):
                exec(program, {})
        self.assertLess(body.index('Require exact reviewed production invocation'), body.index('actions/checkout@'))
        self.assertIn('if: ${{ false }}', body.split('    steps:', 1)[0])


if __name__ == '__main__':
    unittest.main()
