"""Offline regression tests; never authenticates, downloads or submits a real app."""
import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile
import submit_store_release as s
import test_msix


def fixture():
    """Protocol-only journal/Store fixture; deliberately not a valid MSIX."""
    name = 'PeerBrush-1.26.0.0-Windows-x64.msix'
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w') as archive:
        archive.writestr('AppxManifest.xml', f'<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"><Identity Name="{s.IDENTITY}" Publisher="{s.PUBLISHER}" Version="1.26.0.0" ProcessorArchitecture="x64"/></Package>')
    package = stream.getvalue()
    report = dict(schema=1, package=name, sha256=hashlib.sha256(package).hexdigest(),
        commit='a' * 40, app_version='0.26.0', version='1.26.0.0', package_family_name=s.FAMILY,
        development_identity=False, identity=dict(name=s.IDENTITY, publisher=s.PUBLISHER, publisher_display_name='Deftroy'))
    return name, package, report


class FakeStore:
    def __init__(self):
        self.calls = []
        self.app = dict(id=s.STORE_ID, packageIdentityName=s.IDENTITY, publisherName=s.PUBLISHER,
            packageFamilyName=s.FAMILY, lastPublishedApplicationSubmission={'id': '123'})
        self.current = dict(applicationPackages=[dict(fileName='old.msix', fileStatus='Uploaded', version='1.25.0.0', architecture='x64')])
        self.draft = dict(self.current, id='456', listings={'en-us': {'description': 'Keep me'}},
            pricing={'priceId': 'Free'}, targetPublishMode='Manual',
            fileUploadUrl='https://example.blob.core.windows.net/upload?sig=redacted')

    def call(self, path='', method='GET', data=None):
        self.calls.append((path, method, copy.deepcopy(data)))
        return copy.deepcopy({('', 'GET'): self.app, ('/submissions/123', 'GET'): self.current,
            ('/submissions', 'POST'): self.draft, ('/submissions/456/status', 'GET'): {'status': 'PreProcessing'}}.get((path, method), {}))


