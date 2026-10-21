"""Exercise actual dormant signing workflow code without credentials or Apple tools."""
import hashlib
import json
import os
from pathlib import Path
import tarfile
import tempfile
import textwrap
import unittest
from unittest import mock

from package_macos_dmg import bounded_file

ROOT = Path(__file__).resolve().parents[1]
COMMIT = '1' * 40
VERSION = '0.30.0'
UNSIGNED = f'PeerBrush-{VERSION}-macOS-arm64.zip'
SIGNED = UNSIGNED[:-4] + '-apple-signed-r2.zip'
IMAGE = SIGNED[:-4] + '-dmg-signed-r3.dmg'
CREDENTIAL_SETUP_HASH = '3eddda727aa596cb66a663d72a883e88f96bfc208eb4552e1614aedb26e46ae9'
CREDENTIAL_CLEANUP_HASH = '2cf990c218b7c48ed8ac865db83ca9c9de8cc39f9eb962a2318e9875c3310fd0'


def step(text, name):
    return text.split('      - name: ' + name + '\n', 1)[1].split('\n      - ', 1)[0]


def inline(text, name, invocation="python3 - <<'PY'\n"):
    body = step(text, name).split(invocation, 1)[1].split('\n          PY', 1)[0]
    return compile(textwrap.dedent(body), '<fixed-apple-producer>', 'exec')


def digest(data):
    return hashlib.sha256(data).hexdigest()


