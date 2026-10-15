"""Offline regression tests; never authenticates, downloads or submits a real app."""
import copy
import hashlib
import io
import unittest
from unittest.mock import patch
import zipfile
import submit_store_release as s


def fixture():
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
    def test_unexpected_exception_redacted(self):
        output = io.StringIO()
        with patch.object(s.sys, 'argv', ['submit', '--tag', 'v0.26']), patch.object(s, 'released_assets', return_value=fixture()), patch.object(s, 'Store', side_effect=ValueError('FAKE_SECRET')), patch.object(s.sys, 'stderr', output):
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

    def test_package_and_tampering(self):
        name, package, report = fixture()
        checksum = f'{report["sha256"]}  {name}\n'
        s.validate_package(name, package, report, checksum, 'a'*40, '0.26.0', '1.26.0.0')
        for field, value in [('commit', 'b'*40), ('version', '1.25.0.0'), ('development_identity', True), ('package_family_name', 'other')]:
            altered = dict(report, **{field: value})
            with self.subTest(field=field), self.assertRaises(ValueError):
                s.validate_package(name, package, altered, checksum, 'a'*40, '0.26.0', '1.26.0.0')
        with self.assertRaises(ValueError):
            s.validate_package(name, package + b'bad', report, checksum, 'a'*40, '0.26.0', '1.26.0.0')

    def test_read_only_default(self):
        store = FakeStore()
        s.submit(store, *fixture())
        self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    def test_pending_never_deleted(self):
        store = FakeStore()
        store.app['pendingApplicationSubmission'] = {'id': '456'}
        with self.assertRaises(ValueError):
            s.submit(store, *fixture(), execute=True)
        self.assertEqual(len(store.calls), 1)

    def test_live_duplicate_and_downgrade(self):
        for current in ['1.26.0.0', '2.0.0.0']:
            store = FakeStore()
            store.current['applicationPackages'][0]['version'] = current
            with self.assertRaises(ValueError):
                s.submit(store, *fixture(), execute=True)
            self.assertTrue(all(method == 'GET' for _, method, _ in store.calls))

    def test_identity(self):
        store = FakeStore()
        store.app['publisherName'] = 'other'
        with self.assertRaises(ValueError):
            s.submit(store, *fixture(), execute=True)
        self.assertEqual(len(store.calls), 1)

    @patch.object(s, 'request', return_value=b'')
    def test_preserve_metadata_and_upload_before_commit(self, upload):
        store = FakeStore()
        s.submit(store, *fixture(), execute=True)
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
            s.submit(store, *fixture(), execute=True)
        self.assertFalse(any(path.endswith('/commit') for path, _, _ in store.calls))
        self.assertFalse(any(method == 'DELETE' for _, method, _ in store.calls))

    def test_other_architectures_are_not_removed(self):
        store = FakeStore()
        store.draft['applicationPackages'][0]['architecture'] = 'ARM64'
        with self.assertRaises(ValueError):
            s.submit(store, *fixture(), execute=True)
        self.assertFalse(any(method == 'PUT' for _, method, _ in store.calls))


if __name__ == '__main__':
    unittest.main()