class Tests(unittest.TestCase):
    def setUp(self):
        from store_release_state import Journal
        directory = tempfile.TemporaryDirectory(prefix='peerbrush-store-submit-test-')
        self.addCleanup(directory.cleanup)
        self.journal = Journal(Path(directory.name).resolve() / 'receipt.json')
        self.journal.bind('v0.26.0', fixture()[0], fixture()[2])

    def test_unexpected_exception_redacted(self):
        output = io.StringIO()
        args = ['submit', '--tag', 'v0.26', '--source-root', '.', '--portable-sha256', 'b' * 64, '--msix-sha256', 'c' * 64]
        with patch.object(s.sys, 'argv', args), patch.object(s, 'released_assets', return_value=fixture()), patch.object(s, 'Store', side_effect=ValueError('FAKE_SECRET')), patch.object(s.sys, 'stderr', output):
            self.assertEqual(s.main(), 1)
        self.assertNotIn('FAKE_SECRET', output.getvalue())
        self.assertIn('ValueError', output.getvalue())

    def test_authenticated_redirects_disabled(self):
        self.assertIsNone(s.NoRedirect().redirect_request(None, None, 302, '', {}, 'https://other.example'))
        with patch.object(s.urllib.request, 'build_opener') as builder:
            builder.return_value.open.return_value.__enter__.return_value.read.return_value = b'{}'
            s.request('https://example.test', headers={'Authorization': 'Bearer fake'})
            self.assertIsInstance(builder.call_args.args[0], s.NoRedirect)

    def test_commit_failure_and_timeout(self):
        for status in ['CommitFailed', 'CommitStarted']:
            store = FakeStore()
            with patch.object(store, 'call', return_value={'status': status}), self.assertRaises(s.SafeError):
                s.await_commit(store, '456', timeout=0)

    def test_versions(self):
        self.assertEqual(s.release_version('v0.26'), ('0.26.0', '1.26.0.0'))
        self.assertEqual(s.release_version('v1.0.0'), ('1.0.0', '2.0.0.0'))
        for tag in ['v0.25', '0.25.0', 'v0.24', 'v0.26-rc1', 'v65535.0.0', 'x; echo bad']:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                s.release_version(tag)

    def test_release_downloads_bound_headers_and_actual_reads(self):
        for header, body, valid in [('5', b'12345', True), (None, b'12345', True),
                                    ('6', b'', False), ('bad', b'', False),
                                    ('-1', b'', False), (None, b'123456', False), ('1', b'123456', False)]:
            with self.subTest(header=header, bytes=len(body)), patch.object(s.urllib.request, 'build_opener') as builder:
                response = builder.return_value.open.return_value.__enter__.return_value
                response.headers = {} if header is None else {'Content-Length': header}
                response.read.return_value = body
                if valid:
                    self.assertEqual(s.request('https://example.test', binary=True, limit=5), body)
                    response.read.assert_called_once_with(6)
                else:
                    with self.assertRaises(s.SafeError):
                        s.request('https://example.test', binary=True, limit=5)
                    if header in ['6', 'bad', '-1']:
                        response.read.assert_not_called()

    def test_cli_requires_independent_selection_before_remote_access_or_auth(self):
        for extra in [[], ['--source-root', '.'], ['--portable-sha256', 'b' * 64],
                      ['--source-root', '.', '--portable-sha256', 'not-a-hash'],
                      ['--source-root', '.', '--portable-sha256', 'b' * 64],
                      ['--source-root', '.', '--portable-sha256', 'b' * 64, '--msix-sha256', 'not-a-hash']]:
            with self.subTest(args=extra), patch.object(s.sys, 'argv', ['submit', '--tag', 'v0.26.0', *extra]), \
                    patch.object(s, 'github') as remote, patch.object(s, 'Store') as store, patch.object(s.sys, 'stderr', io.StringIO()):
                self.assertEqual(s.main(), 1)
                remote.assert_not_called()
                store.assert_not_called()

    def test_read_only_default(self):
        store = FakeStore()
        s.submit(store, *fixture())
        self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    def test_pending_never_deleted(self):
        store = FakeStore()
        store.app['pendingApplicationSubmission'] = {'id': '456'}
        with self.assertRaises(ValueError):
            s.submit(store, *fixture(), execute=True, journal=self.journal)
        self.assertEqual(len(store.calls), 1)

    def test_live_duplicate_and_downgrade(self):
        for current in ['1.26.0.0', '2.0.0.0']:
            store = FakeStore()
            store.current['applicationPackages'][0]['version'] = current
            with self.assertRaises(ValueError):
                s.submit(store, *fixture(), execute=True, journal=self.journal)
            self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    def test_identity(self):
        store = FakeStore()
        store.app['publisherName'] = 'other'
        with self.assertRaises(ValueError):
            s.submit(store, *fixture(), execute=True, journal=self.journal)
        self.assertEqual(len(store.calls), 1)

    @patch.object(s, 'request', return_value=b'')
    def test_preserve_metadata_and_upload_before_commit(self, upload):
        store = FakeStore()
        s.submit(store, *fixture(), execute=True, journal=self.journal)
        updated = next(data for _, method, data in store.calls if method == 'PUT')
        for key in ('listings', 'pricing', 'targetPublishMode'):
            self.assertEqual(updated[key], store.draft[key])
        self.assertEqual(updated['applicationPackages'][0]['fileStatus'], 'PendingDelete')
        self.assertEqual(updated['applicationPackages'][1], {'fileName': fixture()[0], 'fileStatus': 'PendingUpload', 'minimumDirectXVersion': 'None', 'minimumSystemRam': 'None'})
        self.assertEqual(store.calls[-2][:2], ('/submissions/456/commit', 'POST'))
        self.assertEqual(upload.call_count, 1)
        self.assertEqual(store.draft['applicationPackages'][0]['fileStatus'], 'Uploaded')

    @patch.object(s, 'request', side_effect=RuntimeError('Upload failed'))
    def test_upload_failure_leaves_draft_no_commit(self, upload):
        store = FakeStore()
        with self.assertRaises(RuntimeError):
            s.submit(store, *fixture(), execute=True, journal=self.journal)
        self.assertFalse(any(path.endswith('/commit') for path, _, _ in store.calls))
        self.assertFalse(any(method == 'DELETE' for _, method, _ in store.calls))

    def test_other_architectures_are_not_removed(self):
        store = FakeStore()
        store.draft['applicationPackages'][0]['architecture'] = 'ARM64'
        with self.assertRaises(ValueError):
            s.submit(store, *fixture(), execute=True, journal=self.journal)
        self.assertFalse(any(method == 'PUT' for _, method, _ in store.calls))


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.msix = test_msix.Msix()
        self.msix.setUp()
        self.addCleanup(self.msix.doCleanups)
        self.source = self.msix.root
        fixture = self.msix.fixture
        for name in ['Cargo.toml', 'Cargo.lock']:
            path = self.source / name
            path.write_text(path.read_text().replace('0.2.0', '0.26.0'))
        build_info = fixture.build_info
        fixture.build_info = lambda commit=None: dict(build_info(commit), version='0.26.0')
        fixture.require_provenance()
        self.portable = fixture.build('Windows')
        self.portable_sha = s.msix.portable.digest(self.portable)
        identity = dict(name=s.IDENTITY, publisher=s.PUBLISHER, publisher_display_name='Deftroy')
        self.payload, release = s.msix.expected_payload(self.source, self.portable, identity, require_provenance=True)
        self.commit = release['commit']
        self.name = 'PeerBrush-1.26.0.0-Windows-x64.msix'
        self.package = test_msix.synthetic_package(self.payload)
        self.report = dict(schema=1, package=self.name, sha256=hashlib.sha256(self.package).hexdigest(),
            commit=self.commit, app_version='0.26.0', version='1.26.0.0', identity=identity,
            package_family_name=s.FAMILY, development_identity=False, signed=False,
            portable_sha256=self.portable_sha, installed_tested=False,
            file_types=['.psd', '.psb'], validation=s.msix.VALIDATION)

    def validate(self, package=None, report=None, **selection):
        package = self.package if package is None else package
        report = dict(self.report, sha256=hashlib.sha256(package).hexdigest()) if report is None else report
        options = dict(source_root=self.source, portable_path=self.portable, portable_sha256=self.portable_sha,
                       msix_sha256=hashlib.sha256(package).hexdigest())
        options.update(selection)
        s.validate_package(self.name, package, report, f'{hashlib.sha256(package).hexdigest()}  {self.name}\n',
                           self.commit, '0.26.0', '1.26.0.0', **options)

    def published(self, package=None):
        package = self.package if package is None else package
        report = dict(self.report, sha256=hashlib.sha256(package).hexdigest())
        blobs = {self.name: package, self.name + '.json': json.dumps(report).encode(),
                 self.name + '.sha256': f'{report["sha256"]}  {self.name}\n'.encode(),
                 self.portable.name: self.portable.read_bytes()}
        base = f'https://github.com/{s.REPOSITORY}/releases/download/v0.26.0/'
        release = dict(tag_name='v0.26.0', published_at='2026-01-01T00:00:00Z', draft=False, prerelease=False,
            assets=[dict(name=name, size=len(data), browser_download_url=base + name) for name, data in blobs.items()])
        return release, blobs

    def test_full_payload_and_exact_source_pass_read_only_verification(self):
        self.validate()
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            self.validate(portable_sha256='0' * 64)
        with self.assertRaisesRegex(ValueError, 'MSIX archive differs'):
            self.validate(msix_sha256='0' * 64)
        with patch.object(s.msix.portable, 'git', wraps=s.msix.portable.git) as git:
            self.validate()
            self.assertTrue(all(args[1] in {'rev-parse', 'status', 'ls-tree', 'cat-file'} for args, _ in git.call_args_list))

    def test_manifest_only_or_repacked_payload_cannot_self_authorize_with_new_reports(self):
        self.validate()
        candidates = [fixture()[1]]
        for remove in ['peerbrush.exe', 'release.json', 'PeerBrush-0.26.0-source.zip', 'licenses/ubuntu-sans/LICENCE.txt']:
            changed = dict(self.payload)
            changed.pop(remove)
            candidates.append(test_msix.synthetic_package(changed))
        candidates.append(test_msix.synthetic_package(dict(self.payload, **{'unlisted.txt': b'private canary'})))
        candidates.append(test_msix.synthetic_package(self.payload, lambda files: files.update({'AppxSignature.p7x': b'not approved'})))
        changed = dict(self.payload)
        changed['AppxManifest.xml'] = changed['AppxManifest.xml'].replace(b'peerbrush-mcp.exe', b'other-alias.exe')
        candidates.append(test_msix.synthetic_package(changed))
        for index, candidate in enumerate(candidates):
            with self.subTest(candidate=index), self.assertRaises(ValueError):
                self.validate(candidate)

    def test_report_cannot_change_independently_verified_facts_or_claim_native_acceptance(self):
        changes = [('commit', 'b' * 40), ('version', '1.25.0.0'), ('development_identity', True),
                   ('development_identity', 0), ('signed', 0), ('signed', True), ('schema', True),
                   ('package_family_name', 'other'), ('portable_sha256', 'b' * 64),
                   ('installed_tested', True), ('file_types', ['.psd', '.psb', '.png']),
                   ('identity', dict(self.report['identity'], publisher='CN=Other'))]
        for field, value in changes:
            with self.subTest(field=field, value=value), self.assertRaisesRegex(ValueError, 'report mismatch'):
                self.validate(report=dict(self.report, **{field: value}))

    def test_release_discovery_selects_matching_source_and_portable_before_authentication(self):
        release, blobs = self.published()
        def download(url, **options):
            data = blobs[url.rsplit('/', 1)[-1]]
            self.assertGreaterEqual(options['limit'], len(data))
            self.assertTrue(options['allow_redirects'])
            return data
        with patch.object(s, 'github', side_effect=[release, {'sha': self.commit}]), patch.object(s, 'request', side_effect=download):
            name, package, report = s.released_assets('v0.26.0', self.source, self.portable_sha, self.report['sha256'])
        self.assertEqual((name, package, report), (self.name, self.package, self.report))
        with patch.object(s, 'github', side_effect=[release, {'sha': 'b' * 40}]), patch.object(s, 'request') as download:
            with self.assertRaisesRegex(ValueError, 'resolved release commit'):
                s.released_assets('v0.26.0', self.source, self.portable_sha, self.report['sha256'])
            download.assert_not_called()
        (self.source / 'src/lib.rs').write_text('// tracked change')
        with patch.object(s, 'github') as remote, self.assertRaisesRegex(ValueError, 'tracked changes'):
            s.released_assets('v0.26.0', self.source, self.portable_sha, self.report['sha256'])
        remote.assert_not_called()

    def test_invalid_final_package_and_oversized_assets_stop_before_store_auth(self):
        release, blobs = self.published(fixture()[1])
        args = ['submit', '--tag', 'v0.26.0', '--source-root', str(self.source), '--portable-sha256', self.portable_sha,
                '--msix-sha256', hashlib.sha256(blobs[self.name]).hexdigest()]
        with patch.object(s.sys, 'argv', args), patch.object(s, 'github', side_effect=[release, {'sha': self.commit}]), \
                patch.object(s, 'request', side_effect=lambda url, **_: blobs[url.rsplit('/', 1)[-1]]), \
                patch.object(s, 'Store') as store, patch.object(s.sys, 'stderr', io.StringIO()):
            self.assertEqual(s.main(), 1)
            store.assert_not_called()
        release['assets'][0]['size'] = s.msix.PACKAGE_LIMIT + 1
        with patch.object(s, 'github', side_effect=[release, {'sha': self.commit}]), patch.object(s, 'request') as download:
            with self.assertRaisesRegex(ValueError, 'download budget'):
                s.released_assets('v0.26.0', self.source, self.portable_sha, self.report['sha256'])
            download.assert_not_called()

    def test_changed_final_hash_cannot_be_authorized_by_matching_report_and_checksum(self):
        release, blobs = self.published(self.package + b'changed')
        args = ['submit', '--tag', 'v0.26.0', '--source-root', str(self.source), '--portable-sha256', self.portable_sha,
                '--msix-sha256', self.report['sha256']]
        output = io.StringIO()
        with patch.object(s.sys, 'argv', args), patch.object(s, 'github', side_effect=[release, {'sha': self.commit}]), \
                patch.object(s, 'request', side_effect=lambda url, **_: blobs[url.rsplit('/', 1)[-1]]), \
                patch.object(s, 'Store') as store, patch.object(s.sys, 'stderr', output):
            self.assertEqual(s.main(), 1)
            store.assert_not_called()
        self.assertIn('MSIX archive differs from the independently selected SHA-256', output.getvalue())


if __name__ == '__main__':
    unittest.main()
