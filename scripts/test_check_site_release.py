import json
import unittest

from check_site_release import check_release
from prepare_site_release import API, DOWNLOAD, RELEASE, WEBSITE, public_url


class SiteReleaseCheckTests(unittest.TestCase):
    def setUp(self):
        self.tag = 'v0.4.0'
        self.commit = 'a' * 40
        self.digest = 'b' * 64
        self.image = 'PeerBrush-0.4.0-macOS-arm64-apple-signed-r1-dmg-signed-r1.dmg'
        self.source = 'PeerBrush-0.4.0-source.zip'
        self.checksums = f'{self.digest}  {self.image}\n{self.digest}  {self.source}\n'.encode()
        entry = {
            'version': self.tag, 'commit': self.commit, 'label': 'PeerBrush 0.4.0 · Stable',
            'releaseUrl': RELEASE + self.tag, 'platforms': [
                {'url': self.url(self.image), 'sha256': self.digest, 'bytes': 10}
            ],
            'sourceUrl': self.url(self.source), 'checksumsUrl': self.url('SHA256SUMS'),
            'caution': '', 'notes': ['Native upgrade information.'], 'kind': 'stable',
            'publishedAt': '2026-11-19T11:22:33Z',
        }
        self.feed = dict(entry, schema=2, history=[dict(entry)])
        self.release = dict(tag_name=self.tag, draft=False, prerelease=False, html_url=entry['releaseUrl'],
                            published_at=entry['publishedAt'], assets=[
                                dict(name=name, browser_download_url=self.url(name), size=size, state='uploaded')
                                for name, size in ((self.image, 10), (self.source, 14), ('SHA256SUMS', len(self.checksums)))
                            ])

    def url(self, name):
        return f'{DOWNLOAD}{self.tag}/{name}'

    def get(self, url, limit, **kwargs):
        return {
            API + 'releases/latest': json.dumps(self.release).encode(),
            API + 'commits/' + self.tag: json.dumps({'sha': self.commit}).encode(),
            self.url('SHA256SUMS'): self.checksums,
        }[url]

    def check(self):
        return check_release(json.dumps(self.feed), self.get)

    def test_accepts_exact_reviewed_release(self):
        self.assertEqual(self.check(), self.tag)

    def test_blocks_stale_feed_before_deployment(self):
        self.feed['version'] = 'v0.30.1'
        with self.assertRaisesRegex(ValueError, 'updated to v0.4.0'):
            self.check()

    def test_blocks_different_history_source(self):
        self.feed['history'][0]['commit'] = 'c' * 40
        with self.assertRaisesRegex(ValueError, 'history do not match'):
            self.check()

    def test_blocks_different_asset_digest(self):
        self.release['assets'][0]['digest'] = 'sha256:' + 'e' * 64
        with self.assertRaisesRegex(ValueError, 'checksum or size differs'):
            self.check()

    def test_blocks_prerelease(self):
        self.release['prerelease'] = True
        with self.assertRaisesRegex(ValueError, 'published stable release'):
            self.check()

    def test_readback_admits_existing_release_cache_key(self):
        self.assertTrue(public_url(WEBSITE + 'downloads.json?release=v0.30.1&downloads=5', website=True))
        self.assertFalse(public_url(WEBSITE + 'downloads.json?release=v0.30.1&downloads=old', website=True))


if __name__ == '__main__':
    unittest.main()
