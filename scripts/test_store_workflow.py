"""Exercise the real dormant Store workflow's pre-checkout trust boundary offline."""
import contextlib
import io
import json
import os
from pathlib import Path
import re
import textwrap
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / '.github/workflows/store-submit.yml'
AUTOMATION = 'a' * 40
RELEASE = 'b' * 40
REF = 'Deftr0y/PeerBrush/.github/workflows/store-submit.yml@refs/heads/main'


def invocation():
    return dict(AUTOMATION_COMMIT=AUTOMATION, RELEASE_COMMIT=RELEASE,
                PORTABLE_SHA256='c' * 64, MSIX_SHA256='d' * 64,
                STORE_EXECUTE='false', STORE_AUTOMATION_ENABLED='true',
                GITHUB_REPOSITORY='Deftr0y/PeerBrush', GITHUB_REF='refs/heads/main',
                GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_SHA=AUTOMATION,
                GITHUB_WORKFLOW_SHA=AUTOMATION, GITHUB_WORKFLOW_REF=REF,
                GITHUB_RUN_ATTEMPT='1', RUNNER_ENVIRONMENT='github-hosted',
                RUNNER_OS='Linux', RUNNER_ARCH='X64')


def run_guard(environment):
    text = WORKFLOW.read_text(encoding='utf-8')
    step = text.split('      - name: Require reviewed manual main Store invocation\n', 1)[1]
    body = step.split("          python3 -I - <<'PY'\n", 1)[1].split('\n          PY\n', 1)[0]
    output = io.StringIO()
    with mock.patch.dict(os.environ, environment, clear=True), contextlib.redirect_stdout(output):
        exec(compile(textwrap.dedent(body), str(WORKFLOW), 'exec'), {})
    return output.getvalue()


