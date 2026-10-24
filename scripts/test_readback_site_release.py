"""Readback regressions use fake HTTP and synthetic source, never live releases."""
import contextlib
import copy
import io
import http.client
import json
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock
import urllib.error
import urllib.parse
import urllib.request

import prepare_site_release as site
import readback_site_release as readback
import test_prepare_site_release


ROOT = pathlib.Path(__file__).resolve().parents[1]


class Readback(unittest.TestCase):
    def setUp(self):
        self.fixture = test_prepare_site_release.Published(methodName='runTest')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.entry, excluded = self.fixture.published()
        self.assertEqual(excluded, [])
        captured = json.loads((ROOT / 'tests/fixtures/site-release-output.json').read_bytes())
        previous = captured['files']
        data = json.loads(previous['downloads.json'])
        retained = dict(data['history'][0], version='v0.1.0', label='PeerBrush 0.1.0 · Stable')
        data.update({k: v for k, v in retained.items() if k not in {'kind', 'notes', 'publishedAt'}})
        data['history'] = [retained]
        previous['downloads.json'] = json.dumps(data)
        self.previous = copy.deepcopy(previous)
        self.files = site.render(previous, self.entry)
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-site-readback-')
        self.addCleanup(self.temporary.cleanup)
        self.directory = pathlib.Path(self.temporary.name).resolve()
        (self.directory / 'public-site').mkdir()
        for name, raw in self.files.items():
            (self.directory / 'public-site' / name).write_bytes(raw)
        self.report = dict(schema=1, version='v0.2.0', commit=self.fixture.commit, site_commit=captured['sourceCommit'],
                           site_update_prepared=True, deployment_performed=False, production_readiness='not-assessed',
                           store_public_availability='unverified', excluded=[],
                           release_inventory_sha256=site.inventory_sha(self.fixture.inventory),
                           files={name: site.sha(raw) for name, raw in self.files.items()})
        self.save_report()
        self.fixture.assets['release-inventory.json'] = site.encode(self.fixture.inventory)
        self.fixture.metadata()
        self.urls = readback.resource_urls(self.files)
        self.overrides = {}
        self.requests = []

    def save_report(self):
        (self.directory / 'website-output.json').write_bytes(site.encode(self.report))

    def get(self, url, limit, *, website=False, api=False, destination=None):
        self.requests.append(url)
        if url in self.overrides:
            value = self.overrides[url]
            if isinstance(value, Exception):
                raise value
            return value
        if website:
            self.assertTrue(site.public_url(url, website=True))
            return self.files[self.urls[url]]
        return self.fixture.get(url, limit, api=api, destination=destination)

    def check(self):
        return readback.check(self.directory, self.root, self.get)

    def independent(self, result):
        for key, value in readback.INDEPENDENT_STATUS.items():
            self.assertEqual(result[key], value)
        self.assertFalse(any('store' in url.lower() for url in self.requests))
        self.assertLess(len(site.encode(result)), 16 * 1024)

    def test_exact_deployed_contract_and_real_archive_validation_stay_separate(self):
        result = self.check()
        self.assertEqual(result['status'], 'verified')
        self.assertEqual(result['website']['status'], 'verified')
        self.assertEqual(result['downloads']['status'], 'verified')
        self.assertEqual(set(result['downloads']['verified_platforms']), {'windows', 'macos', 'linux'})
        self.assertEqual(set(self.urls), set(self.requests) & set(self.urls))
        self.assertLessEqual(len(result['website']['files']), 16)
        self.assertIn(site.WEBSITE, self.requests)
        self.assertTrue(any('downloads.json?release=v0.2.0' in url for url in self.requests))
        revision = str(int(site.sha(self.files['downloads.json'])[:12], 16))
        self.assertTrue(any('main.js?release=v0.2.0&layout=' + revision in url for url in self.requests))
        self.independent(result)

    def test_historical_target_cannot_skip_verification_of_latest_downloads(self):
        newer = dict(self.entry, version='v0.30.0', label='PeerBrush 0.30.0 · Stable')
        self.files = site.render({k: v.decode() for k, v in self.files.items()}, newer)
        for name, raw in self.files.items():
            (self.directory / 'public-site' / name).write_bytes(raw)
        self.report['files'] = {name: site.sha(raw) for name, raw in self.files.items()}
        self.save_report()
        self.urls = readback.resource_urls(self.files)
        result = self.check()
        self.assertEqual(result['reason'], 'expected-overlay-or-source-unavailable')
        self.assertEqual(json.loads(self.files['downloads.json'])['version'], 'v0.30.0')
        self.assertEqual(self.requests, [])

    def test_expected_history_must_pin_the_prepared_public_inventory(self):
        data = json.loads(self.files['downloads.json'])
        data['history'][0]['releaseInventorySha256'] = 'f' * 64
        data['releaseInventorySha256'] = 'f' * 64
        self.files['downloads.json'] = site.encode(data)
        (self.directory / 'public-site/downloads.json').write_bytes(self.files['downloads.json'])
        self.report['files']['downloads.json'] = site.sha(self.files['downloads.json'])
        self.save_report()
        self.assertEqual(self.check()['reason'], 'expected-overlay-or-source-unavailable')
        self.assertEqual(self.requests, [])

    def test_stale_canonical_and_query_cached_files_are_independently_rejected(self):
        cached = next(url for url in self.urls if 'downloads.json?' in url)
        for url in [site.WEBSITE, site.WEBSITE + 'main.js', cached]:
            self.overrides = {url: b'SYNTHETIC_STALE_CACHE'}
            with self.subTest(path=urllib.parse.urlsplit(url).path):
                result = self.check()
                self.assertEqual(result['website']['status'], 'mismatch')
                self.assertEqual(result['downloads']['status'], 'verified')
                row = next(r for r in result['website']['files'] if r['path'] == url.removeprefix(site.WEBSITE[:-1]))
                self.assertEqual(row['actual_sha256'], site.sha(b'SYNTHETIC_STALE_CACHE'))
                self.independent(result)

    def test_unavailable_site_file_does_not_claim_download_or_native_failure(self):
        self.overrides[site.WEBSITE + 'releases.html'] = site.Unavailable('SYNTHETIC_REMOTE_DETAIL_NOT_FOR_LOGS')
        result = self.check()
        self.assertEqual(result['website']['status'], 'unavailable')
        self.assertEqual(result['downloads']['status'], 'verified')
        self.assertNotIn('SYNTHETIC_REMOTE_DETAIL', site.encode(result).decode())
        self.independent(result)

    def test_missing_public_inventory_is_unavailable_without_private_receipt_fallback(self):
        del self.fixture.assets['release-inventory.json']
        self.fixture.metadata()
        result = self.check()
        self.assertEqual(result['website']['status'], 'verified')
        self.assertEqual(result['downloads']['status'], 'unavailable')
        self.assertEqual(result['downloads']['reason'], 'public-release-inventory-unavailable')
        self.independent(result)

    def test_actual_final_public_layout_reaches_full_overlay_readback_with_only_an_eligible_portable(self):
        import final_release as final
        import test_final_release
        from test_publish_release import Service
        composition = test_final_release.Composition(methodName='runTest')
        composition.release_policy = dict(schema=1, formats={'Windows': 'zip', 'macOS': 'dmg', 'Linux': 'deb'})
        self.addCleanup(composition.doCleanups)
        composition.setUp()
        candidate = composition.compose()
        service = Service(candidate.manifest)
        tag = 'v' + candidate.manifest['version']
        service.create(tag, candidate.manifest['commit'], 'PeerBrush ' + candidate.manifest['version'], 'Reviewed fixture notes')
        for path in candidate.directory.iterdir(): service.upload(17, path)
        service.publish(17)
        entry, excluded = site.published(composition.root, candidate.manifest['base_inventory'], tag,
            candidate.manifest['commit'], dict(notes=['Reviewed fixture notes'], caution='Native acceptance remains incomplete.'),
            service.public_get)
        self.assertEqual([p['id'] for p in entry['platforms']], ['windows'])
        self.assertNotIn('release-inventory.json', service.data)
        self.files = site.render(self.previous, entry)
        for name, data in self.files.items(): (self.directory / 'public-site' / name).write_bytes(data)
        self.report.update(version=tag, commit=candidate.manifest['commit'], excluded=excluded,
            release_inventory_sha256=site.inventory_sha(candidate.manifest['base_inventory']),
            files={name: site.sha(data) for name, data in self.files.items()})
        self.save_report()
        self.urls = readback.resource_urls(self.files)
        self.requests.clear()
        def get(url, limit, *, website=False, **options):
            self.requests.append(url)
            if website: return self.files[self.urls[url]]
            return service.public_get(url, limit, **options)
        result = readback.check(self.directory, composition.root, get)
        self.assertEqual(result['status'], 'verified')
        self.assertEqual(result['website']['status'], 'verified')
        self.assertEqual(result['downloads']['verified_platforms'], ['windows'])
        self.assertNotIn(site.DOWNLOAD + tag + '/release-inventory.json', self.requests)
        self.independent(result)
        name = next(r['file'] for r in candidate.manifest['base_inventory']['assets']
                    if r.get('platform') == 'Windows')
        del service.data[name]
        result = readback.check(self.directory, composition.root, get)
        self.assertEqual(result['website']['status'], 'verified')
        self.assertEqual(result['downloads']['status'], 'unavailable')
        self.assertNotIn('windows', result['downloads']['verified_platforms'])
        self.assertFalse(any(channel['ready'] for channel in candidate.manifest['channels'].values()))
        self.assertEqual(candidate.manifest['channels']['microsoft-store'], final.store_channel())
        self.independent(result)

    def test_present_invalid_final_manifest_cannot_fall_back_to_valid_legacy_inventory(self):
        self.fixture.assets['final-release-inventory.json'] = b'{}'
        self.fixture.metadata()
        result = self.check()
        self.assertEqual(result['website']['status'], 'verified')
        self.assertEqual(result['downloads']['status'], 'mismatch')
        self.assertNotIn(site.DOWNLOAD + self.report['version'] + '/release-inventory.json', self.requests)
        self.independent(result)
        del self.fixture.assets['final-release-inventory.json']
        self.fixture.metadata()
        self.requests.clear()
        result = self.check()
        self.assertEqual(result['downloads']['status'], 'verified')
        self.assertIn(site.DOWNLOAD + self.report['version'] + '/release-inventory.json', self.requests)
        self.independent(result)

    def test_changed_public_inventory_digest_or_asset_metadata_cannot_pass(self):
        inventory = copy.deepcopy(self.fixture.inventory)
        inventory['commit'] = 'f' * 40
        self.fixture.assets['release-inventory.json'] = site.encode(inventory)
        self.fixture.metadata()
        result = self.check()
        self.assertEqual(result['downloads']['status'], 'mismatch')
        self.independent(result)
        self.fixture.assets['release-inventory.json'] = site.encode(self.fixture.inventory)
        self.fixture.metadata()
        asset = next(a for a in self.fixture.release['assets'] if a['name'] == 'release-inventory.json')
        for key, value in [('digest', 'sha256:' + 'f' * 64), ('size', True),
                           ('browser_download_url', 'https://example.invalid/private')]:
            old = asset[key]
            asset[key] = value
            with self.subTest(key=key):
                self.assertEqual(self.check()['downloads']['status'], 'mismatch')
            asset[key] = old

    def test_changed_public_package_source_and_checksum_bytes_fail(self):
        for name in ['PeerBrush-0.2.0-Windows-x64.zip', 'PeerBrush-0.2.0-source.zip', 'SHA256SUMS']:
            original = self.fixture.assets[name]
            self.fixture.assets[name] += b'SYNTHETIC_CHANGED_BYTES'
            self.fixture.metadata()
            with self.subTest(name=name):
                result = self.check()
                self.assertEqual(result['downloads']['status'], 'mismatch')
                self.independent(result)
            self.fixture.assets[name] = original
        self.fixture.metadata()

    def test_no_older_platform_substitution_when_selected_download_disappears(self):
        del self.fixture.assets['PeerBrush-0.2.0-Windows-x64.zip']
        self.fixture.metadata()
        result = self.check()
        self.assertEqual(result['website']['status'], 'verified')
        self.assertEqual(result['downloads']['status'], 'unavailable')
        self.assertNotIn('windows', result['downloads']['verified_platforms'])
        self.independent(result)

    def test_unpublished_release_wrong_commit_or_unavailable_api_does_not_pass(self):
        for key, value in [('draft', True), ('prerelease', True), ('published_at', None)]:
            old = self.fixture.release[key]
            self.fixture.release[key] = value
            with self.subTest(key=key):
                self.assertEqual(self.check()['downloads']['status'], 'mismatch')
            self.fixture.release[key] = old
        self.overrides[site.API + 'commits/v0.2.0'] = site.encode(dict(sha='e' * 40))
        self.assertEqual(self.check()['downloads']['status'], 'mismatch')
        self.overrides = {site.API + 'releases/tags/v0.2.0': site.Unavailable('offline')}
        self.assertEqual(self.check()['downloads']['status'], 'unavailable')

    def test_invalid_expected_bytes_and_platform_types_fail_before_network(self):
        path = self.directory / 'public-site/downloads.json'
        original = path.read_bytes()
        path.write_bytes(original + b'changed')
        result = self.check()
        self.assertEqual(result['status'], 'unavailable')
        self.assertEqual(self.requests, [])
        for value in [None, [None], [dict(self.entry['platforms'][0], id=[])],
                      [dict(self.entry['platforms'][0], nativeSigning=['verified'])]]:
            data = json.loads(original)
            data['history'][0]['platforms'] = value
            raw = site.encode(data)
            path.write_bytes(raw)
            self.report['files']['downloads.json'] = site.sha(raw)
            self.save_report()
            with self.subTest(shape=type(value).__name__):
                self.assertEqual(self.check()['status'], 'unavailable')
                self.assertEqual(self.requests, [])

    def test_unprepared_or_old_unbound_report_cannot_grant_readiness(self):
        for key, value in [('site_update_prepared', False), ('production_readiness', True),
                           ('store_public_availability', 'verified'), ('files', {})]:
            old = self.report[key]
            self.report[key] = value
            self.save_report()
            with self.subTest(key=key):
                self.assertEqual(self.check()['status'], 'unavailable')
                self.assertEqual(self.requests, [])
            self.report[key] = old
        del self.report['release_inventory_sha256']
        self.save_report()
        self.assertEqual(self.check()['status'], 'unavailable')
        self.assertEqual(self.requests, [])

    def test_wrong_source_checkout_and_private_git_errors_are_bounded_before_gets(self):
        for response in [b'c' * 40, subprocess.CalledProcessError(128, ['git', 'PRIVATE_ARGUMENT'], stderr=b'PRIVATE_STDERR')]:
            options = {'side_effect': response} if isinstance(response, Exception) else {'return_value': response}
            with mock.patch.object(readback, 'git', **options):
                result = self.check()
            self.assertEqual(result['status'], 'unavailable')
            self.assertEqual(self.requests, [])
            self.assertNotIn('PRIVATE_', site.encode(result).decode())
            self.independent(result)


