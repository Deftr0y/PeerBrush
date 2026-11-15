"""Repository candidate regression source; no apt, signing, network or install."""
import datetime
import email.utils
import gzip
import json
import pathlib
import tempfile
import unittest
from unittest import mock

import prepare_apt_repository as apt
from package import digest, git
import test_apt_repository_upgrade as acceptance
import test_deb


class Repository(unittest.TestCase):
    def setUp(self):
        self.fixture = test_deb.DebianPackage(methodName='runTest')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.fixture.final_package()
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-apt-test-')
        self.addCleanup(self.temporary.cleanup)
        self.output = pathlib.Path(self.temporary.name).resolve() / 'candidate'
        f = self.fixture
        self.options = dict(commit=git(f.root, 'rev-parse', 'HEAD').decode().strip(), version='0.2.0',
            portable_sha256=digest(f.archive), package_sha256=digest(f.package), maintainer=test_deb.MAINTAINER,
            date='2026-11-07T10:00:00Z', valid_until='2026-11-14T10:00:00Z')

    def prepare(self, **changes):
        f = self.fixture
        return apt.prepare(f.root, f.archive, f.package, self.output, **dict(self.options, **changes))

    def test_actual_deb_source_and_control_define_indexes_and_exact_copied_package(self):
        f = self.fixture
        original_package = f.package.read_bytes()
        original_portable = f.archive.read_bytes()
        with mock.patch.object(apt.deb, 'run') as native:
            report = self.prepare()
        native.assert_not_called()
        self.assertEqual(f.package.read_bytes(), original_package)
        self.assertEqual(f.archive.read_bytes(), original_portable)
        repository = self.output / 'repository'
        pool = 'pool/main/p/peerbrush/' + f.package.name
        self.assertEqual((repository / pool).read_bytes(), original_package)
        packages = (repository / 'dists/noble/main/binary-amd64/Packages').read_bytes()
        self.assertTrue(packages.startswith(f.final_control.encode('utf-8')))
        self.assertIn(('Filename: ' + pool + '\n').encode(), packages)
        self.assertIn(('Size: ' + str(len(original_package)) + '\n').encode(), packages)
        self.assertIn(('SHA256: ' + digest(f.package) + '\n\n').encode(), packages)
        compressed = (repository / 'dists/noble/main/binary-amd64/Packages.gz').read_bytes()
        self.assertEqual(gzip.decompress(compressed), packages)
        self.assertEqual(compressed[4:8], b'\0' * 4)
        self.assertEqual(compressed[9], 255)
        release = (repository / 'dists/noble/Release').read_text(encoding='utf-8')
        self.assertIn('Architectures: amd64\nComponents: main\nAcquire-By-Hash: yes\n', release)
        fields = dict(line.split(': ', 1) for line in release.splitlines() if ': ' in line)
        self.assertEqual(email.utils.parsedate_to_datetime(fields['Date']),
                         datetime.datetime(2026, 11, 7, 10, tzinfo=datetime.timezone.utc))
        self.assertEqual(email.utils.parsedate_to_datetime(fields['Valid-Until']),
                         datetime.datetime(2026, 11, 14, 10, tzinfo=datetime.timezone.utc))
        for name, data in [('Packages', packages), ('Packages.gz', compressed)]:
            checksum = apt.sha(data)
            self.assertIn(f' {checksum} {len(data)} main/binary-amd64/{name}\n', release)
            self.assertEqual((repository / ('dists/noble/main/binary-amd64/by-hash/SHA256/' + checksum)).read_bytes(), data)
        self.assertEqual(set(report['files']), {p.relative_to(repository).as_posix() for p in repository.rglob('*') if p.is_file()})
        for name, record in report['files'].items():
            actual = (repository / name).read_bytes()
            self.assertEqual(record, dict(bytes=len(actual), sha256=apt.sha(actual)))
        self.assertEqual(json.loads((self.output / 'apt-repository.json').read_bytes()), report)
        self.assertFalse(report['release_ready'])
        self.assertEqual(report['signing'], 'not-performed')
        self.assertEqual(report['public_availability'], 'unverified')
        self.assertFalse((repository / 'dists/noble/InRelease').exists())
        self.assertFalse((repository / 'dists/noble/Release.gpg').exists())
        self.assertFalse(any('path' in key for key in report))

    def test_stable_version_dates_and_validity_are_required_before_writes(self):
        variants = [dict(version='0.2.0-dev.1'), dict(version='0.2.0+local'),
            dict(date='2026-11-07'), dict(date='2026-02-30T10:00:00Z'),
            dict(date='2026-11-07T10:00:00+00:00'),
            dict(valid_until=self.options['date']), dict(valid_until='2026-11-06T10:00:00Z'),
            dict(valid_until='2026-12-01T10:00:00Z')]
        for changes in variants:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.prepare(**changes)
            self.assertFalse(self.output.exists())

    def test_changed_or_unapproved_package_source_control_and_digest_cannot_enter_repository(self):
        for changes in [dict(package_sha256='0' * 64), dict(portable_sha256='0' * 64),
                        dict(commit='0' * 40), dict(maintainer='Other <other@example.invalid>')]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.prepare(**changes)
            self.assertFalse(self.output.exists())
        f = self.fixture
        test_deb.write_deb(f.package, test_deb.tar_bytes(f.final_data),
            test_deb.tar_bytes([('.', 0o755, None), ('control', 0o644, f.final_control.encode()),
                                ('postinst', 0o755, b'#!/bin/sh\nexit 0\n')]))
        with self.assertRaises(ValueError):
            self.prepare(package_sha256=digest(f.package))
        self.assertFalse(self.output.exists())

    def test_repeated_index_generation_is_deterministic_and_output_is_immutable(self):
        report = self.prepare()
        before = {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob('*') if p.is_file()}
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertEqual(before, {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob('*') if p.is_file()})
        checked = self.fixture.inspect()
        _, files = apt.indexes(checked, self.options['date'], self.options['valid_until'])
        for name, data in files.items():
            self.assertEqual(apt.sha(data), report['files'][name]['sha256'])

    def test_copy_mutation_keeps_failed_candidate_without_success_receipt(self):
        with mock.patch.object(apt, 'copy_package', side_effect=ValueError('Changed selected DEB')):
            with self.assertRaises(ValueError):
                self.prepare()
        self.assertFalse((self.output / 'apt-repository.json').exists())
        with self.assertRaises(ValueError):
            self.prepare()

    def test_bounded_copy_rejects_growth_truncation_or_same_size_change(self):
        source = pathlib.Path(self.temporary.name).resolve() / 'selected.deb'
        original = b'original bytes'
        record = dict(bytes=len(original), sha256=apt.sha(original))
        for i, content in enumerate([original + b'extra', original[:-1], original.replace(b'bytes', b'wrong')]):
            source.write_bytes(content)
            with self.subTest(content=content), self.assertRaises(ValueError):
                apt.copy_package(source, source.with_name(f'failed-{i}.deb'), record)


