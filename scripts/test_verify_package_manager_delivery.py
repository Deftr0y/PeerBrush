"""Offline regressions for the package delivery evidence join."""
import copy
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from prepare_apt_repository import timestamp
from prepare_site_release import encode, sha
from verify_package_manager_acceptance import DESKTOP, DESKTOP_CONNECT, FORMATS, SHELL_DESKTOP
import verify_package_manager_delivery as delivery


VERSION = '0.30.1'
COMMIT = 'a' * 40
SOURCE = 'b' * 64
SOURCE_FILE = f'PeerBrush-{VERSION}-source.zip'
ARTIFACT_FILES = {
    'scoop': f'PeerBrush-{VERSION}-Windows-x64.zip',
    'homebrew': f'PeerBrush-{VERSION}-macOS-arm64-apple-signed-r1-dmg-signed-r1.dmg',
    'apt': f'peerbrush_{VERSION}-1_amd64.deb',
    'nix': SOURCE_FILE,
}
BREW_DMG = ARTIFACT_FILES['homebrew']
BREW_COMPANIONS = (BREW_DMG.rsplit('-dmg-signed-r', 1)[0] + '.zip', BREW_DMG + '.qualification.json')
NIX_OUTPUT = dict(flake_nar_sha256='sha256-' + 'A' * 43 + '=',
                  nar_sha256='sha256-' + 'B' * 43 + '=', closure_sha256='c' * 64)
APT_HTTPS = dict(repository_uri='https://packages.example.invalid/apt',
                 public_bootstrap_uri='https://packages.example.invalid/client', inrelease_sha256='a' * 64)


