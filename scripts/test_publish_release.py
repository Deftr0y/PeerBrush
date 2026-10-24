"""Credential-free transaction failures using opaque bytes, never native/release authority."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
from urllib.parse import unquote

import final_release as final
import publish_release as publishing


class Service:
    def __init__(self, manifest):
        self.manifest, self.current, self.data = manifest, None, {}
        self.mutations, self.failure = [], None
        self.main = manifest['commit']
        self.old = []

    def commit(self, ref):
        if ref == 'main': return dict(sha=self.main)
        return dict(sha=self.manifest['commit']) if self.current and not self.current['draft'] else None

    def history(self):
        return copy.deepcopy(self.old)

    def release(self, tag):
        return copy.deepcopy(self.current)

    def create(self, tag, commit, name, notes):
        self.mutations.append(('create', tag))
        self.current = dict(id=17, tag_name=tag, target_commitish=commit, name=name, body=notes, draft=True, prerelease=False,
                            published_at='2026-10-23T12:00:00Z', assets=[])
        if self.failure == 'create':
            self.failure = None
            raise publishing.AmbiguousMutation('create outcome uncertain')
        return self.release(tag)

    def upload(self, identifier, path):
        self.mutations.append(('upload', path.name))
        data = path.read_bytes()
        self.data[path.name] = data
        self.current['assets'].append(dict(id=100 + len(self.data), name=path.name, state='uploaded', size=len(data), digest='sha256:' + hashlib.sha256(data).hexdigest()))
        if self.failure == 'upload':
            self.failure = None
            raise publishing.AmbiguousMutation('upload outcome uncertain')

    def publish(self, identifier):
        self.mutations.append(('publish', identifier))
        self.current['draft'] = False
        if self.failure == 'publish':
            self.failure = None
            raise publishing.AmbiguousMutation('publish outcome uncertain')

    def asset_hash(self, asset):
        return hashlib.sha256(self.data[asset['name']]).hexdigest()

    def public_get(self, url, limit, *, api=False, destination=None, **unused):
        from prepare_site_release import API, DOWNLOAD, RELEASE, Unavailable
        tag = 'v' + self.manifest['version']
        if api:
            if url == API + 'commits/' + tag:
                return final.encode(dict(sha=self.manifest['commit']))
            if url != API + 'releases/tags/' + tag or self.current is None:
                raise Unavailable('not public')
            release = self.release(tag)
            release['html_url'] = RELEASE + tag
            for asset in release['assets']:
                asset['browser_download_url'] = DOWNLOAD + tag + '/' + asset['name']
            return final.encode(release)
        name = unquote(url.rsplit('/', 1)[-1])
        if name not in self.data:
            raise Unavailable('selected public asset unavailable')
        data = self.data[name]
        if len(data) > limit:
            raise ValueError('public budget exceeded')
        if destination is not None:
            with destination.open('xb') as stream: stream.write(data)
            return destination
        return data


class Transactions(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='peerbrush-publish-mechanics-')
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        version, commit = '0.2.0', 'a' * 40
        names = ['PeerBrush-0.2.0-source.zip', 'PeerBrush-0.2.0-Windows-x64.zip',
            'PeerBrush-0.2.0-macOS-arm64-apple-signed-r1.zip', 'PeerBrush-0.2.0-Linux-x64.zip',
            'peerbrush_0.2.0-1_amd64.deb', 'PeerBrush-0.2.0-macOS-arm64-apple-signed-r1-dmg-signed-r1.dmg']
        source = hashlib.sha256(('opaque mechanical fixture ' + names[0]).encode()).hexdigest()
        base = []
        for index, name in enumerate(names):
            record = dict(file=name, bytes=len(('opaque mechanical fixture ' + name).encode()), sha256=hashlib.sha256(('opaque mechanical fixture ' + name).encode()).hexdigest())
            if index:
                platform = 'Windows' if index == 1 else 'macOS' if index in [2, 5] else 'Linux'
                record.update(platform=platform, architecture='amd64' if index == 4 else 'arm64' if platform == 'macOS' else 'x64', binary_source_commit=commit, source_sha256=source)
                if name.endswith('.zip'):
                    record.update(executable_sha256='b' * 64, native_publisher_signing='unsigned', signing_verification='unsigned')
            base.append(record)
        package, runtime, descriptor = 'PeerBrush-1.2.0.0-Windows-x64-signed-r1.msix', 'Microsoft.VCLibs.x64.Desktop.appx', 'PeerBrush-1.2.0.0-Windows-x64.appinstaller'
        extra = [package, runtime, descriptor, names[4] + '.json', names[4] + '.sha256', names[5] + '.qualification.json', names[5] + '.sha256']
        all_names = names + extra
        files = []
        for name in all_names:
            data = ('opaque mechanical fixture ' + name).encode()
            (self.directory / name).write_bytes(data)
            files.append(dict(file=name, bytes=len(data), sha256=hashlib.sha256(data).hexdigest()))
        channel = lambda platform, name, dependencies: dict(platform=platform, file=name, dependencies=dependencies, installed_acceptance='unverified', ready=False)
        self.manifest = dict(schema=1, format=final.FORMAT, version=version, commit=commit,
            base_inventory=dict(schema=1, version=version, commit=commit, binary_source_commit=commit, packaging_correction=None, assets=base),
            files=sorted(files, key=lambda r:r['file']), channels={
                'windows-direct':channel('Windows', descriptor, [package, runtime]),
                'macos-direct':channel('macOS', names[5], []), 'linux-direct':channel('Linux', names[4], []),
                'microsoft-store':final.store_channel()})
        (self.directory / final.MANIFEST).write_bytes(final.encode(self.manifest))
        (self.directory / 'SHA256SUMS').write_bytes(final.checksum_bytes(self.manifest))
        self.service = Service(self.manifest)
        self.notes = 'Reviewed fixture release notes. No real release.'

    def transfer(self, readback=None):
        return publishing._transfer(self.service, self.directory, self.manifest, self.notes,
            readback or (lambda manifest: publishing.public_readback(manifest, self.service.public_get)))

    def test_complete_transaction_and_repeated_invocation_never_duplicate_or_replace(self):
        result = self.transfer()
        self.assertEqual(result['public_availability'], 'verified')
        self.assertFalse(result['production_ready'])
        first = self.service.mutations.copy()
        self.assertEqual(sum(kind == 'create' for kind, _ in first), 1)
        self.assertEqual(sum(kind == 'publish' for kind, _ in first), 1)
        self.assertEqual(len([name for kind, name in first if kind == 'upload']), len(set(self.service.data)))
        self.transfer()
        self.assertEqual(self.service.mutations, first)

    def test_ambiguous_create_upload_and_publish_are_not_retried_until_fresh_reconciliation(self):
        for failure in ['create', 'upload', 'publish']:
            self.service = Service(self.manifest)
            self.service.failure = failure
            with self.subTest(failure=failure), self.assertRaises(publishing.AmbiguousMutation):
                self.transfer()
            first = self.service.mutations.copy()
            self.assertEqual(sum(kind == failure for kind, _ in first), 1)
            self.transfer()
            mutations = self.service.mutations
            self.assertEqual(sum(kind == 'create' for kind, _ in mutations), 1)
            self.assertEqual(sum(kind == 'publish' for kind, _ in mutations), 1)
            uploads = [name for kind, name in mutations if kind == 'upload']
            self.assertEqual(len(uploads), len(set(uploads)))

    def test_postpublication_readback_failure_never_triggers_republish_or_ready_result(self):
        with self.assertRaisesRegex(ValueError, 'visibility delay'):
            self.transfer(lambda manifest: (_ for _ in ()).throw(ValueError('visibility delay')))
        first = self.service.mutations.copy()
        self.assertFalse(self.service.current['draft'])
        self.assertEqual(self.transfer()['public_availability'], 'verified')
        self.assertEqual(self.service.mutations, first)

    def test_existing_wrong_bytes_size_names_notes_and_published_missing_files_refuse_replacement(self):
        self.transfer()
        original = copy.deepcopy(self.service.current)
        data = self.service.data.copy()
        first = self.service.mutations.copy()
        for change in ['bytes', 'size', 'extra', 'missing', 'notes', 'source']:
            self.service.current = copy.deepcopy(original)
            self.service.data = data.copy()
            if change == 'bytes':
                name = self.service.current['assets'][0]['name']
                raw = self.service.data[name]
                self.service.data[name] = b'!' + raw[1:]
            elif change == 'size': self.service.current['assets'][0]['size'] += 1
            elif change == 'extra': self.service.current['assets'][0]['name'] = 'private-unexpected.json'
            elif change == 'missing': self.service.current['assets'].pop()
            elif change == 'notes': self.service.current['body'] = 'different existing notes'
            else: self.service.current['target_commitish'] = 'b' * 40
            with self.subTest(change=change), self.assertRaises(ValueError): self.transfer()
            self.assertEqual(self.service.mutations, first)

    def test_changed_local_or_missing_platform_inputs_never_create_a_remote_release(self):
        name = self.manifest['channels']['windows-direct']['file']
        path = self.directory / name
        original = path.read_bytes()
        for change in ['changed', 'missing', 'extra']:
            if change == 'changed': path.write_bytes(b'changed')
            elif change == 'missing': path.unlink()
            else: (self.directory / 'private-recovery.json').write_bytes(b'never upload')
            with self.subTest(change=change), self.assertRaises(ValueError): self.transfer()
            self.assertEqual(self.service.mutations, [])
            path.write_bytes(original)
            (self.directory / 'private-recovery.json').unlink(missing_ok=True)

    def test_exact_main_tag_and_no_downgrade_are_required_before_creation(self):
        self.service.main = 'b' * 40
        with self.assertRaises(ValueError): self.transfer()
        self.assertEqual(self.service.mutations, [])
        self.service.main = self.manifest['commit']
        self.service.old = [dict(tag_name='v0.26.0', draft=False, prerelease=False)]
        with self.assertRaisesRegex(ValueError, 'downgrade'): self.transfer()
        self.assertEqual(self.service.mutations, [])
        self.service.old = []
        with mock.patch.object(self.service, 'commit', return_value=dict(sha='b' * 40)):
            with self.assertRaises(ValueError): self.transfer()

    def test_source_moving_during_upload_preserves_draft_without_publishing(self):
        original = self.service.upload
        def changed(identifier, path):
            original(identifier, path)
            self.service.main = 'b' * 40
        with mock.patch.object(self.service, 'upload', side_effect=changed), self.assertRaisesRegex(ValueError, 'changed before publication'):
            self.transfer()
        self.assertTrue(self.service.current['draft'])
        self.assertFalse(any(kind == 'publish' for kind, _ in self.service.mutations))

    def test_actual_public_readback_refuses_missing_wrong_or_draft_asset_state(self):
        self.transfer()
        first = self.service.mutations.copy()
        name = self.manifest['channels']['macos-direct']['file']
        raw = self.service.data.pop(name)
        with self.assertRaises(ValueError): publishing.public_readback(self.manifest, self.service.public_get)
        self.service.data[name] = b'!' + raw[1:]
        with self.assertRaises(ValueError): publishing.public_readback(self.manifest, self.service.public_get)
        self.service.data[name] = raw
        self.service.current['draft'] = True
        with self.assertRaises(ValueError): publishing.public_readback(self.manifest, self.service.public_get)
        self.assertEqual(self.service.mutations, first)

    def test_reports_cannot_construct_publication_authority_and_no_producer_or_approval_is_inferred(self):
        with self.assertRaises(ValueError):
            final.Candidate(None, self.directory, self.manifest, dict(production_ready=True), object())
        with self.assertRaisesRegex(ValueError, 'JSON report'):
            publishing.publish(dict(production_ready=True, blockers=[]), self.service, self.notes, execute=True)
        self.assertEqual(self.service.mutations, [])

    def test_service_mutation_error_is_redacted_and_never_automatically_retried(self):
        connection = mock.Mock()
        connection.request.side_effect = OSError('SYNTHETIC_PRIVATE_TRANSPORT_DETAIL')
        client = publishing.GitHub('SYNTHETIC_TEST_CREDENTIAL_VALUE')
        with mock.patch.object(publishing.http.client, 'HTTPSConnection', return_value=connection), \
                self.assertRaises(publishing.AmbiguousMutation) as error:
            client.create('v0.2.0', self.manifest['commit'], 'PeerBrush 0.2.0', self.notes)
        self.assertNotIn('SYNTHETIC_', str(error.exception))
        self.assertEqual(connection.request.call_count, 1)
        connection.close.assert_called_once()

    def test_preparation_workflow_has_no_publication_authority_or_ready_report_override(self):
        root = Path(__file__).resolve().parent.parent
        text = (root / '.github/workflows/production.yml').read_text(encoding='utf-8')
        self.assertIn('contents: read', text)
        self.assertNotIn('contents: write', text)
        self.assertIn('if: ${{ false }}', text.split('\n  publish:\n', 1)[1])
        acquire = text.split('      - name: Acquire separate exact final-input producer data\n', 1)[1].split('\n      - ', 1)[0]
        self.assertIn('python -B scripts/acquire_final_inputs.py --source-root "$GITHUB_WORKSPACE"', acquire)
        self.assertIn('--commit "$EXPECTED_COMMIT" --candidate-run-id "$CANDIDATE_RUN"', acquire)
        self.assertIn('--run-id "$FINAL_INPUTS_RUN" --artifact-id "$FINAL_INPUTS_ARTIFACT"', acquire)
        self.assertIn('--artifact-sha256 "$FINAL_INPUTS_ARTIFACT_SHA256" --bundle-sha256 "$FINAL_INPUTS_BUNDLE_SHA256"', acquire)
        self.assertIn('--index-sha256 "$FINAL_INPUTS_INDEX_SHA256" --selection-sha256 "$SELECTION_SHA256"', acquire)
        self.assertIn('--selection-sha256 "$SELECTION_SHA256"', text)
        preflight = text.split('      - name: Revalidate actual final inputs through the publication preflight\n', 1)[1].split('\n      - ', 1)[0]
        self.assertIn('scripts/publish_release.py --source-root', preflight)
        self.assertIn('--final-input-index-sha256 "$FINAL_INPUTS_INDEX_SHA256"', preflight)
        self.assertIn('--notes "$GITHUB_WORKSPACE/docs/release-0.30.md" --notes-sha256 "$RELEASE_NOTES_SHA256"', preflight)
        self.assertNotIn('--execute', preflight)
        self.assertNotIn('GH_TOKEN:', preflight)
        for name in ['ci.yml', 'release.yml']:
            caller = (root / '.github/workflows' / name).read_text(encoding='utf-8')
            self.assertIn('python -B scripts/test_final_release.py', caller)
            self.assertIn('python -B scripts/test_publish_release.py', caller)


if __name__ == '__main__':
    unittest.main()