class AcceptanceSelection(unittest.TestCase):
    def test_desktop_input_binds_the_package_entry_and_native_fixture_bytes(self):
        archive = pathlib.Path('selected.deb')
        project = pathlib.Path('Artwork 作品.psb')
        expected = {'executable_sha256': 'a' * 64}
        workspace = mock.Mock(projects={project: ({'layers': []}, (32, 24, 16, b'\x12\x34'))},
                              files={project: 'b' * 64})
        entry = b'[Desktop Entry]\nExec=/usr/bin/peerbrush -- %F\n'
        path = acceptance.DESKTOP.relative_to('/').as_posix()
        with mock.patch.object(acceptance, 'selected_package') as selected, \
                mock.patch.object(acceptance, 'deb_archives', return_value={'data.tar.xz': b'decoded tar'}), \
                mock.patch.object(acceptance, 'tar_entries', return_value={path: (0o644, entry)}) as entries:
            facts = acceptance.desktop_input(archive, expected, workspace)
            selected.assert_called_once_with(archive, expected)
            workspace.preserve.assert_called_once_with()
            self.assertEqual(facts['desktop'], entry.decode())
            self.assertEqual(facts['projects'][project.name]['native_hex'], '1234')
            entries.return_value = {path: (0o644, entry + b'Exec=/usr/bin/other -- %F\n')}
            with self.assertRaisesRegex(ValueError, 'guarded desktop Exec route'):
                acceptance.desktop_input(archive, expected, workspace)

    def test_install_only_cannot_silently_replace_upgrade_acceptance(self):
        common = ['--repository-uri', 'https://packages.example.invalid', '--fingerprint', 'A' * 40,
                  '--output', 'receipt.json']
        candidate = ['stage', 'candidate.json', 'a' * 64]
        successor = ['new-stage', 'new-candidate.json', 'b' * 64]
        archived = [*candidate[1:], 'old-install.json', 'c' * 64]
        predecessor = ['refreshed.json', 'd' * 64]
        bootstrap = ['--public-bootstrap-uri', 'https://packages.example.invalid/client']
        with mock.patch.object(acceptance, 'check') as check:
            acceptance.main([*common, '--install-only', *candidate])
            self.assertEqual(check.call_args.args[:2], (candidate, None))
            self.assertNotIn('desktop', check.call_args.kwargs)
            acceptance.main([*common, '--install-only', *candidate, '--desktop', *bootstrap])
            self.assertIs(check.call_args.kwargs['desktop'], True)
            self.assertEqual(check.call_args.kwargs['public_bootstrap_uri'], bootstrap[1])
            acceptance.main([*common, '--older', *candidate, '--newer', *successor])
            self.assertEqual(check.call_args.args[:2], (candidate, successor))
            acceptance.main([*common, '--archived-baseline', *archived, '--newer', *successor,
                             '--newer-previous', *predecessor, *bootstrap])
            self.assertEqual(check.call_args.args[:2], (None, successor))
            self.assertEqual(check.call_args.kwargs['archived'], archived)
            self.assertEqual(check.call_args.kwargs['newer_previous'], predecessor)
            self.assertEqual(check.call_args.kwargs['public_bootstrap_uri'], bootstrap[1])
            check.reset_mock()
            for flags in (['--older', *candidate],
                          ['--archived-baseline', *archived],
                          ['--archived-baseline', *archived, '--newer', *successor, '--older-previous', *predecessor],
                          ['--older', *candidate, '--newer', *successor, '--newer-previous', *predecessor],
                          ['--older', *candidate, '--newer', *successor, *bootstrap],
                          ['--install-only', *candidate, '--newer', *successor],
                          ['--install-only', *candidate, '--older-previous', *successor[1:]]):
                with self.subTest(flags=flags), mock.patch('sys.stderr'), self.assertRaises(SystemExit):
                    acceptance.main([*common, *flags])
            check.assert_not_called()

    def test_native_apt_artifact_requires_exact_size_and_digest(self):
        artifact = self.output.with_name('download.deb')
        artifact.write_bytes(b'package')
        record = dict(bytes=7, sha256=digest(artifact))
        acceptance.require_apt_bytes(artifact, record)
        for altered in (b'packag', b'Package'):
            artifact.write_bytes(altered)
            with self.subTest(altered=altered), self.assertRaisesRegex(ValueError, 'differ from the signed candidate'):
                acceptance.require_apt_bytes(artifact, record)


if __name__ == '__main__':
    unittest.main()
