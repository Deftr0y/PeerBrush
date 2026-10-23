"""Synthetic published downloads and actual reviewed website layout; no network."""
import contextlib
import copy
import io
import json
import os
import pathlib
import shutil
import tempfile
import unittest
from unittest import mock
import urllib.request

import prepare_site_release as site
import test_package
import test_deb
from package import git
from verify_release import verify


class Published(unittest.TestCase):
    def setUp(self):
        fixture = self.fixture = test_package.Packages(methodName='runTest')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.root = fixture.root.resolve()
        self.formats = {'Windows': 'zip', 'macOS': 'zip', 'Linux': 'zip'}
        path = self.root / 'packaging/release-policy.json'
        path.parent.mkdir()
        path.write_bytes(site.encode(dict(schema=1, formats=self.formats)))
        fixture.policy['source'].append('packaging/release-policy.json')
        fixture.save_policy()
        fixture.commit()
        self.commit = git(self.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in site.PLATFORMS:
            fixture.build(platform)
        self.directory = self.root / 'verified'
        with contextlib.redirect_stdout(io.StringIO()):
            verify(fixture.output, self.commit, '0.2.0', self.directory, self.root)
        self.inventory = json.loads((self.directory / 'release-inventory.json').read_bytes())
        self.assets = {p.name: p.read_bytes() for p in self.directory.iterdir() if p.name != 'release-inventory.json'}
        self.release = dict(tag_name='v0.2.0', draft=False, prerelease=False,
                            published_at='2026-10-19T12:00:00Z', html_url=site.RELEASE + 'v0.2.0', assets=[])
        self.metadata()
        self.notes = dict(notes=['Reviewed public changes.'], caution='Native desktop acceptance remains unverified.')
        self.requests = []

    def metadata(self):
        self.release['assets'] = [dict(name=name, state='uploaded', size=len(data), digest='sha256:' + site.sha(data),
                                      browser_download_url=site.DOWNLOAD + 'v0.2.0/' + name)
                                  for name, data in self.assets.items()]

    def get(self, url, limit, *, api=False, destination=None):
        self.requests.append((url, api))
        if api:
            return site.encode(self.release if '/releases/' in url else dict(sha=self.commit))
        data = self.assets[url.rsplit('/', 1)[-1]]
        if destination is not None:
            with destination.open('xb') as stream:
                stream.write(data)
            return destination
        return data

    def published(self, maintainer=None):
        return site.published(self.root, self.inventory, 'v0.2.0', self.commit, self.notes, self.get, maintainer=maintainer)

    def test_public_exact_source_bytes_and_checksums_are_required(self):
        entry, excluded = self.published()
        self.assertEqual(excluded, [])
        self.assertEqual({p['id'] for p in entry['platforms']}, {'windows', 'macos', 'linux'})
        self.assertTrue(all(p['nativeSigning'] == 'unsigned' for p in entry['platforms']))
        self.assertTrue(all(p['packageReadiness'] == 'portable-archive-verified' for p in entry['platforms']))
        self.assertEqual(entry['commit'], self.commit)
        self.assertEqual(entry['sourceUrl'], site.DOWNLOAD + 'v0.2.0/PeerBrush-0.2.0-source.zip')
        self.assertFalse(any('store' in url.lower() for url, _ in self.requests))

    def test_missing_platform_is_excluded_and_never_gets_invented_url(self):
        name = 'PeerBrush-0.2.0-Windows-x64.zip'
        del self.assets[name]
        self.metadata()
        entry, excluded = self.published()
        self.assertEqual({p['id'] for p in entry['platforms']}, {'macos', 'linux'})
        self.assertIn(dict(file=name, reason='not-publicly-available'), excluded)

    def test_delayed_public_bytes_add_platform_without_changing_release_facts(self):
        name = 'PeerBrush-0.2.0-Windows-x64.zip'
        delayed = self.assets.pop(name)
        self.metadata()
        partial, _ = self.published()
        previous = json.loads((pathlib.Path(__file__).resolve().parents[1] / 'tests/fixtures/site-release-output.json').read_bytes())['files']
        data = json.loads(previous['downloads.json'])
        retained = dict(data['history'][0], version='v0.1.0', label='PeerBrush 0.1.0 · Stable')
        data.update({k: v for k, v in retained.items() if k not in {'kind', 'notes', 'publishedAt'}})
        data['history'] = [retained]
        previous['downloads.json'] = json.dumps(data)
        first = site.render(previous, partial)
        self.assets[name] = delayed
        self.metadata()
        complete, excluded = self.published()
        self.assertEqual(excluded, [])
        self.assertEqual(partial['releaseInventorySha256'], complete['releaseInventorySha256'])
        result = site.render({k: v.decode() for k, v in first.items()}, complete)
        final = json.loads(result['downloads.json'])
        self.assertEqual(final['history'], [complete, retained])
        self.assertEqual(len(final['platforms']), 3)
        self.assertEqual(site.render({k: v.decode() for k, v in result.items()}, complete), result)
        # Rechecking availability is mandatory, including previously selected rows.
        del self.assets['PeerBrush-0.2.0-Linux-x64.zip']
        self.metadata()
        unavailable, _ = self.published()
        with self.assertRaises(ValueError):
            site.render({k: v.decode() for k, v in result.items()}, unavailable)

    def test_draft_prerelease_unpublished_tag_commit_and_asset_state_fail(self):
        for key, value in [('draft', True), ('prerelease', True), ('published_at', None),
                           ('published_at', '2026-02-31T12:00:00Z'), ('tag_name', 'v0.2.1'), ('html_url', site.RELEASE + 'other')]:
            original = self.release[key]
            self.release[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.published()
            self.release[key] = original
        with self.assertRaises(ValueError):
            site.published(self.root, self.inventory, 'v0.2.0-dev.1', self.commit, self.notes, self.get)
        for key, value in [('browser_download_url', 'https://example.invalid/download'), ('state', 'new'), ('size', True)]:
            original = self.release['assets'][0][key]
            self.release['assets'][0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.published()
            self.release['assets'][0][key] = original
        with self.assertRaises(ValueError):
            site.published(self.root, self.inventory, 'v0.2.0', 'f' * 40, self.notes, self.get)

    def test_changed_package_source_checksum_and_metadata_digest_fail(self):
        for name in ['PeerBrush-0.2.0-source.zip', 'PeerBrush-0.2.0-Linux-x64.zip', 'SHA256SUMS']:
            original = self.assets[name]
            self.assets[name] += b'changed'
            self.metadata()
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.published()
            self.assets[name] = original
        self.metadata()
        self.release['assets'][0]['digest'] = 'sha256:' + 'f' * 64
        with self.assertRaises(ValueError):
            self.published()

    def test_malformed_public_api_types_dates_and_duplicate_assets_fail_cleanly(self):
        original = copy.deepcopy(self.release)
        for assets in [[None], [1], [dict(name=[])], [dict(name=True)], original['assets'] * 2]:
            self.release['assets'] = assets
            with self.subTest(assets=type(assets[0]).__name__), self.assertRaises(ValueError):
                self.published()
        self.release = original
        for value in [123, True, [], '2026-1-19T12:00:00Z', '2026-10-19T12:00:00+00:00', '2026-10-19T12:00:00.000Z']:
            self.release['published_at'] = value
            with self.subTest(date=type(value).__name__), self.assertRaises(ValueError):
                self.published()

    def test_installed_policy_blocks_portable_fallback_without_version_magic(self):
        # A source policy is part of the exact selected commit, not a cutoff in
        # the website script. This synthetic release uses installed-only formats.
        path = self.root / 'packaging/release-policy.json'
        path.write_bytes(site.encode(dict(schema=1, formats={'Windows': 'microsoft-store', 'macOS': 'dmg', 'Linux': 'deb'})))
        self.fixture.commit()
        commit = git(self.root, 'rev-parse', 'HEAD').decode().strip()
        self.commit = commit
        for platform in site.PLATFORMS:
            self.fixture.build(platform)
        for path in self.directory.iterdir():
            path.unlink()
        with contextlib.redirect_stdout(io.StringIO()):
            verify(self.fixture.output, commit, '0.2.0', self.directory, self.root)
        self.inventory = json.loads((self.directory / 'release-inventory.json').read_bytes())
        self.assets = {p.name: p.read_bytes() for p in self.directory.iterdir() if p.name != 'release-inventory.json'}
        self.metadata()
        entry, excluded = self.published()
        self.assertEqual(entry['platforms'], [])
        self.assertEqual(sum(e['reason'] == 'not-selected-by-committed-distribution-policy' for e in excluded), 3)
        self.assertIn(dict(platform='Windows', format='microsoft-store', reason='store-public-availability-unverified'), excluded)
        output = self.root.parent / (self.root.name + '-blocked')
        self.addCleanup(lambda: shutil.rmtree(output, ignore_errors=True))
        with mock.patch.object(site, 'site_sources', return_value={}):
            report = site.prepare(self.root, 'a' * 40, self.inventory, 'v0.2.0', commit, self.notes, output, self.get)
        self.assertFalse(report['site_update_prepared'])
        self.assertEqual(list(p.name for p in output.iterdir()), ['website-output.json'])

    def final_deb(self):
        deb = test_deb.deb
        source_root = pathlib.Path(__file__).resolve().parents[1]
        for name in [f'packaging/linux/{deb.APP_ID}.desktop', f'packaging/linux/{deb.APP_ID}.xml']:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((source_root / name).read_bytes())
            self.fixture.policy['source'].append(name)
        (self.root / 'packaging/release-policy.json').write_bytes(site.encode(dict(
            schema=1, formats={'Windows': 'microsoft-store', 'macOS': 'dmg', 'Linux': 'deb'})))
        self.fixture.save_policy()
        self.fixture.commit()
        self.commit = git(self.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in site.PLATFORMS:
            self.fixture.build(platform)
        for path in self.directory.iterdir():
            path.unlink()
        with contextlib.redirect_stdout(io.StringIO()):
            verify(self.fixture.output, self.commit, '0.2.0', self.directory, self.root)
        self.inventory = json.loads((self.directory / 'release-inventory.json').read_bytes())
        self.assets = {p.name: p.read_bytes() for p in self.directory.iterdir() if p.name != 'release-inventory.json'}
        self.linux = next(row for row in self.inventory['assets'] if row.get('platform') == 'Linux')
        stage = self.root / 'site-deb-stage'
        self.deb_report = deb.stage_payload(self.root, self.directory / self.linux['file'], stage, self.linux['sha256'])
        self.deb_entries = [('.', 0o755, None)] + [
            (path.relative_to(stage).as_posix(), 0o755 if path.is_dir() or path.relative_to(stage).as_posix() == 'usr/bin/peerbrush' else 0o644,
             None if path.is_dir() else path.read_bytes()) for path in sorted(stage.rglob('*'))]
        size = (sum(len(data) for _, _, data in self.deb_entries if data is not None) + 1023) // 1024
        self.deb_control = deb.control('0.2.0', 'libc6 (>= 2.34), ' + ', '.join(deb.RUNTIME), size, test_deb.MAINTAINER)
        self.deb_package = self.root / 'peerbrush_0.2.0-1_amd64.deb'
        test_deb.write_deb(self.deb_package, test_deb.tar_bytes(self.deb_entries),
                           test_deb.tar_bytes([('.', 0o755, None), ('control', 0o644, self.deb_control.encode())]))
        self.deb_record = dict(file=self.deb_package.name, platform='Linux', architecture='amd64',
                              binary_source_commit=self.commit, source_sha256=self.deb_report['source_sha256'])
        self.inventory['assets'].append(self.deb_record)
        self.refresh_deb()

    def refresh_deb(self):
        data, name = self.deb_package.read_bytes(), self.deb_package.name
        self.deb_record.update(bytes=len(data), sha256=site.sha(data))
        self.deb_report.update(file=name, sha256=site.sha(data))
        self.assets.update({name: data, name + '.json': site.encode(self.deb_report),
                            name + '.sha256': f'{site.sha(data)}  {name}\n'.encode()})
        self.assets['SHA256SUMS'] = ''.join(f'{r["sha256"]}  {r["file"]}\n' for r in self.inventory['assets']).encode()
        self.metadata()

    def test_exact_installer_names_and_candidate_receipts_remain_blocked(self):
        dmg = 'PeerBrush-0.2.0-macOS-arm64-apple-signed-r2-dmg-signed-r3.dmg'
        deb = 'peerbrush_0.2.0-1_amd64.deb'
        self.assertEqual(site.recognize(dmg, '0.2.0'), ('dmg', 'macOS'))
        self.assertEqual(site.recognize(deb, '0.2.0'), ('deb', 'Linux'))
        for bad in ['peerbrush_0.2.0_amd64.deb', 'peerbrush_0.2.0-2_amd64.deb', dmg.replace('signed-r3', 'candidate-r3'), 'arbitrary.dmg']:
            with self.subTest(name=bad), self.assertRaises(ValueError):
                site.recognize(bad, '0.2.0')
        for kind, name in [('dmg', dmg), ('deb', deb)]:
            record = dict(file=name, sha256='a' * 64, source_sha256='b' * 64)
            common = dict(schema=1, file=name, sha256='a' * 64, source_sha256='b' * 64,
                          version='0.2.0', commit=self.commit, release_ready=False)
            if kind == 'dmg':
                common.update(architecture='arm64', binary_source_commit=self.commit, image_signing='developer-id-notarized',
                              notary_status='Accepted', secure_timestamp=True, stapled=True, drag_to_applications_test='not-performed',
                              fresh_quarantined_download_test='not-performed')
            else:
                common.update(architecture='amd64', target='Ubuntu 24.04 amd64', update_owner='apt-dpkg',
                              update_repository='not-configured', signing='not-performed', installation_test='not-performed', default_app='user-choice-only')
            self.assertEqual(site.installed_report(kind, record, common, self.commit, '0.2.0'), 'native-final-installer-validation-not-configured')
            for key, value in [('release_ready', True), ('commit', 'f' * 40), ('sha256', 'f' * 64)]:
                with self.subTest(kind=kind, key=key), self.assertRaises(ValueError):
                    site.installed_report(kind, record, dict(common, **{key: value}), self.commit, '0.2.0')

    def test_public_final_deb_receipt_is_recognized_but_cannot_become_ready(self):
        name = 'peerbrush_0.2.0-1_amd64.deb'
        data = b'Synthetic DEB-like input; deliberately not a native package.'
        source = next(r for r in self.inventory['assets'] if r['file'].endswith('-source.zip'))
        record = dict(file=name, bytes=len(data), sha256=site.sha(data), platform='Linux', architecture='amd64',
                      source_sha256=source['sha256'], binary_source_commit=self.commit)
        self.inventory['assets'].append(record)
        report = dict(schema=1, file=name, sha256=record['sha256'], source_sha256=source['sha256'],
                      version='0.2.0', commit=self.commit, release_ready=False, architecture='amd64', target='Ubuntu 24.04 amd64',
                      update_owner='apt-dpkg', update_repository='not-configured', signing='not-performed',
                      installation_test='not-performed', default_app='user-choice-only')
        self.assets.update({name: data, name + '.json': site.encode(report),
                            name + '.sha256': f'{record["sha256"]}  {name}\n'.encode()})
        self.assets['SHA256SUMS'] = ''.join(f'{r["sha256"]}  {r["file"]}\n' for r in self.inventory['assets']).encode()
        self.metadata()
        entry, excluded = self.published()
        blocked = next(row for row in excluded if row.get('file') == name)
        self.assertEqual(blocked['reason'], 'final-deb-approved-maintainer-not-configured')
        self.assertFalse(blocked['content_verified'])
        self.assertFalse(blocked['ready'])
        self.assertFalse(any(p['url'].endswith('.deb') for p in entry['platforms']))
        with self.assertRaisesRegex(ValueError, 'required integration asset'):
            self.published(maintainer=test_deb.MAINTAINER)
        report['release_ready'] = True
        self.assets[name + '.json'] = site.encode(report)
        self.metadata()
        with self.assertRaises(ValueError):
            self.published()

    def test_actual_deb_content_is_verified_but_installed_policy_never_becomes_ready(self):
        self.final_deb()
        self.requests.clear()
        with mock.patch.object(test_deb.deb, 'run') as native:
            entry, excluded = self.published(maintainer=test_deb.MAINTAINER)
        native.assert_not_called()
        self.assertEqual(sum(not api and url.rsplit('/', 1)[-1] == self.linux['file']
                             for url, api in self.requests), 1)
        proof = next(row for row in excluded if row.get('file') == self.deb_package.name)
        self.assertTrue(proof['content_verified'])
        self.assertEqual(len(proof['content_checks']), 5)
        self.assertFalse(proof['ready'])
        self.assertFalse(proof['production_ready'])
        self.assertEqual(proof['installed_acceptance'], 'unverified')
        self.assertEqual(proof['update_repository'], 'not-configured')
        self.assertEqual(proof['public_availability'], 'unverified')
        self.assertEqual(entry['platforms'], [])
        # Selection and public bytes, not inventory iteration order, bind the companion ZIP.
        self.inventory['assets'].remove(self.deb_record)
        self.inventory['assets'].insert(0, self.deb_record)
        self.refresh_deb()
        self.requests.clear()
        repeated, excluded = self.published(maintainer=test_deb.MAINTAINER)
        self.assertEqual(sum(not api and url.rsplit('/', 1)[-1] == self.linux['file']
                             for url, api in self.requests), 1)
        self.assertEqual(repeated['platforms'], [])
        self.assertIn(proof, excluded)
        output = self.root.parent / (self.root.name + '-deb-content')
        self.addCleanup(lambda: shutil.rmtree(output, ignore_errors=True))
        with mock.patch.object(site, 'site_sources', return_value={}):
            report = site.prepare(self.root, 'a' * 40, self.inventory, 'v0.2.0', self.commit,
                                  self.notes, output, self.get, maintainer=test_deb.MAINTAINER)
        self.assertFalse(report['site_update_prepared'])
        self.assertEqual(list(path.name for path in output.iterdir()), ['website-output.json'])
        self.assertIn(proof, report['excluded'])
        self.assertNotIn(test_deb.MAINTAINER, json.dumps(report))

    def test_deb_missing_approved_identity_or_public_companion_remains_unverified(self):
        self.final_deb()
        _, excluded = self.published()
        proof = next(row for row in excluded if row.get('file') == self.deb_package.name)
        self.assertFalse(proof['content_verified'])
        self.assertEqual(proof['reason'], 'final-deb-approved-maintainer-not-configured')
        for name, reason in [(self.deb_package.name, 'not-publicly-available'),
                             (self.linux['file'], 'final-deb-portable-not-publicly-available')]:
            with self.subTest(missing=name):
                data = self.assets.pop(name)
                self.metadata()
                entry, excluded = self.published(maintainer=test_deb.MAINTAINER)
                self.assertEqual(entry['platforms'], [])
                self.assertIn(reason, [row['reason'] for row in excluded])
                self.assertFalse(any(row.get('content_verified') for row in excluded))
                self.assets[name] = data
                self.metadata()
        self.inventory['assets'].remove(self.linux)
        self.refresh_deb()
        _, excluded = self.published(maintainer=test_deb.MAINTAINER)
        self.assertIn('final-deb-portable-not-in-inventory', [row['reason'] for row in excluded])
        self.assertFalse(any(row.get('content_verified') for row in excluded))

    def test_deb_inspection_rejects_actual_payload_source_version_and_control_changes(self):
        self.final_deb()
        for maintainer in ['Other <other@example.invalid>', 'invalid', 'Synthetic\nInjected <mail@example.invalid>']:
            with self.subTest(maintainer=maintainer), self.assertRaises(ValueError):
                self.published(maintainer=maintainer)
        variants = [
            (self.deb_entries + [('usr/share/unexpected', 0o644, b'extra')], self.deb_control),
            ([(name, mode, b'wrong source' if name.endswith('-source.zip') else data)
              for name, mode, data in self.deb_entries], self.deb_control),
            (self.deb_entries, self.deb_control.replace('Version: 0.2.0-1', 'Version: 0.30.0-1')),
            (self.deb_entries, self.deb_control + 'Unexpected: injected\n'),
        ]
        for entries, control in variants:
            with self.subTest(control=control[-30:]):
                test_deb.write_deb(self.deb_package, test_deb.tar_bytes(entries),
                                   test_deb.tar_bytes([('.', 0o755, None), ('control', 0o644, control.encode())]))
                self.refresh_deb()
                with self.assertRaises(ValueError):
                    self.published(maintainer=test_deb.MAINTAINER)

    def test_deb_sidecar_claims_and_prerelease_never_grant_download_authority(self):
        self.final_deb()
        for key, value in [('release_ready', True), ('installation_test', 'passed'),
                           ('update_repository', 'https://example.invalid/approved')]:
            with self.subTest(claim=key):
                self.assets[self.deb_package.name + '.json'] = site.encode(dict(self.deb_report, **{key: value}))
                self.metadata()
                with self.assertRaises(ValueError):
                    self.published(maintainer=test_deb.MAINTAINER)
        self.refresh_deb()
        self.assets[self.deb_package.name + '.json'] = site.encode(dict(self.deb_report, ready=True, native_verified=True))
        self.metadata()
        entry, excluded = self.published(maintainer=test_deb.MAINTAINER)
        self.assertEqual(entry['platforms'], [])
        self.assertTrue(any(row.get('content_verified') for row in excluded))
        self.assertFalse(any(row.get('ready') for row in excluded))
        with self.assertRaises(ValueError):
            site.published(self.root, self.inventory, 'v0.2.0-dev.1', self.commit, self.notes,
                           self.get, maintainer=test_deb.MAINTAINER)

    def test_cli_forwards_the_explicit_approved_public_maintainer(self):
        notes = self.root / 'public-notes.json'
        notes.write_bytes(site.encode(self.notes))
        arguments = ['prepare_site_release.py', '--site-commit', 'a' * 40, '--tag', 'v0.2.0',
                     '--commit', self.commit, '--inventory', str(self.directory / 'release-inventory.json'),
                     '--notes', str(notes), '--output', str(self.root.parent / 'not-written'),
                     '--maintainer', test_deb.MAINTAINER]
        with mock.patch('sys.argv', arguments), mock.patch.object(site, 'prepare',
                return_value={'site_update_prepared': False}) as prepare, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(site.main(), 2)
        self.assertEqual(prepare.call_args.kwargs['maintainer'], test_deb.MAINTAINER)

    def test_preparation_writes_only_new_review_overlay_and_bound_hashes(self):
        previous = json.loads((pathlib.Path(__file__).resolve().parents[1] / 'tests/fixtures/site-release-output.json').read_bytes())['files']
        data = json.loads(previous['downloads.json'])
        retained = dict(data['history'][0], version='v0.1.0', label='PeerBrush 0.1.0 · Stable')
        data.update({key: value for key, value in retained.items() if key not in {'kind', 'notes', 'publishedAt'}})
        data['history'] = [retained]
        previous['downloads.json'] = json.dumps(data)
        output = self.root.parent / (self.root.name + '-website-output')
        self.addCleanup(lambda: shutil.rmtree(output, ignore_errors=True))
        with mock.patch.object(site, 'site_sources', return_value=previous):
            report = site.prepare(self.root, 'a' * 40, self.inventory, 'v0.2.0', self.commit, self.notes, output, self.get)
            self.assertTrue(report['site_update_prepared'])
            self.assertFalse(report['deployment_performed'])
            self.assertEqual(report['store_public_availability'], 'unverified')
            self.assertEqual(set(report['files']), set(site.SITE_FILES))
            if os.name == 'posix':
                self.assertEqual(output.stat().st_mode & 0o7777, 0o700)
            for name, digest in report['files'].items():
                self.assertEqual(site.sha((output / 'public-site' / name).read_bytes()), digest)
            with self.assertRaises(ValueError):
                site.prepare(self.root, 'a' * 40, self.inventory, 'v0.2.0', self.commit, self.notes, output, self.get)
            self.assets['SHA256SUMS'] = b'changed'
            with self.assertRaises(ValueError):
                site.prepare(self.root, 'a' * 40, self.inventory, 'v0.2.0', self.commit, self.notes, output.with_name(output.name + '-failed'), self.get)
            self.assertFalse(output.with_name(output.name + '-failed').exists())

    def final_dmg(self):
        import test_sign_macos
        from verify_release import inspect_archive
        self.final_deb()
        original = next(a for a in self.inventory['assets'] if a.get('platform') == 'macOS')
        signer = test_sign_macos.Signing(methodName='test_success_final_hashes_source_preservation_and_native_validation')
        signer.setUp()
        self.addCleanup(signer.doCleanups)
        signer.archive = self.directory / original['file']
        signer.kwargs.update(archive=signer.archive, commit=self.commit,
                             expected_sha256=site.sha(self.assets[original['file']]), source_root=self.root)
        signed = signer.sign()  # Every Apple operation remains mocked by the existing fixture.
        self.macos = inspect_archive(signed, self.commit, '0.2.0', self.root)['record']
        self.inventory['assets'] = [self.macos if a == original else a for a in self.inventory['assets']]
        self.assets.pop(original['file'])
        self.assets[signed.name] = signed.read_bytes()
        self.dmg_name = signed.stem + '-dmg-signed-r2.dmg'
        image = b'SYNTHETIC_UNVERIFIED_BYTES_NOT_A_DMG'
        self.dmg_record = dict(file=self.dmg_name, bytes=len(image), sha256=site.sha(image), platform='macOS',
                               architecture='arm64', binary_source_commit=self.commit, source_sha256=self.macos['source_sha256'])
        self.inventory['assets'].append(self.dmg_record)
        self.qualification = dict(schema=1, format='PeerBrushDmgQualificationV1', version='0.2.0', commit=self.commit,
            record=self.dmg_record, signed_zip=dict(file=signed.name, sha256=self.macos['sha256']),
            executable_sha256=self.macos['executable_sha256'], publisher_team_id=test_sign_macos.TEAM,
            candidate_verified=True, ready=False, production_ready=False, public_availability='unverified',
            content_checks=['exact-source-signed-zip-and-corresponding-source', 'exact-mounted-files-modes-and-Applications-link'],
            native_checks=['Developer-ID-publisher-and-secure-timestamps', 'app-hardened-runtime-and-empty-entitlements',
                           'image-and-app-stapled-tickets-and-Gatekeeper', 'hdiutil-integrity-and-read-only-mount'],
            limits={'drag-to-Applications-and-launch': 'not-performed', 'fresh-quarantined-download': 'not-performed',
                    'upgrade-settings-and-unsaved-work': 'not-performed', 'public-availability': 'not-assessed'},
            native_job_provenance='requires-trusted-native-job-at-consumption')
        self.assets[self.dmg_name] = image
        self.assets[self.dmg_name + '.qualification.json'] = site.encode(self.qualification)
        self.assets[self.dmg_name + '.sha256'] = f'{site.sha(image)}  {self.dmg_name}\n'.encode()
        self.assets['SHA256SUMS'] = ''.join(f"{a['sha256']}  {a['file']}\n" for a in self.inventory['assets']).encode()
        self.metadata()

    def test_public_typed_dmg_qualification_binds_actual_companion_bytes_without_native_or_installed_authority(self):
        self.final_dmg()
        self.requests.clear()
        with mock.patch('verify_macos_qualification.authenticate') as authenticate:
            entry, excluded = self.published(maintainer=test_deb.MAINTAINER)
        authenticate.assert_not_called()
        proof = next(row for row in excluded if row.get('file') == self.dmg_name)
        self.assertTrue(proof['asset_binding_verified'])
        self.assertEqual(proof['qualification_binding'], 'exact-schema-and-independent-digest-matched')
        self.assertFalse(proof['content_verified'])
        self.assertFalse(proof['ready'])
        self.assertFalse(proof['production_ready'])
        self.assertEqual(proof['native_producer_authentication'], 'not-verified-by-website')
        self.assertEqual(proof['installed_acceptance'], 'unverified')
        self.assertEqual(proof['public_availability'], 'unverified')
        self.assertEqual(entry['platforms'], [])
        requests = [url.rsplit('/', 1)[-1] for url, api in self.requests if not api]
        self.assertEqual(requests.count(self.macos['file']), 1)
        self.assertLess(requests.index(self.macos['file']), requests.index(self.dmg_name))
        self.assertIn(self.dmg_name + '.qualification.json', requests)
        self.assertNotIn(self.dmg_name + '.verification.json', requests)

    def test_public_dmg_canonicalizes_owned_temp_root_without_accepting_selected_links(self):
        from production_readiness import handoff_path

        self.final_dmg()
        with tempfile.TemporaryDirectory(prefix='peerbrush-public-alias-test-', dir=self.root.parent) as directory:
            real = pathlib.Path(directory).resolve() / 'actual'
            real.mkdir(mode=0o700)
            alias = real.parent / 'alias'
            try:
                alias.symlink_to(real, target_is_directory=True)
            except OSError:
                self.skipTest('Directory symlink creation is unavailable on this host')
            selected = real / 'selected-input'
            selected.write_bytes(b'unchanged')
            with self.assertRaisesRegex(ValueError, 'links or reparse points'):
                handoff_path(alias / selected.name)
            with mock.patch.object(site.tempfile, 'tempdir', str(alias)), \
                    mock.patch.object(self, 'get', wraps=self.get) as fetched, \
                    mock.patch('verify_macos_qualification.authenticate') as authenticate:
                entry, excluded = self.published(maintainer=test_deb.MAINTAINER)
            authenticate.assert_not_called()
            destinations = [call.kwargs['destination'] for call in fetched.call_args_list if 'destination' in call.kwargs]
            self.assertTrue(destinations)
            self.assertTrue(all(path.is_relative_to(real) for path in destinations))
            proof = next(row for row in excluded if row.get('file') == self.dmg_name)
            self.assertTrue(proof['asset_binding_verified'])
            self.assertFalse(proof['ready'])
            self.assertFalse(proof['production_ready'])
            self.assertEqual(proof['native_producer_authentication'], 'not-verified-by-website')
            self.assertEqual(proof['installed_acceptance'], 'unverified')
            self.assertEqual(entry['platforms'], [])
            self.assertEqual(selected.read_bytes(), b'unchanged')
            with self.assertRaisesRegex(ValueError, 'links or reparse points'):
                handoff_path(alias / selected.name)

    def test_public_typed_dmg_qualification_rejects_forged_malformed_and_overclaiming_reports(self):
        self.final_dmg()
        original = self.assets[self.dmg_name + '.qualification.json']
        changes = [dict(self.qualification, ready=True), dict(self.qualification, production_ready=True),
                   dict(self.qualification, private_path='PRIVATE_CANARY'), dict(self.qualification, commit='0' * 40),
                   dict(self.qualification, publisher_team_id='Z9Y8X7W6V5'),
                   dict(self.qualification, record=dict(self.dmg_record, sha256='0' * 64)),
                   dict(self.qualification, signed_zip=dict(self.qualification['signed_zip'], sha256='0' * 64)),
                   [], True]
        for report in changes:
            self.assets[self.dmg_name + '.qualification.json'] = site.encode(report)
            self.metadata()
            with self.subTest(report_type=type(report).__name__), self.assertRaises(ValueError):
                self.published(maintainer=test_deb.MAINTAINER)
        duplicate = original.replace(b'"schema": 1', b'"schema": 1, "schema": 1')
        for raw in [b'{', b'\x80', original + b' {}', duplicate, b'x' * (site.MAX_DMG_QUALIFICATION_BYTES + 1)]:
            self.assets[self.dmg_name + '.qualification.json'] = raw
            self.metadata()
            with self.subTest(length=len(raw)), self.assertRaises((ValueError, UnicodeError)):
                self.published(maintainer=test_deb.MAINTAINER)

    def test_public_typed_dmg_qualification_requires_available_companion_and_refuses_ambiguous_signer_receipts(self):
        self.final_dmg()
        companion = self.assets.pop(self.macos['file'])
        self.metadata()
        entry, excluded = self.published(maintainer=test_deb.MAINTAINER)
        proof = next(row for row in excluded if row.get('file') == self.dmg_name)
        self.assertEqual(proof['reason'], 'final-dmg-signed-zip-not-publicly-available')
        self.assertFalse(proof['asset_binding_verified'])
        self.assertEqual(entry['platforms'], [])
        self.assets[self.macos['file']] = companion
        self.inventory['assets'].remove(self.macos)
        self.assets['SHA256SUMS'] = ''.join(f"{a['sha256']}  {a['file']}\n" for a in self.inventory['assets']).encode()
        self.metadata()
        _, excluded = self.published(maintainer=test_deb.MAINTAINER)
        self.assertIn('final-dmg-signed-zip-not-in-inventory', [row['reason'] for row in excluded])
        self.inventory['assets'].append(self.macos)
        self.assets['SHA256SUMS'] = ''.join(f"{a['sha256']}  {a['file']}\n" for a in self.inventory['assets']).encode()
        self.assets[self.dmg_name + '.verification.json'] = b'{}'
        self.metadata()
        with self.assertRaisesRegex(ValueError, 'ambiguous qualification'):
            self.published(maintainer=test_deb.MAINTAINER)

class Rendering(unittest.TestCase):
    def setUp(self):
        fixture = json.loads((pathlib.Path(__file__).resolve().parents[1] / 'tests/fixtures/site-release-output.json').read_bytes())
        self.previous = fixture['files']
        self.history = json.loads(self.previous['downloads.json'])['history']
        self.entry = copy.deepcopy(self.history[0])
        self.entry.update(version='v0.30.0', label='PeerBrush 0.30.0 · Stable', commit='b' * 40,
                          releaseUrl=site.RELEASE + 'v0.30.0', sourceUrl=site.DOWNLOAD + 'v0.30.0/source.zip',
                          checksumsUrl=site.DOWNLOAD + 'v0.30.0/SHA256SUMS', releaseInventorySha256='d' * 64,
                          caution='Limited <checked> & honest.', notes=['New public notes.'])
        self.entry.pop('buildUrl', None)
        self.entry['platforms'] = [dict(id='windows', name='Windows', architecture='x64', url=site.DOWNLOAD + 'v0.30.0/final.zip',
                                        bytes=1234, sha256='c' * 64, nativeSigning='unverified', description='Portable ZIP · Trust unverified')]

    def test_actual_site_output_updates_all_baked_fields_cache_and_history(self):
        result = site.render(self.previous, self.entry)
        data = json.loads(result['downloads.json'])
        self.assertEqual(data['history'][1:], self.history)
        self.assertEqual(data['platforms'], self.entry['platforms'])
        for name, raw in result.items():
            text = raw.decode()
            if name != 'downloads.json':
                self.assertNotIn('v0.26.0', text, name)
                self.assertNotIn('PeerBrush 0.26.0', text, name)
        index = result['index.html'].decode()
        self.assertIn('Limited &lt;checked&gt; &amp; honest.', index)
        self.assertIn('c' * 64, index)
        self.assertNotIn('PeerBrush-0.26.0-Linux-x64.zip', index)
        self.assertIn('/blob/' + 'b' * 40 + '/docs/', result['releases.html'].decode())
        self.assertEqual(site.render({k: v.decode() for k, v in result.items()}, self.entry), result)

    def test_late_platforms_preserve_release_facts_and_cannot_demote_latest(self):
        first = site.render(self.previous, self.entry)
        added = dict(id='linux', name='Linux', architecture='x64', url=site.DOWNLOAD + 'v0.30.0/linux.zip',
                     bytes=4567, sha256='e' * 64, nativeSigning='unsigned', description='Verified portable fixture')
        amended = dict(self.entry, platforms=[*self.entry['platforms'], added])
        current = site.render({k: v.decode() for k, v in first.items()}, amended)
        self.assertEqual(json.loads(current['downloads.json'])['history'], [amended, *self.history])
        self.assertNotEqual(first['main.js'], current['main.js'])
        self.assertEqual(site.render({k: v.decode() for k, v in current.items()}, amended), current)
        newer = dict(self.entry, version='v0.31.0', label='PeerBrush 0.31.0 · Stable')
        promoted = site.render({k: v.decode() for k, v in first.items()}, newer)
        with self.assertRaisesRegex(ValueError, 'latest-release verification'):
            site.render({k: v.decode() for k, v in promoted.items()}, amended)

    def test_availability_amendments_reject_replacement_or_unpinned_metadata(self):
        first = site.render(self.previous, self.entry)
        previous = {k: v.decode() for k, v in first.items()}
        added = dict(self.entry['platforms'][0], id='linux')
        valid = dict(self.entry, platforms=[*self.entry['platforms'], added])
        for field, value in [('notes', ['changed']), ('publishedAt', '2026-10-20T00:00:00Z'),
                             ('commit', 'f' * 40), ('sourceUrl', 'changed'), ('checksumsUrl', 'changed'),
                             ('releaseInventorySha256', 'f' * 64)]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                site.render(previous, dict(valid, **{field: value}))
        for rows in [[added], [*valid['platforms'], added],
                     [dict(self.entry['platforms'][0], sha256='f' * 64), added]]:
            with self.subTest(rows=len(rows)), self.assertRaises(ValueError):
                site.render(previous, dict(valid, platforms=rows))
        unpinned = dict(self.entry)
        del unpinned['releaseInventorySha256']
        old = site.render(self.previous, unpinned)
        with self.assertRaises(ValueError):
            site.render({k: v.decode() for k, v in old.items()}, dict(unpinned, platforms=valid['platforms']))

    def test_history_replacement_downgrade_duplicate_and_unknown_layout_fail(self):
        for field, value in [('notes', ['changed']), ('commit', 'c' * 40), ('platforms', [])]:
            changed = dict(self.history[0], **{field: value})
            with self.subTest(field=field), self.assertRaises(ValueError):
                site.render(self.previous, changed)
        with self.assertRaises(ValueError):
            site.render(self.previous, dict(self.entry, version='v0.1.0'))
        changed = dict(self.previous, **{'index.html': '<html>changed output</html>'})
        with self.assertRaisesRegex(ValueError, 'layout changed'):
            site.render(changed, self.entry)
        for name, before in [('main.js', 'release=v0.26.0'), ('index.html', 'release=v0.26.0'),
                             ('releases.html', 'data-doc="getting-started.md"'), ('release-page.js', 'release=v0.26.0'),
                             ('releases.js', 'release=v0.26.0')]:
            changed = dict(self.previous)
            changed[name] = changed[name].replace(before, 'changed-layout', 1)
            with self.subTest(file=name), self.assertRaisesRegex(ValueError, 'layout changed'):
                site.render(changed, self.entry)
        changed = dict(self.previous)
        data = json.loads(changed['downloads.json'])
        for key, value in [('version', 'missing'), ('platforms', [])]:
            changed['downloads.json'] = json.dumps(dict(data, **{key: value}))
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'latest fields'):
                site.render(changed, dict(self.entry, version='v0.1.0'))


class NetworkAndOutput(unittest.TestCase):
    def test_urls_and_redirects_remain_public_and_bounded(self):
        self.assertTrue(site.public_url(site.DOWNLOAD + 'v1.0.0/file.zip'))
        for url in ['http://github.com/Deftr0y/PeerBrush/releases/download/file', site.DOWNLOAD + 'file?token=private',
                    'https://github.com@evil.invalid/Deftr0y/PeerBrush/releases/download/file', 'https://localhost/file',
                    'https://release-assets.githubusercontent.com:443/file']:
            self.assertFalse(site.public_url(url))
        redirect = site.PublicRedirect()
        request = urllib.request.Request(site.DOWNLOAD + 'v1.0.0/file.zip')
        for target in ['http://release-assets.githubusercontent.com/file', 'https://evil.invalid/file', 'https://localhost/file']:
            with self.assertRaises(ValueError):
                redirect.redirect_request(request, None, 302, 'Found', {}, target)
        self.assertIsNotNone(redirect.redirect_request(request, None, 302, 'Found', {}, 'https://release-assets.githubusercontent.com/file?signature=opaque'))
        class Response(io.BytesIO):
            status = 200
            headers = {'Content-Length': '4'}
            def geturl(self): return request.full_url
        for raw, limit in [(b'abc', 4), (b'abcd', 3)]:
            with mock.patch.object(urllib.request, 'build_opener') as opener:
                opener.return_value.open.return_value = Response(raw)
                with self.assertRaises(ValueError):
                    site.public_get(request.full_url, limit)

    def test_streaming_has_chunk_size_total_deadline_and_no_large_memory_return(self):
        url = site.DOWNLOAD + 'v1.0.0/file.zip'
        class Response(io.BytesIO):
            status = 200
            headers = {}
            sizes = []
            def geturl(self): return url
            def read(self, size):
                raise AssertionError('Use one underlying buffered read, never fill a whole chunk')
            def read1(self, size):
                self.sizes.append(size)
                return super().read1(size)
        with tempfile.TemporaryDirectory() as temporary:
            destination = pathlib.Path(temporary).resolve() / 'large.zip'
            response = Response(b'x' * 200000)
            with mock.patch.object(urllib.request, 'build_opener') as opener:
                opener.return_value.open.return_value = response
                self.assertEqual(site.public_get(url, 300000, destination=destination), destination)
                self.assertEqual(destination.stat().st_size, 200000)
                self.assertLessEqual(max(response.sizes), 64 * 1024)
                self.assertEqual(opener.return_value.open.call_args.kwargs['timeout'], 15)
            with mock.patch.object(urllib.request, 'build_opener') as opener, mock.patch.object(site.time, 'monotonic', side_effect=[0, 0, 121]):
                opener.return_value.open.return_value = Response(b'slow')
                with self.assertRaisesRegex(ValueError, 'total deadline'):
                    site.public_get(url, 100)
            with mock.patch.object(urllib.request, 'build_opener') as opener:
                opener.return_value.open.return_value = Response(b'oversized')
                with self.assertRaisesRegex(ValueError, 'size budget'):
                    site.public_get(url, 3)
            with self.assertRaisesRegex(ValueError, 'temporary file'):
                site.public_get(url, 3 * 1024 * 1024)

    def test_external_output_is_immutable_and_rejects_other_worktrees(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = pathlib.Path(temporary).resolve()
            root = base / 'repo'
            root.mkdir()
            git(root, 'init', '-q')
            (root / 'source').write_bytes(b'fixture')
            git(root, 'add', '.')
            git(root, '-c', 'user.name=Website Tests', '-c', 'user.email=website-tests@example.invalid', 'commit', '-qm', 'Synthetic fixture')
            other = base / 'other'
            git(root, 'worktree', 'add', '--detach', str(other))
            self.assertEqual(site.output_path(root, base / 'candidate'), base / 'candidate')
            for path in [root / 'candidate', other / 'candidate', base, base / 'candidate/../other/candidate']:
                with self.assertRaises(ValueError):
                    site.output_path(root, path)


if __name__ == '__main__':
    unittest.main()
