"""Catalog readback regressions using authenticated fixtures and synthetic public responses."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from prepare_package_managers import definition, homebrew_cask
from prepare_site_release import API as RELEASE_API, DOWNLOAD, RELEASE, encode, sha
import readback_package_catalog as catalog
from test_nix_profile_upgrade import DEFINITION
from verify_release import MACOS_SIGNING_KIND


SOURCE = '1' * 40
HEAD = '2' * 40
ROOT = '3' * 40
DIRECTORY = '4' * 40
GROUP = '5' * 40
TEAM = 'ABCD123456'
REPO = 'Example/reviewed-catalog'
API = '/repos/' + REPO
RAW = '/' + REPO + '/' + HEAD + '/'
REF = API + '/git/ref/heads/main'
TAG = 'v0.30.1'
SOURCE_NAME = 'PeerBrush-0.30.1-source.zip'
SOURCE_URL = DOWNLOAD + TAG + '/' + SOURCE_NAME
SOURCE_BYTES = b'authenticated corresponding source fixture'
ARCHIVE_BYTES = b'authenticated install fixture'


def blob(path, data, mode='100644'):
    object_id = hashlib.sha1(f'blob {len(data)}\0'.encode() + data, usedforsecurity=False).hexdigest()
    return dict(path=path, size=len(data), sha=object_id, mode=mode, type='blob')


def directory(path, object_id):
    return dict(path=path, sha=object_id, mode='040000', type='tree')


class CatalogReadback(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name)
        self.responses = {
            (catalog.API, API): dict(default_branch='main', full_name=REPO, private=False),
            (catalog.API, REF): dict(ref='refs/heads/main', object=dict(type='commit', sha=HEAD)),
            (catalog.API, API + '/git/commits/' + HEAD): dict(sha=HEAD, tree=dict(sha=ROOT)),
        }
        self.requests = []
        self.downloads = {}
        self.release = {}
        self.release_commit = SOURCE

    def get(self, host, path, limit):
        self.requests.append((host, path))
        result = self.responses[host, path]
        data = result if isinstance(result, bytes) else encode(result)
        self.assertLessEqual(len(data), limit)
        return data

    def tree(self, object_id, rows, *, recursive=False):
        self.responses[catalog.API, API + '/git/trees/' + object_id + ('?recursive=1' if recursive else '')] = dict(
            sha=object_id, truncated=False, tree=rows)

    def release_assets(self, files):
        self.downloads = {DOWNLOAD + TAG + '/' + name: data for name, data in files.items()}
        self.release = dict(tag_name=TAG, html_url=RELEASE + TAG, draft=False, prerelease=False, assets=[
            dict(name=name, browser_download_url=DOWNLOAD + TAG + '/' + name, size=len(data),
                 digest='sha256:' + sha(data), state='uploaded')
            for name, data in files.items()
        ])

    def get_release(self, url, limit, *, api=False, destination=None):
        if api:
            self.assertIsNone(destination)
            if url == RELEASE_API + 'commits/' + TAG:
                return encode(dict(sha=self.release_commit))
            self.assertEqual(url, RELEASE_API + 'releases/tags/' + TAG)
            return encode(self.release)
        data = self.downloads[url]
        self.assertLessEqual(len(data), limit)
        with destination.open('xb') as file:
            file.write(data)

    def setup_definition(self, channel, *, dmg=False):
        asset = dict(sha256=sha(ARCHIVE_BYTES), executable_sha256='b' * 64,
                     source_sha256=sha(SOURCE_BYTES), binary_source_commit=SOURCE)
        companions = {}
        if channel == 'scoop':
            asset['file'] = 'PeerBrush-0.30.1-Windows-x64.zip'
            asset['url'] = DOWNLOAD + 'v0.30.1/' + asset['file']
            prefix = 'bucket/'
            name = 'peerbrush.json'
            data = definition(channel, None, dict(manifest=dict(platform='Windows', architecture='x64',
                              version='0.30.1'), prefix=asset['file'][:-4] + '/', record=asset), asset['url'])
            self.tree(ROOT, [directory('bucket', DIRECTORY)])
            self.tree(DIRECTORY, [blob(name, data)])
        else:
            asset.update(file='PeerBrush-0.30.1-macOS-arm64-apple-signed-r1.zip', publisher_team_id=TEAM,
                         native_publisher_signing=MACOS_SIGNING_KIND, signing_verification='metadata-and-hashes-only',
                         support_files={name: 'd' * 64 for name in ('LICENSE', 'THIRD_PARTY_NOTICES.md', 'licenses/example.txt')})
            if dmg:
                companion = asset['file']
                asset['file'] = companion.removesuffix('.zip') + '-dmg-signed-r1.dmg'
                companions = {companion: b'signed ZIP fixture',
                              asset['file'] + '.qualification.json': b'authenticated qualification fixture'}
                asset.update(signed_zip=dict(file=companion, sha256=sha(companions[companion])),
                             qualification_sha256=sha(companions[asset['file'] + '.qualification.json']))
            asset['url'] = DOWNLOAD + 'v0.30.1/' + asset['file']
            prefix = 'Casks/p/'
            name = 'peerbrush.rb'
            data = homebrew_cask('0.30.1', asset['sha256'], asset['url'], dmg=dmg)
            self.tree(ROOT, [directory('Casks', DIRECTORY)])
            self.tree(DIRECTORY, [directory('p', GROUP)])
            self.tree(GROUP, [blob(name, data)])
        self.release_assets({SOURCE_NAME: SOURCE_BYTES, asset['file']: ARCHIVE_BYTES, **companions})
        report = encode(dict(schema=1, version=TAG, commit=SOURCE, source_url=SOURCE_URL, published=False, release_ready=False,
                             channels={channel: dict(definition_prepared=True, asset=asset)}, files={name: sha(data)}))
        (self.path / name).write_bytes(data)
        (self.path / 'package-managers.json').write_bytes(report)
        self.responses[catalog.RAW, RAW + prefix + name] = data
        return sha(report)

    def verify(self, channel, digest, get=None, get_release=None):
        return catalog.check(channel, self.path, digest, tag=TAG, source_commit=SOURCE,
                             repository=REPO, catalog_commit=HEAD, team=TEAM if channel == 'homebrew' else None,
                             get=get or self.get, get_release=get_release or self.get_release)

    def test_scoop_and_homebrew_read_exact_files_at_the_pinned_default_head(self):
        for channel in ('scoop', 'homebrew'):
            with self.subTest(channel=channel):
                self.requests.clear()
                digest = self.setup_definition(channel)
                report = self.verify(channel, digest)
                self.assertEqual(report['catalog_publication'], 'exact-bytes-and-modes-verified')
                self.assertEqual(report['release_downloads_readback'], 'exact-bytes-verified')
                self.assertEqual(len(report['release_downloads']), 2)
                self.assertEqual(report['release_downloads'][SOURCE_NAME],
                                 dict(bytes=len(SOURCE_BYTES), sha256=sha(SOURCE_BYTES)))
                self.assertEqual(report['verified_files'], 1)
                self.assertEqual(self.requests.count((catalog.API, REF)), 2)

    def test_scoop_rejects_a_link_changed_bytes_and_a_moved_head(self):
        digest = self.setup_definition('scoop')
        rows = self.responses[catalog.API, API + '/git/trees/' + DIRECTORY]['tree']
        rows[0]['mode'] = '120000'
        with self.assertRaisesRegex(ValueError, 'file mode'):
            self.verify('scoop', digest)
        rows[0]['mode'] = '100644'
        key = catalog.RAW, RAW + 'bucket/peerbrush.json'
        data = self.responses[key]
        self.responses[key] = b'x' + data[1:]
        with self.assertRaisesRegex(ValueError, 'authenticated bytes'):
            self.verify('scoop', digest)
        self.responses[key] = data

        def moved(host, path, limit):
            if path == REF and (host, path) in self.requests:
                self.responses[host, path]['object']['sha'] = '0' * 40
            return self.get(host, path, limit)

        self.requests.clear()
        with self.assertRaisesRegex(ValueError, 'default branch differs'):
            self.verify('scoop', digest, moved)

    def test_nix_rechecks_flake_inventory_and_executable_bits(self):
        support = 'support/launcher'
        contents = {name: name.encode() for name in DEFINITION | {support}}
        inputs = dict(format='PeerBrushNixSourceInputsV1', version='0.30.1', source_commit=SOURCE,
                      source_url=SOURCE_URL, source_sha256=sha(SOURCE_BYTES), cargo_lock_sha256=sha(contents['Cargo.lock']))
        contents['source-inputs.json'] = encode(inputs)
        report = encode(dict(schema=1, format='PeerBrushNixPackagePreparationV1', platforms=['x86_64-linux'],
                             version='0.30.1', commit=SOURCE, release_ready=False, inputs=inputs,
                             files={name: sha(data) for name, data in contents.items()}, executable_files=[support]))
        (self.path / 'nix-package.json').write_bytes(report)
        rows = [directory('support', DIRECTORY)]
        for name, data in contents.items():
            rows.append(blob(name, data, '100755' if name == support else '100644'))
            self.responses[catalog.RAW, RAW + name] = data
        self.tree(ROOT, rows, recursive=True)
        self.release_assets({SOURCE_NAME: SOURCE_BYTES})
        verified = self.verify('nix', sha(report))
        self.assertEqual(verified['verified_files'], len(contents))
        self.assertEqual(set(verified['release_downloads']), {SOURCE_NAME})
        for extra in (blob('support/unreviewed', b'new'), blob('.gitattributes', b'* export-ignore\n'),
                      directory('support/empty', GROUP)):
            with self.subTest(path=extra['path']):
                rows.append(extra)
                with self.assertRaisesRegex(ValueError, 'different support tree or flake root'):
                    self.verify('nix', sha(report))
                rows.pop()
        next(row for row in rows if row['path'] == support)['mode'] = '100644'
        with self.assertRaisesRegex(ValueError, 'file mode'):
            self.verify('nix', sha(report))

    def test_catalog_bytes_alone_cannot_qualify_a_missing_or_replaced_release_download(self):
        digest = self.setup_definition('scoop')
        archive = self.release['assets'].pop()
        with self.assertRaisesRegex(ValueError, 'asset metadata'):
            self.verify('scoop', digest)
        self.release['assets'].append(archive)
        url = archive['browser_download_url']
        self.downloads[url] = b'x' * len(ARCHIVE_BYTES)
        with self.assertRaisesRegex(ValueError, 'authenticated bytes'):
            self.verify('scoop', digest)
        self.downloads[url] = ARCHIVE_BYTES

        def moved_tag(url, limit, **kwargs):
            data = self.get_release(url, limit, **kwargs)
            if kwargs.get('destination') is not None:
                self.release_commit = HEAD
            return data

        with self.assertRaisesRegex(ValueError, 'selected stable release'):
            self.verify('scoop', digest, get_release=moved_tag)

    def test_homebrew_dmg_readback_requires_exact_signed_companion_and_qualification(self):
        digest = self.setup_definition('homebrew', dmg=True)
        verified = self.verify('homebrew', digest)['release_downloads']
        for row in self.release['assets'][2:]:
            name, url = row['name'], row['browser_download_url']
            data = self.downloads[url]
            self.assertEqual(verified[name], dict(bytes=len(data), sha256=sha(data)))
            with self.subTest(name=name):
                self.downloads[url] = b'x' * len(data)
                with self.assertRaisesRegex(ValueError, 'authenticated bytes'):
                    self.verify('homebrew', digest)
                self.downloads[url] = data
                row['state'] = 'new'
                with self.assertRaisesRegex(ValueError, 'asset metadata'):
                    self.verify('homebrew', digest)
                row['state'] = 'uploaded'


if __name__ == '__main__':
    unittest.main()