class PackageDelivery(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.reports = {}
        for channel, marker in zip(FORMATS, 'cdef'):
            identity = dict(version=VERSION, source_commit=COMMIT)
            definition = marker * 64
            artifact = SOURCE if channel == 'nix' else '5' * 64
            artifact_file = ARTIFACT_FILES[channel]
            scope = dict(identity, schema=1, format='PeerBrushPackageManagerNativeReadbackV1', channel=channel,
                         artifact_file=artifact_file, artifact_sha256=artifact, definition_receipt_sha256=definition,
                         source_archive_sha256=SOURCE, desktop_startup_documents=DESKTOP[channel],
                         public_availability='unverified', release_ready=False)
            if channel == 'apt':
                scope.update(publisher='F' * 40, repository_predecessor_sha256='1' * 64)
                public = dict(identity, format='PeerBrushAptStageReadbackV1', candidate_receipt_sha256=definition,
                              predecessor_receipt_sha256='1' * 64, primary_fingerprint=scope['publisher'],
                              local_stage='verified', public_availability='exact-bytes-verified',
                              date='2026-10-01T00:00:00Z', valid_until='2026-10-10T00:00:00Z', package_sha256=artifact,
                              package_file=artifact_file, source_archive_sha256=SOURCE, release_ready=False,
                              **APT_HTTPS)
            else:
                files = {SOURCE_FILE: dict(bytes=7, sha256=SOURCE)}
                if channel != 'nix':
                    files[artifact_file] = dict(bytes=11, sha256=artifact)
                public = dict(identity, schema=1, format='PeerBrushPackageCatalogReadbackV1', channel=channel,
                              catalog_commit='0' * 40, definition_receipt_sha256=definition, release_downloads=files,
                              catalog_publication='exact-bytes-and-modes-verified',
                              release_downloads_readback='exact-bytes-verified')
                if channel == 'homebrew':
                    public['publisher_team_id'] = scope['publisher'] = 'ABCD123456'
                    files.update({name: dict(bytes=11, sha256='6' * 64) for name in BREW_COMPANIONS})
                if channel == 'scoop':
                    scope.update(shell_desktop=SHELL_DESKTOP, desktop_connect=DESKTOP_CONNECT)
            install = dict(scope, native_lifecycle='install', native_upgrade='not-performed', native_receipt_sha256='3' * 64)
            upgrade = dict(scope, native_lifecycle='upgrade', native_upgrade='verified', native_receipt_sha256='4' * 64,
                           previous_definition_receipt_sha256='2' * 64)
            if channel == 'apt':
                install['apt_https'] = dict(APT_HTTPS)
                upgrade['apt_https'] = dict(APT_HTTPS)
            if channel == 'nix':
                install['nix_output'] = dict(NIX_OUTPUT)
                upgrade['nix_output'] = dict(NIX_OUTPUT)
                upgrade['native_rollback'] = 'verified'
            self.reports[channel] = [public, install, upgrade]

    def entries(self):
        entries = []
        for channel, reports in self.reports.items():
            row = [channel]
            for suffix, report in zip(('public', 'install', 'upgrade'), reports):
                data = encode(report)
                path = self.root / f'{channel}-{suffix}.json'
                path.write_bytes(data)
                row.extend((path, sha(data)))
            entries.append(row)
        return entries

    def check(self, now='2026-10-05T00:00:00Z'):
        with mock.patch.object(delivery, 'current_time', return_value=timestamp(now)):
            return delivery.check(self.entries(), version=VERSION, source_commit=COMMIT)

    def test_reconciles_all_channels_and_keeps_historical_apt_baseline_distinct(self):
        report = self.check()
        self.assertEqual(report['source_archive_sha256'], SOURCE)
        self.assertEqual(set(report['channels']), set(FORMATS))
        self.assertFalse(report['release_ready'])
        self.assertEqual({name: row['artifact_file'] for name, row in report['channels'].items()}, ARTIFACT_FILES)
        self.assertEqual(report['channels']['nix']['artifact_sha256'], SOURCE)
        self.assertEqual(report['channels']['nix']['nix_output'], NIX_OUTPUT)

    def test_binds_each_native_apt_https_lifecycle_to_the_public_generation(self):
        _, install, upgrade = self.reports['apt']
        self.assertEqual(self.check()['channels']['apt']['apt_https'], dict(install=APT_HTTPS, upgrade=APT_HTTPS))
        for report in (install, upgrade):
            with self.subTest(lifecycle=report['native_lifecycle']):
                del report['apt_https']
                with self.assertRaisesRegex(ValueError, 'same signed generation'):
                    self.check()
                report['apt_https'] = dict(APT_HTTPS)
        for key, value in (('inrelease_sha256', 'b' * 64), ('public_bootstrap_uri', 'https://example.invalid/client')):
            with self.subTest(key=key):
                upgrade['apt_https'] = dict(APT_HTTPS, **{key: value})
                with self.assertRaisesRegex(ValueError, 'same signed generation'):
                    self.check()

    def test_initial_apt_repository_can_upgrade_a_standalone_deb(self):
        public, install, upgrade = self.reports['apt']
        public['predecessor_receipt_sha256'] = None
        install['repository_predecessor_sha256'] = upgrade['repository_predecessor_sha256'] = None
        del upgrade['previous_definition_receipt_sha256']
        upgrade['standalone_deb_sha256'] = '8' * 64
        self.assertEqual(self.check()['channels']['apt']['standalone_deb_sha256'], '8' * 64)
        upgrade['standalone_deb_sha256'] = public['package_sha256']
        with self.assertRaisesRegex(ValueError, 'must differ'):
            self.check()

    def test_rejects_missing_channels_or_unpinned_evidence(self):
        entries = self.entries()
        with self.assertRaisesRegex(ValueError, 'each of the four channels'):
            delivery.check(entries[:-1], version=VERSION, source_commit=COMMIT)
        entries[0][2] = '8' * 64
        with self.assertRaisesRegex(ValueError, 'independent digest'):
            delivery.check(entries, version=VERSION, source_commit=COMMIT)

    def test_rejects_incompatible_public_install_and_upgrade_claims(self):
        original = copy.deepcopy(self.reports)
        for channel, kind, field, value, reason in [
            ('scoop', 0, 'release_downloads_readback', 'unverified', 'Public package catalog'),
            ('homebrew', 1, 'publisher', 'EEEE123456', 'different publishers'),
            ('apt', 0, 'predecessor_receipt_sha256', '9' * 64, 'Public APT repository'),
            ('apt', 0, 'package_file', 'wrong.deb', 'Public APT repository'),
            ('apt', 0, 'date', None, 'canonical UTC repository dates'),
            ('apt', 2, 'source_commit', '9' * 40, 'Native upgrade'),
            ('nix', 2, 'native_rollback', 'not-performed', 'Nix rollback'),
            ('nix', 1, 'nix_output', None, 'same flake, output and closure'),
            ('nix', 2, 'nix_output', dict(NIX_OUTPUT, closure_sha256='9' * 64), 'same flake, output and closure'),
            ('scoop', 1, 'native_lifecycle', 'upgrade', 'Native clean install'),
            ('scoop', 1, 'artifact_file', 'wrong.zip', 'same release download filename'),
            ('scoop', 1, 'shell_desktop', 'not-performed', 'Scoop clean install desktop evidence'),
            ('scoop', 2, 'desktop_connect', 'not-performed', 'Scoop upgrade desktop evidence'),
            ('homebrew', 1, 'artifact_sha256', '9' * 64, 'same authenticated artifact'),
            ('homebrew', 2, 'source_archive_sha256', '9' * 64, 'same authenticated artifact'),
            ('apt', 0, 'source_archive_sha256', '9' * 64, 'same authenticated artifact'),
            ('nix', 1, 'source_archive_sha256', None, 'same authenticated artifact'),
        ]:
            with self.subTest(channel=channel, field=field):
                self.reports = copy.deepcopy(original)
                self.reports[channel][kind][field] = value
                with self.assertRaisesRegex(ValueError, reason):
                    self.check()

        self.reports = copy.deepcopy(original)
        self.reports['scoop'][0]['release_downloads'][SOURCE_FILE]['sha256'] = '9' * 64
        for report in self.reports['scoop'][1:]:
            report['source_archive_sha256'] = '9' * 64
        with self.assertRaisesRegex(ValueError, 'different corresponding source'):
            self.check()

    def test_rejects_renamed_downloads_and_disagreeing_source_sizes(self):
        downloads = self.reports['homebrew'][0]['release_downloads']
        package = downloads.pop(ARTIFACT_FILES['homebrew'])
        downloads['renamed.dmg'] = package
        with self.assertRaisesRegex(ValueError, 'exact release download filenames'):
            self.check()
        downloads[ARTIFACT_FILES['homebrew']] = downloads.pop('renamed.dmg')
        downloads[SOURCE_FILE]['bytes'] += 1
        with self.assertRaisesRegex(ValueError, 'source archive size'):
            self.check()

    def test_rejects_homebrew_dmg_readback_without_its_companions(self):
        downloads = self.reports['homebrew'][0]['release_downloads']
        for name in BREW_COMPANIONS:
            with self.subTest(name=name):
                saved = downloads.pop(name)
                with self.assertRaisesRegex(ValueError, 'exact release download filenames'):
                    self.check()
                downloads[name] = saved

    def test_rejects_matching_nix_readbacks_without_a_valid_output_hash(self):
        for report in self.reports['nix'][1:]:
            report['nix_output']['nar_sha256'] = 'invalid'
        with self.assertRaisesRegex(ValueError, 'same flake, output and closure'):
            self.check()

    def test_apt_requires_currently_valid_signed_metadata(self):
        self.check('2026-10-01T00:00:00Z')
        for now in ('2026-09-30T23:59:59Z', '2026-10-10T00:00:00Z'):
            with self.subTest(now=now), self.assertRaisesRegex(ValueError, 'outside its signed validity window'):
                self.check(now)


if __name__ == '__main__':
    unittest.main()