class StoreWorkflow(unittest.TestCase):
    def test_actual_guard_keeps_release_source_separate_from_automation(self):
        environment = invocation()
        self.assertNotEqual(environment['AUTOMATION_COMMIT'], environment['RELEASE_COMMIT'])
        self.assertEqual(run_guard(environment), '')
        environment['RELEASE_COMMIT'] = AUTOMATION
        self.assertEqual(run_guard(environment), '')

    def test_actual_guard_rejects_foreign_or_unreviewed_invocations(self):
        invalid = [
            ('GITHUB_REPOSITORY', 'fork/PeerBrush'), ('GITHUB_REF', 'refs/heads/version/0.3'),
            ('GITHUB_EVENT_NAME', 'pull_request'), ('GITHUB_EVENT_NAME', 'release'),
            ('GITHUB_EVENT_NAME', 'workflow_call'), ('GITHUB_SHA', RELEASE),
            ('GITHUB_WORKFLOW_SHA', RELEASE), ('GITHUB_WORKFLOW_REF', REF.replace('store-submit', 'other')),
            ('GITHUB_WORKFLOW_REF', REF.replace('refs/heads/main', 'refs/heads/version/0.3')),
            ('GITHUB_RUN_ATTEMPT', '2'), ('RUNNER_ENVIRONMENT', 'self-hosted'),
            ('RUNNER_OS', 'macOS'), ('RUNNER_ARCH', 'ARM64'),
            ('STORE_AUTOMATION_ENABLED', ''), ('STORE_AUTOMATION_ENABLED', 'TRUE'),
            ('AUTOMATION_COMMIT', 'a' * 39), ('AUTOMATION_COMMIT', 'A' * 40),
            ('AUTOMATION_COMMIT', ''), ('RELEASE_COMMIT', ''), ('RELEASE_COMMIT', 'B' * 40),
            ('PORTABLE_SHA256', 'c' * 63), ('MSIX_SHA256', 'D' * 64),
        ]
        for key, value in invalid:
            with self.subTest(key=key, value=value):
                environment = invocation()
                environment[key] = value
                with self.assertRaisesRegex(SystemExit, '^Store preflight requires independently reviewed exact manual main automation and separate release inputs before checkout\\.$'):
                    run_guard(environment)
        for key in invocation():
            environment = invocation()
            del environment[key]
            with self.subTest(missing=key), self.assertRaises(SystemExit):
                run_guard(environment)

    def test_hosted_write_route_fails_before_checkout_and_does_not_echo_input(self):
        for value in ['true', 'True', '', 'private-input\r\n::error::unexpected']:
            environment = invocation()
            environment['STORE_EXECUTE'] = value
            with self.subTest(value=value), self.assertRaises(SystemExit) as error:
                run_guard(environment)
            self.assertEqual(str(error.exception), 'Hosted Store submission remains blocked until an approved durable private host/state integration is implemented.')
        environment = invocation()
        environment['RELEASE_COMMIT'] = 'private-input\r\n::error::unexpected'
        with self.assertRaises(SystemExit) as error:
            run_guard(environment)
        self.assertNotIn('private-input', str(error.exception))
        self.assertNotIn('::error::', str(error.exception))

    def test_actual_checkout_order_and_pinned_execution_source(self):
        text = WORKFLOW.read_text(encoding='utf-8')
        steps = text.split('    steps:\n', 1)[1]
        self.assertTrue(steps.startswith('      - name: Require reviewed manual main Store invocation\n'))
        names = re.findall(r'^      - name: (.+)$', steps, re.M)
        self.assertEqual(names, ['Require reviewed manual main Store invocation',
                                 'Check out the reviewed automation source',
                                 'Verify actual automation checkout',
                                 'Read the selected release source without executing its scripts',
                                 'Validate the published release (read-only)'])
        chunks = re.split(r'^      - name: ', steps, flags=re.M)[1:]
        trusted, verified, selected, preflight = chunks[1:]
        pin = 'uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4'
        self.assertIn(pin, trusted)
        self.assertIn('ref: ${{ github.sha }}', trusted)
        self.assertIn('path: trusted-automation', trusted)
        self.assertNotIn('inputs.source_commit', trusted)
        self.assertNotIn('ref: main', text)
        self.assertIn('working-directory: trusted-automation', verified)
        self.assertIn('AUTOMATION_COMMIT: ${{ inputs.automation_commit }}', verified)
        self.assertIn('scripts/release_gate.py --commit "$AUTOMATION_COMMIT"', verified)
        self.assertIn(pin, selected)
        self.assertIn('ref: ${{ inputs.source_commit }}', selected)
        self.assertIn('path: selected-release-source', selected)
        self.assertNotIn('run:', selected)
        self.assertEqual(text.count('persist-credentials: false'), 2)
        self.assertIn('working-directory: trusted-automation', preflight)
        self.assertIn('--source-root "$GITHUB_WORKSPACE/selected-release-source"', preflight)
        self.assertIn('python3 scripts/submit_store_release.py', preflight)
        self.assertNotIn('--submit', preflight)
        self.assertNotIn('--state', preflight)
        for name in ['MSSTORE_TENANT_ID', 'MSSTORE_CLIENT_ID', 'MSSTORE_CLIENT_SECRET']:
            self.assertIn(name + ': ${{ secrets.' + name + ' }}', preflight)
        self.assertIn('GH_TOKEN: ${{ github.token }}', preflight)
        for chunk in chunks[:-1]:
            self.assertNotIn('secrets.', chunk)

    def test_inactive_routes_least_permissions_and_all_real_test_callers(self):
        text = WORKFLOW.read_text(encoding='utf-8')
        condition = text.split('  submit:\n', 1)[1].split('    if: ', 1)[1].splitlines()[0]
        for required in ["vars.MSSTORE_AUTOMATION_ENABLED == 'true'",
                         "github.repository == 'Deftr0y/PeerBrush'", "github.ref == 'refs/heads/main'",
                         "github.event_name == 'workflow_dispatch'", "github.workflow_ref == '" + REF + "'"]:
            self.assertIn(required, condition)
        self.assertEqual(text.count('      automation_commit:\n'), 2)
        self.assertEqual(text.count('      submit:\n'), 2)
        self.assertEqual(text.count('        default: false\n'), 2)
        self.assertIn('permissions:\n  contents: read\n', text)
        self.assertNotIn('id-token:', text)
        self.assertNotIn('environment:', text)
        self.assertEqual(text.count('    secrets:\n'), 1)
        self.assertIn('cancel-in-progress: false', text)
        for name in ['ci.yml', 'release.yml']:
            workflow = (ROOT / '.github/workflows' / name).read_text(encoding='utf-8')
            self.assertEqual(workflow.count('python -B scripts/test_store_workflow.py'), 1)
        source = json.loads((ROOT / 'scripts/package-files.json').read_text(encoding='utf-8'))['source']
        self.assertEqual(source.count('scripts/test_store_workflow.py'), 1)


if __name__ == '__main__':
    unittest.main()
