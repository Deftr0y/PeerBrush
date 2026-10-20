"""Offline policy tests; fake verifier processes never establish real signature evidence."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import verify_macos_qualification as native
import test_dmg_handoff
from test_production_readiness import assert_publication_phases


def sha(data):
    return hashlib.sha256(data).hexdigest()


def verified(commit, qualification_sha256, run_id='12345', run_attempt='1'):
    cert = dict(certificateIssuer='CN=Synthetic Sigstore Fixture,O=sigstore.dev',
                subjectAlternativeName=native.IDENTITY, issuer=native.ISSUER,
                buildSignerURI=native.IDENTITY, buildSignerDigest=commit,
                runnerEnvironment='github-hosted', sourceRepositoryURI=native.REPOSITORY_URI,
                sourceRepositoryDigest=commit, sourceRepositoryRef='refs/heads/main',
                sourceRepositoryIdentifier=native.REPOSITORY_ID,
                sourceRepositoryOwnerURI='https://github.com/Deftr0y',
                sourceRepositoryOwnerIdentifier=native.OWNER_ID,
                buildConfigURI=native.IDENTITY, buildConfigDigest=commit,
                buildTrigger='workflow_dispatch',
                runInvocationURI=native.REPOSITORY_URI + '/actions/runs/' + run_id + '/attempts/' + run_attempt,
                sourceRepositoryVisibilityAtSigning='public')
    return [{'attestation': {'bundle': {'unverified_input_must_never_be_authority': True}},
             'verificationResult': dict(mediaType=native.RESULT_MEDIA_TYPE,
                signature={'certificate': cert},
                verifiedTimestamps=[dict(type='Tlog', uri='https://synthetic.invalid/log',
                                         timestamp='2026-10-20T00:00:00Z')],
                statement={'_type': 'https://in-toto.io/Statement/v1',
                           'predicateType': native.PREDICATE,
                           'subject': [{'name': 'dmg-qualification.json', 'digest': {'sha256': qualification_sha256}}],
                           'predicate': {'runner': 'self-hosted', 'production_ready': True}})}]


class Authentication(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='peerbrush-native-origin-')
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.root, self.inputs = self.base / 'source', self.base / 'inputs'
        self.root.mkdir()
        self.inputs.mkdir()
        self.qualification = json.dumps(dict(format='PeerBrushDmgQualificationV1', version='0.2.0',
                                             commit='a' * 40)).encode()
        self.bundle, self.trusted_root, self.cli = [self.inputs / name for name in ['bundle.jsonl', 'root.jsonl', 'gh']]
        # Deliberately not valid bundles, roots or executable programs. Only the
        # process seam is fake; no private test trust is available in production.
        for path, data in [(self.bundle, b'UNSIGNED_BUNDLE_CLAIMS'),
                           (self.trusted_root, b'PUBLIC_TEST_ROOT_NOT_TRUSTED\n'),
                           (self.cli, b'FAKE_CLI_NEVER_EXECUTED')]:
            path.write_bytes(data)
        self.values = dict(bundle=self.bundle, bundle_sha256=sha(self.bundle.read_bytes()),
                           trusted_root=self.trusted_root, trusted_root_sha256=sha(self.trusted_root.read_bytes()),
                           cli=self.cli, cli_sha256=sha(self.cli.read_bytes()), cli_version=native.CLI_VERSION,
                           workflow_commit='a' * 40, run_id='12345', run_attempt='1', work=self.base / 'work')
        self.commands = []

    def command(self, executable, arguments, work, label):
        self.commands.append((executable, arguments, work, label))
        if label == 'version':
            return b'gh version 2.102.0 (2026-09-30)\nhttps://github.com/cli/cli/releases/tag/v2.102.0\n'
        return json.dumps(verified('a' * 40, sha(self.qualification))).encode()

    def authenticate(self, **changes):
        options = dict(self.values, **changes)
        with mock.patch.object(native.sys, 'platform', 'linux'), \
                mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(native, '_command', side_effect=self.command):
            return native.authenticate(self.qualification, qualification_sha256=sha(self.qualification),
                commit='a' * 40, version='0.2.0', source_root=self.root, **options)

    def test_pinned_crypto_process_is_called_with_exact_policy_and_result_stays_unready(self):
        result = self.authenticate()
        self.assertEqual(len(self.commands), 2)
        command = self.commands[-1][1]
        for flag, value in [('--repo', native.REPOSITORY), ('--cert-identity', native.IDENTITY),
                             ('--cert-oidc-issuer', native.ISSUER), ('--source-ref', 'refs/heads/main'),
                             ('--source-digest', 'a' * 40), ('--signer-digest', 'a' * 40),
                             ('--predicate-type', native.PREDICATE), ('--format', 'json')]:
            self.assertEqual(command[command.index(flag) + 1], value)
        self.assertIn('--deny-self-hosted-runners', command)
        self.assertIn('--bundle', command)
        self.assertIn('--custom-trusted-root', command)
        self.assertNotIn('--cert-identity-regex', command)
        self.assertEqual(result['native_producer_authentication'], 'verified-pinned-workflow-oidc-attestation')
        for key in ['candidate_verified', 'ready', 'production_ready']:
            self.assertIs(result[key], False)
        self.assertEqual(result['native_job_conclusion'], 'not-service-reconciled')
        self.assertEqual(result['origin']['runner'], 'github-hosted', 'Predicate runner must not supply identity')
        self.assertNotIn('production_ready', result['origin'])
        self.assertNotIn(str(self.base), json.dumps(result))
        self.assertTrue(self.values['work'].is_dir(), 'Private immutable verification work is retained')

    def test_reviewed_pins_version_workflow_and_run_selection_are_required(self):
        for changes in [dict(cli_version='2.101.0'), dict(workflow_commit='b' * 40),
                        dict(run_id='0'), dict(run_id=True), dict(run_attempt='01'),
                        dict(bundle_sha256='0' * 64), dict(trusted_root_sha256='0' * 64),
                        dict(cli_sha256='0' * 64)]:
            work = self.base / ('rejected-' + str(len(list(self.base.iterdir()))))
            with self.subTest(changes=list(changes)), self.assertRaises(ValueError):
                self.authenticate(**dict(changes, work=work))
        self.assertEqual(self.commands, [])
        with self.assertRaises(ValueError):
            self.authenticate(bundle=self.inputs / 'missing')
        self.assertEqual(self.commands, [])

    def test_certificate_origin_rejects_forks_prs_other_workflows_and_missing_fields(self):
        original = verified('a' * 40, sha(self.qualification))
        certificate = original[0]['verificationResult']['signature']['certificate']
        variants = [('issuer', 'https://evil.invalid'), ('subjectAlternativeName', native.IDENTITY + '-extra'),
                    ('sourceRepositoryURI', 'https://github.com/fork/PeerBrush'),
                    ('sourceRepositoryIdentifier', '1'), ('sourceRepositoryOwnerIdentifier', '2'),
                    ('sourceRepositoryRef', 'refs/heads/Main'), ('sourceRepositoryRef', 'refs/pull/1/merge'),
                    ('sourceRepositoryDigest', 'b' * 40), ('buildSignerDigest', 'b' * 40),
                    ('buildConfigDigest', 'b' * 40), ('buildConfigURI', native.IDENTITY + '-other'),
                    ('runnerEnvironment', 'self-hosted'), ('buildTrigger', 'pull_request'),
                    ('buildTrigger', 'pull_request_target'), ('runInvocationURI', native.REPOSITORY_URI + '/actions/runs/99/attempts/1'),
                    ('sourceRepositoryVisibilityAtSigning', 'private')]
        for field, value in variants + [(field, None) for field in certificate]:
            changed = copy.deepcopy(original)
            target = changed[0]['verificationResult']['signature']['certificate']
            if value is None:
                del target[field]
            else:
                target[field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                native._verified_claims(json.dumps(changed).encode(), commit='a' * 40,
                    qualification_sha256=sha(self.qualification), run_id='12345', run_attempt='1')

    def test_verified_output_requires_one_subject_observer_timestamp_and_strict_types(self):
        original = verified('a' * 40, sha(self.qualification))
        variants = [[], original * 2, {}, original[0]]
        for field, value in [('mediaType', 'unknown'), ('verifiedTimestamps', []),
                             ('verifiedTimestamps', [dict(type='CurrentTime', uri='', timestamp='2026-10-20T00:00:00Z')]),
                             ('verifiedTimestamps', [dict(type='Tlog', uri='log', timestamp='2026-10-20T00:00:00')]),
                             ('signature', {'publicKeyId': 'unsigned'}), ('statement', {'predicateType': native.PREDICATE})]:
            changed = copy.deepcopy(original)
            changed[0]['verificationResult'][field] = value
            variants.append(changed)
        changed = copy.deepcopy(original)
        changed[0]['verificationResult']['statement']['subject'][0]['digest']['sha256'] = 'b' * 64
        variants.append(changed)
        for value in variants:
            with self.subTest(value=str(value)[:45]), self.assertRaises(ValueError):
                native._verified_claims(json.dumps(value).encode(), commit='a' * 40,
                    qualification_sha256=sha(self.qualification), run_id='12345', run_attempt='1')
        raw = json.dumps(original).replace('"issuer":', '"issuer":"duplicate","issuer":').encode()
        for data in [raw, b'\xff', b'{', b'x' * (native.OUTPUT_LIMIT + 1)]:
            with self.assertRaises(ValueError):
                native._verified_claims(data, commit='a' * 40, qualification_sha256=sha(self.qualification),
                                         run_id='12345', run_attempt='1')

    def test_forged_claims_cannot_bypass_process_failure_or_wrong_cli_version(self):
        for failure in ['crypto', 'version']:
            work = self.base / failure
            def command(executable, arguments, selected, label):
                if label == 'version':
                    return self.command(executable, arguments, selected, label) if failure == 'crypto' else b'gh version 2.101.0\n'
                raise ValueError('Cryptographic rejection')
            with mock.patch.object(native.sys, 'platform', 'linux'), \
                    mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                    mock.patch.object(native, '_command', side_effect=command), self.assertRaises(ValueError):
                native.authenticate(self.qualification, qualification_sha256=sha(self.qualification),
                    commit='a' * 40, version='0.2.0', source_root=self.root, **dict(self.values, work=work))

    def test_changed_original_or_owned_inputs_reject_and_work_cannot_be_reused(self):
        real = self.command
        def command(executable, arguments, work, label):
            result = real(executable, arguments, work, label)
            if label == 'verification':
                self.bundle.write_bytes(b'CHANGED')
            return result
        self.command = command
        with self.assertRaisesRegex(ValueError, 'changed during verification'):
            self.authenticate()
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.authenticate()
        for target in [self.root / 'inside', self.inputs / 'inside']:
            with self.assertRaises(ValueError):
                self.authenticate(work=target)

    def test_command_uses_isolated_environment_and_rejects_failures_output_and_timeout(self):
        captured, terminated = [], []
        class Child:
            def __init__(self, command, **options):
                captured.append((command, options))
                self.return_code = 0
                options['stdout'].write(b'fixture-output')
                options['stdout'].flush()
            def __enter__(self): return self
            def __exit__(self, *unused): return False
            def poll(self): return self.return_code
            def kill(self):
                terminated.append('kill')
                self.return_code = -9
            def wait(self, timeout=None):
                terminated.append(timeout)
                return self.return_code
        for name in ['normal', 'failure', 'limit', 'timeout']:
            work = self.base / name
            work.mkdir()
            class Selected(Child):
                def __init__(self, *arguments, **options):
                    super().__init__(*arguments, **options)
                    if name == 'failure': self.return_code = 1
            with mock.patch.object(native.subprocess, 'Popen', Selected), \
                    mock.patch.dict(os.environ, {'GH_TOKEN': 'PRIVATE_SENTINEL', 'HTTPS_PROXY': 'PRIVATE_SENTINEL'}):
                if name == 'normal':
                    self.assertEqual(native._command(self.cli, ['--version'], work, 'fixture'), b'fixture-output')
                elif name == 'timeout':
                    with mock.patch.object(native.time, 'monotonic', side_effect=[0, 61]), self.assertRaises(ValueError):
                        native._command(self.cli, ['--version'], work, 'fixture')
                elif name == 'limit':
                    with mock.patch.object(native, 'OUTPUT_LIMIT', 2), self.assertRaises(ValueError):
                        native._command(self.cli, ['--version'], work, 'fixture')
                else:
                    with self.assertRaises(ValueError):
                        native._command(self.cli, ['--version'], work, 'fixture')
        class Running(Child):
            def __init__(self, *arguments, **options):
                super().__init__(*arguments, **options)
                self.return_code = None
        interrupted = self.base / 'interrupted'
        interrupted.mkdir()
        with mock.patch.object(native.subprocess, 'Popen', Running), \
                mock.patch.object(native.time, 'sleep', side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            native._command(self.cli, ['--version'], interrupted, 'fixture')
        self.assertEqual(terminated, ['kill', 10])
        for command, options in captured:
            self.assertEqual(options['stdin'], subprocess.DEVNULL)
            self.assertNotIn('GH_TOKEN', options['env'])
            self.assertNotIn('HTTPS_PROXY', options['env'])
            self.assertEqual(options['env']['GH_PROMPT_DISABLED'], '1')


class Aggregate(unittest.TestCase):
    def test_only_actual_verifier_call_can_add_origin_and_never_production_authority(self):
        fixture = test_dmg_handoff.Handoff(methodName='runTest')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        trust = Authentication(methodName='runTest')
        trust.setUp()
        self.addCleanup(trust.doCleanups)
        values = dict(trust.values, workflow_commit=fixture.commit)
        def command(executable, arguments, work, label):
            if label == 'version':
                return trust.command(executable, arguments, work, label)
            return json.dumps(verified(fixture.commit, sha(fixture.evidence.read_bytes()))).encode()
        with mock.patch.object(native.sys, 'platform', 'linux'), \
                mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(native, '_command', side_effect=ValueError('CRYPTO_FAILED')):
            with self.assertRaises(ValueError):
                fixture.handoff(dmg_trust=values)
        self.assertFalse((fixture.output / 'production-readiness.json').exists())
        values['work'] = trust.base / 'verified-second'
        with mock.patch.object(native.sys, 'platform', 'linux'), \
                mock.patch.object(native.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(native, '_command', side_effect=command):
            result = fixture.handoff(output=fixture.base / 'authenticated', dmg_trust=values)
        self.assertFalse(result['candidate_verified'])
        self.assertFalse(result['production_ready'])
        assert_publication_phases(self, result)
        macos = result['installed_candidates']['macOS']
        self.assertEqual(macos['native_producer_authentication'], 'verified-pinned-workflow-oidc-attestation')
        self.assertEqual(macos['native_origin']['qualification_sha256'], sha(fixture.evidence.read_bytes()))
        self.assertIn('macos-native-job-conclusion-not-service-reconciled', result['blockers'])
        self.assertIn('macos-native-job-conclusion-not-service-reconciled',
                      result['publication_phases']['prepublication_blockers'])
        self.assertNotIn('macos-native-job-provenance-unverified', result['blockers'])
        self.assertEqual(macos['installed_acceptance'], 'not-established-by-aggregate')


if __name__ == '__main__':
    unittest.main()