class AppleSigningProducer(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / '.github/workflows/macos-release-signing.yml').read_text(encoding='utf-8')
        self.build, self.sign = self.workflow.split('\n  build:\n', 1)[1].split('\n  sign:\n', 1)
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-apple-workflow-')
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name).resolve()
        self.environment = dict(SELECTED_COMMIT=COMMIT, SELECTED_VERSION=VERSION,
            ZIP_REVISION='2', DMG_REVISION='3', APPLE_TEAM='ABCD123456', APPLE_IDENTITY_SHA1='A' * 40,
            PEERBRUSH_APPLE_RELEASE_SIGNING_ENABLED='true', GITHUB_REPOSITORY='Deftr0y/PeerBrush',
            GITHUB_REF='refs/heads/main', GITHUB_EVENT_NAME='workflow_dispatch',
            GITHUB_SHA=COMMIT, GITHUB_WORKFLOW_SHA=COMMIT,
            GITHUB_WORKFLOW_REF='Deftr0y/PeerBrush/.github/workflows/macos-release-signing.yml@refs/heads/main',
            GITHUB_RUN_ATTEMPT='1', RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='macOS', RUNNER_ARCH='ARM64',
            RUNNER_TEMP=str(self.parent), GITHUB_WORKSPACE=str(ROOT),
            GITHUB_OUTPUT=str(self.parent / 'outputs'), GITHUB_RUN_ID='101')

    def execute(self, name, **environment):
        with mock.patch.dict(os.environ, self.environment | environment, clear=True):
            exec(inline(self.workflow, name), {})

    def write(self, name, data, mode=0o644):
        path = self.parent / name
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        path.write_bytes(data)
        path.chmod(mode)
        return path

    def zip_state(self, submission=True):
        original = self.write('macos-unsigned/' + UNSIGNED, b'synthetic unsigned public package')
        self.environment['UNSIGNED_SHA256'] = digest(original.read_bytes())
        payload = self.write('macos-zip-work/payload/PeerBrush/PeerBrush.app/Contents/MacOS/peerbrush', b'synthetic signed executable', 0o755)
        self.write('macos-zip-work/unrelated-private-data', b'NEVER UPLOAD')
        self.write('apple-credentials/signing.keychain-db', b'NEVER UPLOAD')
        submission_path = self.write('macos-zip-work/submission.zip', b'synthetic submitted public app')
        state = dict(configuration=dict(schema=1, input_sha256=self.environment['UNSIGNED_SHA256'],
            commit=COMMIT, version=VERSION, team_id=self.environment['APPLE_TEAM'],
            certificate_sha1=self.environment['APPLE_IDENTITY_SHA1'], revision='apple-signed-r2'),
            submission_sha256=digest(submission_path.read_bytes()),
            bundle_hashes={'PeerBrush/PeerBrush.app/Contents/MacOS/peerbrush': digest(payload.read_bytes())})
        if submission:
            state['submission_id'] = '12345678-1234-1234-1234-123456789abc'
        self.write('macos-zip-work/state.json', json.dumps(state).encode(), 0o600)
        return original, payload, state

    def test_actual_fixed_guards_deny_non_main_or_different_source_before_checkout(self):
        for section in [self.build, self.sign]:
            guard = section.split('    steps:\n', 1)[1].split('\n      - ', 1)[0]
            self.assertTrue(guard.lstrip().startswith('- name: Require reviewed main invocation'))
            self.assertNotIn('scripts/', guard)
            self.assertNotIn('uses:', guard)
            program = inline(section, 'Require reviewed main invocation', "python -I - <<'PY'\n")
            with mock.patch.dict(os.environ, self.environment, clear=True):
                exec(program, {})
            for key, value in [('SELECTED_COMMIT', 'x'), ('GITHUB_SHA', '2' * 40),
                    ('GITHUB_WORKFLOW_SHA', '2' * 40), ('GITHUB_REPOSITORY', 'fork/PeerBrush'),
                    ('GITHUB_REF', 'refs/heads/version/0.3'), ('GITHUB_EVENT_NAME', 'push'),
                    ('GITHUB_EVENT_NAME', 'pull_request_target'), ('GITHUB_RUN_ATTEMPT', '2'),
                    ('RUNNER_ENVIRONMENT', 'self-hosted'), ('RUNNER_OS', 'Linux'), ('RUNNER_ARCH', 'X64'),
                    ('GITHUB_WORKFLOW_REF', 'Deftr0y/PeerBrush/.github/workflows/apple-signing.yml@refs/heads/main'),
                    ('PEERBRUSH_APPLE_RELEASE_SIGNING_ENABLED', ''), ('ZIP_REVISION', '0'),
                    ('ZIP_REVISION', '1;false'), ('DMG_REVISION', '0001'), ('DMG_REVISION', '1' * 7)]:
                with self.subTest(context=key, value=value), \
                        mock.patch.dict(os.environ, self.environment | {key: value}, clear=True), \
                        self.assertRaises(SystemExit):
                    exec(program, {})
            checkout = section.index('name: Checkout reviewed main source')
            self.assertLess(section.index('name: Require reviewed main invocation'), checkout)
            self.assertIn('ref: ' + '$' + '{{ github.sha }}', section[checkout:].split('\n      - ', 1)[0])

    def test_stable_gate_runs_before_build_and_any_credential_import(self):
        program = inline(self.workflow, 'Require stable exact source before tests', "python3 -I - <<'PY'\n")
        for version in [VERSION, '0.30.0-dev.1', '00.30.0', '0.30.0+build']:
            with self.subTest(version=version), mock.patch.object(Path, 'read_text', return_value='[package]\nversion="' + version + '"\n'):
                if version == VERSION:
                    exec(program, {})
                else:
                    with self.assertRaises(SystemExit):
                        exec(program, {})
        self.assertLess(self.build.index('name: Require stable exact source'), self.build.index('name: Test and build'))
        self.assertNotIn('secrets.', self.build)

    def test_native_activation_and_quit_must_pass_in_guarded_unprivileged_build_before_signing(self):
        build = step(self.build, 'Test and build without Apple credentials')
        activation = 'cargo run --locked --example macos_activation'
        quit = 'cargo run --locked --example macos_quit'
        for command in [activation, quit]:
            self.assertEqual(self.workflow.count(command), 1)
            self.assertIn(command, build)
            self.assertNotIn(command, self.sign)
        self.assertLess(build.index('cargo test --locked\n'), build.index(activation))
        self.assertLess(build.index('cargo test --locked -p egui-winit --lib peerbrush_clipboard_tests'), build.index(activation))
        self.assertLess(build.index(activation), build.index(quit))
        self.assertLess(build.index(quit), build.index('python3 -B scripts/build_release.py'))
        for forbidden in ['continue-on-error:', 'if: always()', 'secrets.', '|| true']:
            self.assertNotIn(forbidden, build)
        self.assertLess(self.build.index('name: Require reviewed main invocation'), self.build.index('name: Checkout reviewed main source'))
        self.assertLess(self.build.index('name: Require stable exact source before tests'), self.build.index('name: Test and build without Apple credentials'))
        self.assertLess(self.build.index('name: Test and build without Apple credentials'), self.build.index('name: Retain exact unsigned input'))
        self.assertIn('needs: build\n', self.sign)
        self.assertIn("needs.build.result == 'success'", self.sign)
        self.assertIn('environment: apple-signing', self.sign)

    def test_existing_native_signers_and_protected_approval_are_the_only_signing_path(self):
        self.assertIn('permissions:\n  contents: read\n', self.workflow)
        self.assertNotIn('id-token:', self.workflow)
        self.assertNotIn('contents: write', self.workflow)
        self.assertNotIn('workflow_call:', self.workflow)
        self.assertIn('environment: apple-signing', self.sign)
        self.assertEqual(self.workflow.count("if: vars.PEERBRUSH_APPLE_RELEASE_SIGNING_ENABLED == 'true'"), 2)
        self.assertIn('group: peerbrush-apple-signing\n  cancel-in-progress: false', self.workflow)
        signing = step(self.workflow, 'Sign ZIP then final DMG with separate submission journals')
        self.assertEqual(signing.count('scripts/sign_macos.py'), 2)
        self.assertLess(signing.index('--format zip'), signing.index('--format dmg'))
        for text in ['--work "$RUNNER_TEMP/macos-zip-work"', '--work "$RUNNER_TEMP/macos-dmg-work"',
                     '--expected-sha256 "$UNSIGNED_SHA256"', '--expected-sha256 "$SIGNED_SHA"']:
            self.assertIn(text, signing)
        self.assertEqual(signing.count('--keychain "$RUNNER_TEMP/apple-credentials/signing.keychain-db"'), 2)
        for forbidden in ['--resume', '||', 'retry', 'gh release', 'scripts/submit_store_release.py']:
            self.assertNotIn(forbidden, signing)
        for first, second in [('Verify exact source and unsigned input before credentials', 'Import credentials only after environment approval'),
                              ('Sign ZIP then final DMG with separate submission journals', 'Delete temporary signing credentials before artifact upload'),
                              ('Delete temporary signing credentials before artifact upload', 'Prepare exact flat signed pair'),
                              ('Prepare exact flat signed pair', 'Retain only the two final signed files')]:
            self.assertLess(self.sign.index('name: ' + first), self.sign.index('name: ' + second))
        for name, expected in [('Import credentials only after environment approval', CREDENTIAL_SETUP_HASH),
                               ('Delete temporary signing credentials before artifact upload', CREDENTIAL_CLEANUP_HASH)]:
            # The historical workflow need not ship inside a source archive.
            body = step(self.workflow, name).split('        run: |\n', 1)[1].rstrip()
            self.assertEqual(digest(body.encode('utf-8')), expected)

    def test_actual_pair_copy_is_exact_hash_bound_and_never_replaces_outputs(self):
        original = {SIGNED: b'synthetic signed ZIP', IMAGE: b'synthetic signed image'}
        for name, body in original.items():
            self.write('macos-signed/' + name, body)
        self.write('macos-signed/' + IMAGE + '.verification.json', b'NOT AN AUTHORITY OR PAIR INPUT')
        self.execute('Prepare exact flat signed pair')
        output = self.parent / 'macos-signed-pair'
        self.assertEqual({path.name for path in output.iterdir()}, set(original))
        self.assertEqual({path.name: path.read_bytes() for path in output.iterdir()}, original)
        outputs = (self.parent / 'outputs').read_text()
        self.assertIn('archive_sha256=' + digest(original[SIGNED]), outputs)
        self.assertIn('image_sha256=' + digest(original[IMAGE]), outputs)
        with self.assertRaises(SystemExit):
            self.execute('Prepare exact flat signed pair')
        self.assertEqual({path.name: path.read_bytes() for path in output.iterdir()}, original)
        upload = step(self.workflow, 'Retain only the two final signed files')
        self.assertIn('name: macos-signed-pair-' + '$' + '{{ github.run_id }}-1', upload)
        self.assertIn('path: ' + '$' + '{{ runner.temp }}/macos-signed-pair/*', upload)
        self.assertNotIn('if: always()', upload)

    def test_pair_copy_detects_changed_owned_bytes_before_upload(self):
        for name in [SIGNED, IMAGE]:
            self.write('macos-signed/' + name, b'synthetic public bytes')
        def changed(path, limit, destination=None):
            result = bounded_file(path, limit, destination)
            if destination is not None:
                destination.write_bytes(b'changed')
            return result
        with mock.patch('package_macos_dmg.bounded_file', side_effect=changed), self.assertRaises(SystemExit):
            self.execute('Prepare exact flat signed pair')

    def test_actual_recovery_retains_only_same_submission_files_and_original_modes(self):
        self.zip_state()
        self.execute('Preserve narrow credentials-free same-submission recovery')
        destination = self.parent / 'macos-signing-recovery.tar'
        with tarfile.open(destination) as archive:
            members = archive.getmembers()
            self.assertEqual({member.name for member in members}, {
                'zip/input/' + UNSIGNED, 'zip/state.json', 'zip/submission.zip',
                'zip/payload/PeerBrush/PeerBrush.app/Contents/MacOS/peerbrush'})
            for member in members:
                self.assertTrue(member.isfile())
                self.assertEqual((member.uid, member.gid, member.uname, member.gname, member.mtime), (0, 0, '', '', 0))
            executable = archive.getmember('zip/payload/PeerBrush/PeerBrush.app/Contents/MacOS/peerbrush')
            if os.name != 'nt':
                self.assertEqual(executable.mode, 0o755)
            self.assertNotIn(b'NEVER UPLOAD', b''.join(archive.extractfile(member).read() for member in members))
        self.assertFalse((self.parent / 'macos-signing-recovery.tar.partial').exists())

    def test_uncertain_upload_without_id_is_preserved_but_changed_state_or_payload_refuses(self):
        original, payload, state = self.zip_state(submission=False)
        self.execute('Preserve narrow credentials-free same-submission recovery')
        self.assertTrue((self.parent / 'macos-signing-recovery.tar').is_file())
        # Every following case uses a fresh parent; the successful archive remains intact.
        for damage in ['private-state', 'changed-payload']:
            with self.subTest(damage=damage), tempfile.TemporaryDirectory(prefix='peerbrush-recovery-negative-') as temporary:
                self.parent = Path(temporary).resolve()
                self.environment['RUNNER_TEMP'] = str(self.parent)
                original, payload, state = self.zip_state()
                if damage == 'private-state':
                    state['private_account'] = 'NEVER UPLOAD'
                    self.write('macos-zip-work/state.json', json.dumps(state).encode())
                else:
                    payload.write_bytes(b'changed payload')
                with self.assertRaises(SystemExit):
                    self.execute('Preserve narrow credentials-free same-submission recovery')
                self.assertFalse((self.parent / 'macos-signing-recovery.tar').exists())

    def test_actual_dmg_recovery_uses_only_three_prepared_files_and_never_walks_finalization(self):
        original = self.write('macos-signed/' + SIGNED, b'synthetic public signed ZIP')
        self.environment['UNSIGNED_SHA256'] = '1' * 64
        submitted = self.write('macos-dmg-work/submission.dmg', b'synthetic submitted disk image')
        candidate = SIGNED[:-4] + '-dmg-candidate-r3.dmg'
        hashes = {}
        for name in [candidate, candidate + '.sha256', candidate + '.verification.json']:
            body = ('synthetic public ' + name).encode()
            self.write('macos-dmg-work/prepared/' + name, body)
            hashes[name] = digest(body)
        self.write('macos-dmg-work/.dmg-finalize-preserved/mounted/NEVER-WALK', b'NEVER UPLOAD')
        state = dict(configuration=dict(schema=1, format='dmg', input_sha256=digest(original.read_bytes()),
            commit=COMMIT, version=VERSION, team_id=self.environment['APPLE_TEAM'],
            certificate_sha1=self.environment['APPLE_IDENTITY_SHA1'], revision='dmg-signed-r3'),
            submission_sha256=digest(submitted.read_bytes()), prepared_hashes=hashes,
            payload={'Applications': {'link': '/Applications'}, 'PeerBrush.app': {'directory': True}})
        self.write('macos-dmg-work/state.json', json.dumps(state).encode())
        self.execute('Preserve narrow credentials-free same-submission recovery')
        with tarfile.open(self.parent / 'macos-signing-recovery.tar') as archive:
            self.assertEqual({member.name for member in archive.getmembers()},
                {'dmg/input/' + SIGNED, 'dmg/state.json', 'dmg/submission.dmg'} |
                {'dmg/prepared/' + name for name in hashes})

    def test_changed_final_recovery_input_cannot_publish_a_partial_archive(self):
        original, payload, state = self.zip_state()
        def changed(path, limit, destination=None):
            if path == original and (self.parent / 'macos-signing-recovery.tar.partial').exists():
                original.write_bytes(b'changed original')
            return bounded_file(path, limit, destination)
        with mock.patch('package_macos_dmg.bounded_file', side_effect=changed), self.assertRaises(SystemExit):
            self.execute('Preserve narrow credentials-free same-submission recovery')
        self.assertTrue((self.parent / 'macos-signing-recovery.tar.partial').exists())
        self.assertFalse((self.parent / 'macos-signing-recovery.tar').exists())

    def test_actual_tar_member_reads_reject_owned_mutation_with_unchanged_originals(self):
        original, payload, state = self.zip_state()
        expected_input, expected_payload = original.read_bytes(), payload.read_bytes()
        addfile = tarfile.TarFile.addfile
        changed = []
        def mutate(archive, info, fileobj=None):
            if info.name.endswith('/MacOS/peerbrush'):
                self.assertEqual(info.size, len(expected_payload))
                Path(fileobj.stream.name).write_bytes(b'!' * info.size)
                changed.append(info.name)
            return addfile(archive, info, fileobj)
        with mock.patch.object(tarfile.TarFile, 'addfile', new=mutate), self.assertRaises(SystemExit):
            self.execute('Preserve narrow credentials-free same-submission recovery')
        self.assertEqual(changed, ['zip/payload/PeerBrush/PeerBrush.app/Contents/MacOS/peerbrush'])
        self.assertEqual(original.read_bytes(), expected_input)
        self.assertEqual(payload.read_bytes(), expected_payload)
        self.assertTrue((self.parent / 'macos-signing-recovery.tar.partial').exists())
        self.assertFalse((self.parent / 'macos-signing-recovery.tar').exists())


if __name__ == '__main__':
    unittest.main()
