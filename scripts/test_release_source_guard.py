"""Exercise the actual fixed release workflow guard offline, without checkout or signing."""
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


if __name__ == '__main__':
    unittest.main()