class Transport(unittest.TestCase):
    def test_official_website_allowlist_rejects_redirects_and_unexpected_queries(self):
        for value in [site.WEBSITE, site.WEBSITE + 'main.js?release=v0.2.0&layout=2']:
            self.assertTrue(site.public_url(value, website=True))
        for value in ['http://peerbrush.com/index.html', 'https://www.peerbrush.com/index.html',
                      'https://peerbrush.com:443/index.html', site.WEBSITE + 'main.js?private=value',
                      site.WEBSITE + 'main.js?release=v0.2.0&release=v0.2.0',
                      site.WEBSITE + 'index.html#private', 'https://peerbrush.com@evil.invalid/index.html',
                      site.WEBSITE + 'private-report.json', site.WEBSITE + 'main.js?release=v0.2.0&layout=01']:
            self.assertFalse(site.public_url(value, website=True), value)
        request = urllib.request.Request(site.WEBSITE + 'index.html')
        for target in [site.WEBSITE, 'https://www.peerbrush.com/', 'https://release-assets.githubusercontent.com/private']:
            with self.assertRaises(site.Unavailable):
                site.PublicRedirect(website=True).redirect_request(request, None, 302, 'Found', {}, target)

    def test_site_transport_sends_no_credentials_rejects_cached304_and_bounds_body(self):
        url = site.WEBSITE + 'main.js?release=v0.2.0&layout=2'
        class Response(io.BytesIO):
            status = 200
            headers = {'Content-Length': '4'}
            def geturl(self): return url
        with mock.patch.object(urllib.request, 'build_opener') as opener:
            opener.return_value.open.return_value = Response(b'data')
            self.assertEqual(site.public_get(url, 100, website=True), b'data')
            request = opener.return_value.open.call_args.args[0]
            headers = {k.lower(): v for k, v in request.header_items()}
            self.assertEqual(headers['cache-control'], 'no-cache')
            self.assertEqual(headers['pragma'], 'no-cache')
            self.assertNotIn('authorization', headers)
        for raw, status, size in [(b'data', 304, '4'), (b'data', 200, '3'), (b'data', 200, '5')]:
            response = Response(raw)
            response.status = status
            response.headers = {'Content-Length': size}
            with mock.patch.object(urllib.request, 'build_opener') as opener:
                opener.return_value.open.return_value = response
                with self.assertRaises(ValueError):
                    site.public_get(url, 100, website=True)

    def test_interrupted_http_chunk_does_not_expose_response_detail(self):
        url = site.WEBSITE + 'downloads.json'
        class Response(io.BytesIO):
            status = 200
            headers = {}
            def geturl(self): return url
            def read1(self, size): raise http.client.IncompleteRead(b'PRIVATE_PARTIAL_RESPONSE')
        with mock.patch.object(urllib.request, 'build_opener') as opener:
            opener.return_value.open.return_value = Response()
            with self.assertRaises(site.Unavailable) as caught:
                site.public_get(url, 100, website=True)
        self.assertNotIn('PRIVATE_PARTIAL_RESPONSE', str(caught.exception))

    def test_cli_invalid_input_prints_bounded_json_without_traceback_or_paths(self):
        output = io.StringIO()
        with mock.patch.object(readback, 'expected', side_effect=OSError('PRIVATE_PATH_OR_VALUE')), contextlib.redirect_stdout(output):
            self.assertEqual(readback.main(['--expected', '/private/input', '--source-root', '/private/source']), 2)
        result = json.loads(output.getvalue())
        self.assertEqual(result['status'], 'unavailable')
        self.assertNotIn('PRIVATE_', output.getvalue())
        self.assertNotIn('/private/', output.getvalue())


if __name__ == '__main__':
    unittest.main()
