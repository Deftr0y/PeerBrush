"""Actual repository/source binding with mocked GPG; no cryptographic acceptance claim."""
import json
import datetime
import os
import shutil
import unittest
from unittest import mock

import attach_apt_signatures as attach
from package import digest
import test_prepare_apt_repository
import test_apt_repository_upgrade as acceptance
import test_verify_apt_signatures as crypto
import verify_apt_stage as stage_check


REPOSITORY_URI = 'https://packages.example.invalid/peerbrush/apt'


class Attachment(unittest.TestCase):
    def setUp(self):
        self.fixture = test_prepare_apt_repository.Repository(methodName='runTest')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.manifest = self.fixture.prepare()
        self.unsigned = self.fixture.output
        self.base = self.unsigned.parent
        self.output = self.base / 'signed'
        self.release = (self.unsigned / 'repository/dists/noble/Release').read_bytes()
        for name, data in [('InRelease', crypto.inline(self.release)), ('Release.gpg', b'SYNTHETIC_NOT_A_SIGNATURE'),
                           ('public-key.asc', crypto.PUBLIC), ('gpg', b'SYNTHETIC_NOT_AN_EXECUTABLE')]:
            (self.base / name).write_bytes(data)
        self.tool = self.base / 'gpg'
        if os.name == 'nt':
            self.tool = self.tool.rename(self.base / 'gpg.exe')
        self.tool.chmod(0o755)
        self.options = {key: value for key, value in self.fixture.options.items() if key not in ['date', 'valid_until']}
        self.options.update(unsigned_sha256=digest(self.unsigned / 'apt-repository.json'),
            inrelease=self.base / 'InRelease', inrelease_sha256=digest(self.base / 'InRelease'),
            detached=self.base / 'Release.gpg', detached_sha256=digest(self.base / 'Release.gpg'),
            public_key=self.base / 'public-key.asc', public_key_sha256=digest(self.base / 'public-key.asc'),
            fingerprint=crypto.PRIMARY, tool=self.tool, tool_sha256=digest(self.tool),
            repository_uri=REPOSITORY_URI)

    def attach(self, **changes):
        f = self.fixture.fixture
        with mock.patch.object(attach, 'current_time', return_value=crypto.NOW):
            return attach.attach(f.root, f.archive, f.package, self.unsigned, self.output,
                                 **dict(self.options, **changes))

    def previous(self, versions=('0.1.8', '0.1.9'), *, fingerprint=crypto.PRIMARY, keyring=crypto.KEYRING):
        directory = self.base / 'previous'
        date, valid_until = '2026-11-05T10:00:00Z', '2026-11-12T10:00:00Z'
        immutable = {}
        for version in versions:
            deb = f'SYNTHETIC_DEB_{version}'.encode()
            package = dict(file=f'peerbrush_{version}-1_amd64.deb', bytes=len(deb), sha256=attach.sha(deb))
            checked = dict(record=package, control=f'Package: peerbrush\nVersion: {version}-1\nArchitecture: amd64\n')
            path, active = attach.apt.indexes(checked, date, valid_until)
            immutable[path] = deb
            immutable.update({name: data for name, data in active.items() if '/by-hash/' in name})
        previous_files = {**immutable, **active, 'dists/noble/InRelease': crypto.inline(active['dists/noble/Release']),
            'dists/noble/Release.gpg': b'SYNTHETIC_NOT_A_SIGNATURE'}
        for name, data in previous_files.items():
            path = directory / 'repository' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        trust = {'bootstrap/' + attach.signatures.KEYRING_NAME: dict(bytes=len(keyring), sha256=attach.sha(keyring))}
        report = dict(self.manifest, format='PeerBrushSignedAptRepositoryV3', signing='openpgp-verified',
            date=date, valid_until=valid_until, version=versions[-1],
            signature_verification=dict(primary_fingerprint=fingerprint),
            bootstrap=dict(repository_uri=REPOSITORY_URI, files=trust),
            files={name: dict(bytes=len(data), sha256=attach.sha(data)) for name, data in previous_files.items()})
        receipt = directory / 'apt-repository.json'
        receipt.write_bytes(attach.encode(report))
        return (directory, digest(receipt)), immutable

    def stage(self):
        directory = self.base / 'stage'
        for name in ('repository', 'bootstrap'):
            shutil.copytree(self.output / name, directory / name)
        return directory

    def check_stage(self, directory, *, now=crypto.NOW, **changes):
        receipt = self.output / 'apt-repository.json'
        options = dict(digest=digest(receipt), repository_uri=REPOSITORY_URI, fingerprint=crypto.PRIMARY)
        with mock.patch.object(stage_check, 'current_time', return_value=now):
            return stage_check.check(directory, receipt, **dict(options, **changes))

    def test_actual_deb_source_tree_and_signatures_are_bound_without_publishing_or_mutating_input(self):
        before = {p.relative_to(self.unsigned): p.read_bytes() for p in self.unsigned.rglob('*') if p.is_file()}
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native):
            report = self.attach()
        self.assertEqual(before, {p.relative_to(self.unsigned): p.read_bytes() for p in self.unsigned.rglob('*') if p.is_file()})
        self.assertEqual(report['signing'], 'openpgp-verified')
        self.assertFalse(report['release_ready'])
        self.assertEqual(report['public_availability'], 'unverified')
        self.assertEqual(report['by_hash_retention'], 'current-candidate-only')
        self.assertEqual(report['repository_update_acceptance'], 'not-performed')
        self.assertEqual(report['signature_verification']['primary_fingerprint'], crypto.PRIMARY)
        repository = self.output / 'repository'
        for name, record in report['files'].items():
            data = (repository / name).read_bytes()
            self.assertEqual(record, dict(bytes=len(data), sha256=attach.sha(data)))
        self.assertEqual((repository / 'dists/noble/Release').read_bytes(), self.release)
        self.assertEqual((repository / 'dists/noble/InRelease').read_bytes(), (self.base / 'InRelease').read_bytes())
        self.assertEqual(json.loads((self.output / 'apt-repository.json').read_bytes()), report)
        self.assertNotIn(str(self.base), json.dumps(report))
        self.assertNotIn('Synthetic fixture', json.dumps(report))
        self.assertEqual(report['bootstrap']['repository_uri'], REPOSITORY_URI)
        keyring = 'peerbrush-archive-keyring.gpg'
        self.assertEqual(report['bootstrap']['files'].keys(), {'bootstrap/peerbrush.sources', f'bootstrap/{keyring}'})
        for name, record in report['bootstrap']['files'].items():
            data = (self.output / name).read_bytes()
            self.assertEqual(record, dict(bytes=len(data), sha256=attach.sha(data)))
            self.assertFalse((repository / name).exists())
        self.assertEqual((self.output / 'bootstrap' / keyring).read_bytes(), crypto.KEYRING)
        self.assertEqual((self.output / 'bootstrap/peerbrush.sources').read_bytes(),
            (f'Types: deb\nURIs: {REPOSITORY_URI}\nSuites: noble\nComponents: main\nArchitectures: amd64\n'
             f'Signed-By: /etc/apt/keyrings/{keyring} {crypto.PRIMARY}\nCheck-Date: yes\nCheck-Valid-Until: yes\n').encode('ascii'))

    def test_unsafe_or_ambiguous_bootstrap_uri_fails_before_native_access(self):
        for uri in [REPOSITORY_URI.replace('https:', 'http:'), REPOSITORY_URI + '?token=SYNTHETIC',
                    REPOSITORY_URI + '#fragment', REPOSITORY_URI + '\nTrusted: yes',
                    'https://user:password@packages.example.invalid', 'https://packages.example.invalid:8443',
                    'https://packages.example.invalid/../repo', 'https://packages.example.invalid/%2e%2e/repo',
                    'https://packages.example.invalid//repo', 'https://192.0.2.1/apt', 'https://-host.invalid/apt']:
            with self.subTest(uri=uri), mock.patch.object(attach.signatures, 'run') as run, self.assertRaises(ValueError):
                self.attach(repository_uri=uri)
            run.assert_not_called()
            self.assertFalse(self.output.exists())

    def test_expired_install_baseline_uses_the_retained_package_and_authenticated_install(self):
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native):
            report = self.attach()
        receipt = self.output / 'apt-repository.json'
        stage = self.stage()
        expired = datetime.datetime(2026, 12, 1, tzinfo=datetime.timezone.utc)
        with self.assertRaisesRegex(ValueError, 'not currently valid'):
            self.check_stage(stage, now=expired)
        relative, _, expected = acceptance.package_binding(report)
        package = stage / 'repository' / relative
        install = self.base / 'install.json'
        previous = dict(schema=1, format='PeerBrushAptInstallV1', product='PeerBrush',
            platform='Ubuntu 24.04 amd64', primary_fingerprint=crypto.PRIMARY, transport='local-file',
            receipt_sha256=digest(receipt), candidate=expected, missing_key='refused', initial_install='verified',
            purge='verified', update_owner='LinuxPackage', user_files='preserved', upgrade='not-performed')
        install.write_bytes(attach.encode(previous))
        selection = (receipt, digest(receipt), install, digest(install))
        with mock.patch('test_deb_installation.run', side_effect=['peerbrush', 'amd64', '0.2.0-1']):
            self.assertEqual(acceptance.archived_package(selection, stage, report, REPOSITORY_URI, crypto.PRIMARY),
                             (expected, package))
        with self.assertRaisesRegex(ValueError, 'does not retain the historical package'):
            acceptance.archived_package(selection, stage, dict(report, files={}), REPOSITORY_URI, crypto.PRIMARY)
        package.write_bytes(b'Changed archived package')
        with self.assertRaisesRegex(ValueError, 'filename or bytes differ'):
            acceptance.archived_package(selection, stage, report, REPOSITORY_URI, crypto.PRIMARY)
        install.write_bytes(attach.encode(dict(previous, receipt_sha256='0' * 64)))
        with self.assertRaisesRegex(ValueError, 'does not qualify this package'):
            acceptance.archived_package((*selection[:3], digest(install)), stage, report, REPOSITORY_URI, crypto.PRIMARY)

    def test_clean_install_cli_passes_the_signed_predecessor_without_an_upgrade(self):
        selected = ['current-stage', 'current-receipt', 'a' * 64]
        previous = ['previous-receipt', 'b' * 64]
        output = self.base / 'install.json'
        with mock.patch.object(acceptance, 'check') as check:
            acceptance.main([
                '--install-only', *selected, '--install-previous', *previous,
                '--repository-uri', REPOSITORY_URI, '--fingerprint', crypto.PRIMARY,
                '--output', str(output),
            ])
        check.assert_called_once_with(selected, None, output, repository_uri=REPOSITORY_URI,
                                      fingerprint=crypto.PRIMARY, older_previous=previous)

    def test_unusable_export_cannot_create_client_setup_or_receipt(self):
        def native(tool, arguments, work, label):
            if label == 'public-keys':
                return crypto.listing().replace(crypto.PRIMARY.encode(), b'C' * 40)
            return crypto.native(tool, arguments, work, label)
        with mock.patch.object(attach.signatures, 'run', side_effect=native), self.assertRaises(ValueError):
            self.attach()
        self.assertFalse(self.output.exists())

    def test_update_preserves_both_predecessor_generations_and_current_signed_indexes(self):
        previous, immutable = self.previous()
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native):
            report = self.attach(previous=previous)
        repository = self.output / 'repository'
        self.assertEqual(report['predecessor_manifest_sha256'], previous[1])
        self.assertEqual(report['by_hash_retention'], 'previous-candidate-chain')
        self.assertEqual(len(immutable), 6)
        for name, data in immutable.items():
            self.assertEqual((repository / name).read_bytes(), data)
            self.assertEqual(report['files'][name], dict(bytes=len(data), sha256=attach.sha(data)))
            self.assertEqual((previous[0] / 'repository' / name).read_bytes(), data)
        for name in ('Release', 'main/binary-amd64/Packages', 'main/binary-amd64/Packages.gz'):
            path = 'dists/noble/' + name
            self.assertEqual((repository / path).read_bytes(), (self.unsigned / 'repository' / path).read_bytes())
        self.assertEqual((repository / 'dists/noble/InRelease').read_bytes(), (self.base / 'InRelease').read_bytes())
        self.assertEqual(set(report['files']), {p.relative_to(repository).as_posix() for p in repository.rglob('*') if p.is_file()})
        stage = self.stage()
        predecessor = (previous[0] / 'apt-repository.json', previous[1])
        with self.assertRaises(ValueError):
            self.check_stage(stage)
        self.assertEqual(self.check_stage(stage, previous=predecessor)['predecessor_receipt_sha256'], previous[1])
        client_uri = 'https://packages.example.invalid/peerbrush/client'
        with mock.patch.object(stage_check.public, 'addresses', return_value=[mock.sentinel.endpoint]), \
                mock.patch.object(stage_check.public, 'compare') as compare:
            observed = self.check_stage(stage, previous=predecessor, public_bootstrap_uri=client_uri)
        self.assertEqual(observed['public_availability'], 'exact-bytes-verified')
        self.assertFalse(observed['release_ready'])
        targets = [(call.args[0].path, call.args[3]) for call in compare.call_args_list]
        inrelease = ('/peerbrush/apt', attach.apt.DIST + 'InRelease')
        self.assertEqual(targets[0], inrelease)
        self.assertEqual(targets[-1], inrelease)
        self.assertCountEqual(targets[:-1], [('/peerbrush/apt', name) for name in report['files']] +
            [('/peerbrush/client', name.removeprefix('bootstrap/')) for name in report['bootstrap']['files']])
        old_name = next(iter(immutable))
        (stage / 'repository' / old_name).unlink()
        del report['files'][old_name]
        # Even a newly pinned receipt cannot waive the independently selected history.
        (self.output / 'apt-repository.json').write_bytes(attach.encode(report))
        with self.assertRaisesRegex(ValueError, 'previously published immutable URLs'):
            self.check_stage(stage, previous=predecessor)

    def test_initial_stage_checks_exact_inventory_pins_expiry_and_bootstrap(self):
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native):
            self.attach()
        stage = self.stage()
        result = self.check_stage(stage)
        self.assertEqual(result['local_stage'], 'verified')
        self.assertEqual(result['source_archive_sha256'], self.manifest['package']['source_sha256'])
        self.assertEqual(result['package_file'], self.manifest['package']['file'])
        self.assertFalse(result['release_ready'])
        self.assertEqual(result['public_availability'], 'unverified')
        for options in [dict(digest='0' * 64), dict(fingerprint='C' * 40), dict(now=crypto.END),
                        dict(repository_uri='https://other.example.invalid')]:
            with self.subTest(options=options), self.assertRaises(ValueError):
                self.check_stage(stage, **options)
        extra = stage / 'apt-repository.json'
        extra.write_bytes(b'not a publication file')
        with self.assertRaises(ValueError):
            self.check_stage(stage)
        extra.unlink()
        keyring = stage / 'bootstrap' / attach.signatures.KEYRING_NAME
        keyring.write_bytes(b'x' * len(crypto.KEYRING))
        with self.assertRaises(ValueError):
            self.check_stage(stage)

    def test_changed_retained_bytes_cannot_create_an_update_receipt(self):
        previous, immutable = self.previous()
        name, data = next(iter(immutable.items()))
        (previous[0] / 'repository' / name).write_bytes(b'x' * len(data))
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native), self.assertRaises(ValueError):
            self.attach(previous=previous)
        self.assertFalse((self.output / 'apt-repository.json').exists())

    def test_update_rejects_changed_client_trust_before_writing_a_candidate(self):
        for changes in (dict(fingerprint='C' * 40), dict(keyring=b'different keyring')):
            with self.subTest(changes=changes):
                previous, _ = self.previous(**changes)
                with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native), \
                        self.assertRaisesRegex(ValueError, 'same approved client fingerprint and keyring'):
                    self.attach(previous=previous)
                self.assertFalse(self.output.exists())

    def test_stage_rejects_a_changed_keyring_even_when_its_bytes_match_the_new_receipt(self):
        previous, _ = self.previous()
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native):
            report = self.attach(previous=previous)
        stage = self.stage()
        keyring = 'bootstrap/' + attach.signatures.KEYRING_NAME
        changed = b'different keyring'
        (stage / keyring).write_bytes(changed)
        report['bootstrap']['files'][keyring] = dict(bytes=len(changed), sha256=attach.sha(changed))
        (self.output / 'apt-repository.json').write_bytes(attach.encode(report))
        with mock.patch.object(stage_check.public, 'check') as readback, \
                self.assertRaisesRegex(ValueError, 'same approved client fingerprint and keyring'):
            self.check_stage(stage, previous=(previous[0] / 'apt-repository.json', previous[1]),
                             public_bootstrap_uri='https://packages.example.invalid/peerbrush/client')
        readback.assert_not_called()

    def test_update_refuses_unpinned_history_and_replacing_a_versioned_deb(self):
        previous, _ = self.previous(versions=(self.options['version'],))
        with mock.patch.object(attach.signatures, 'run') as run:
            for selection in [(previous[0], '0' * 64), previous]:
                with self.subTest(selection=selection), self.assertRaises(ValueError):
                    self.attach(previous=selection)
                self.assertFalse(self.output.exists())
            run.assert_not_called()

    def test_modified_or_extra_unsigned_bytes_and_forged_readiness_fail_before_native_access(self):
        extra = self.unsigned / 'repository/unreviewed'
        extra.write_bytes(b'extra')
        with mock.patch.object(attach.signatures, 'run') as run, self.assertRaises(ValueError):
            self.attach()
        run.assert_not_called()
        self.assertFalse(self.output.exists())
        extra.unlink()
        selected = dict(self.manifest, release_ready=True)
        (self.unsigned / 'apt-repository.json').write_bytes(attach.encode(selected))
        with mock.patch.object(attach.signatures, 'run') as run, self.assertRaises(ValueError):
            self.attach(unsigned_sha256=digest(self.unsigned / 'apt-repository.json'))
        run.assert_not_called()
        self.assertFalse(self.output.exists())

    def test_metadata_changed_during_native_verification_is_rechecked_before_output(self):
        def native(tool, arguments, work, label):
            if label == 'detached':
                (self.unsigned / 'repository/dists/noble/Release').write_bytes(b'changed')
            return crypto.native(tool, arguments, work, label)
        with mock.patch.object(attach.signatures, 'run', side_effect=native), self.assertRaises(ValueError):
            self.attach()
        self.assertFalse(self.output.exists())

    def test_wrong_signature_hash_or_native_result_cannot_create_success(self):
        with mock.patch.object(attach.signatures, 'run') as run, self.assertRaises(ValueError):
            self.attach(inrelease_sha256='0' * 64)
        run.assert_not_called()
        def native(tool, arguments, work, label):
            return b'[GNUPG:] BADSIG invalid\n' if label == 'detached' else crypto.native(tool, arguments, work, label)
        with mock.patch.object(attach.signatures, 'run', side_effect=native), self.assertRaises(ValueError):
            self.attach()
        self.assertFalse(self.output.exists())

    def test_fresh_output_is_required_and_expiry_during_copy_cannot_create_receipt(self):
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native):
            self.attach()
        before = {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob('*') if p.is_file()}
        with self.assertRaises(ValueError):
            self.attach()
        self.assertEqual(before, {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob('*') if p.is_file()})
        f = self.fixture.fixture
        output = self.base / 'expired'
        with mock.patch.object(attach.signatures, 'run', side_effect=crypto.native), \
                mock.patch.object(attach, 'current_time', side_effect=[crypto.NOW, crypto.NOW, crypto.END]), \
                self.assertRaises(ValueError):
            attach.attach(f.root, f.archive, f.package, self.unsigned, output, **self.options)
        self.assertFalse((output / 'apt-repository.json').exists())


if __name__ == '__main__':
    unittest.main()
