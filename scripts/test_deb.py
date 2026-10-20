"""DEB staging checks with synthetic committed source, never real installation."""
import hashlib
import io
import json
import lzma
import pathlib
import os
import sys
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock
import xml.etree.ElementTree as ET
import zipfile

import package_deb as deb
import test_deb_installation as installation
import test_package
from package import digest, git


ROOT = pathlib.Path(__file__).resolve().parents[1]
MAINTAINER = 'Synthetic CI <builds@example.invalid>'


def tar_bytes(entries):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode='w', format=tarfile.USTAR_FORMAT) as archive:
        for name, mode, content in entries:
            entry = tarfile.TarInfo('./' if name == '.' else './' + name)
            entry.uid = entry.gid = 0
            entry.mode = mode
            entry.type = tarfile.DIRTYPE if content is None else tarfile.REGTYPE
            entry.size = 0 if content is None else len(content)
            archive.addfile(entry, None if content is None else io.BytesIO(content))
    return data.getvalue()


def ar_bytes(members):
    result = bytearray(b'!<arch>\n')
    for name, data in members:
        header = f'{name + "/":<16}{0:<12}{0:<6}{0:<6}{"100644":<8}{len(data):<10}`\n'.encode('ascii')
        if len(header) != 60:
            raise ValueError('Invalid synthetic ar member')
        result.extend(header + data + (b'\n' if len(data) % 2 else b''))
    return bytes(result)


def write_deb(path, data, control):
    path.write_bytes(ar_bytes([('debian-binary', b'2.0\n'),
                               ('control.tar.xz', lzma.compress(control)),
                               ('data.tar.xz', lzma.compress(data))]))


class DebianPackage(unittest.TestCase):
    def setUp(self):
        self.fixture = test_package.Packages()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        # macOS exposes its temporary directory through the /var symlink.
        # Select the real fixture root; production output still rejects links.
        self.root = self.fixture.root.resolve()
        self.fixture.root = self.root
        self.fixture.output = self.fixture.output.resolve()
        for filename in [f'packaging/linux/{deb.APP_ID}.desktop', f'packaging/linux/{deb.APP_ID}.xml']:
            path = self.root / filename
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((ROOT / filename).read_bytes())
            self.fixture.policy['source'].append(filename)
        self.fixture.save_policy()
        self.fixture.commit()
        self.archive = self.fixture.build('Linux')

    def stage(self, stage, **overrides):
        return deb.stage_payload(self.root, self.archive, stage, overrides.get('expected_sha256', digest(self.archive)))

    def final_package(self):
        stage = self.root / 'final-deb-stage'
        self.stage(stage)
        self.final_data = [('.', 0o755, None)]
        for path in sorted(stage.rglob('*')):
            name = path.relative_to(stage).as_posix()
            self.final_data.append((name, 0o755 if path.is_dir() or name == 'usr/bin/peerbrush' else 0o644,
                                    None if path.is_dir() else path.read_bytes()))
        size = (sum(len(content) for _, _, content in self.final_data if content is not None) + 1023) // 1024
        self.final_control = deb.control('0.2.0', 'libc6 (>= 2.34), ' + ', '.join(deb.RUNTIME), size, MAINTAINER)
        self.package = self.root / 'peerbrush_0.2.0-1_amd64.deb'
        write_deb(self.package, tar_bytes(self.final_data), tar_bytes([('.', 0o755, None), ('control', 0o644, self.final_control.encode())]))
        return self.package

    def inspect(self, **overrides):
        selected = dict(commit=git(self.root, 'rev-parse', 'HEAD').decode().strip(), version='0.2.0',
                        portable_sha256=digest(self.archive), package_sha256=digest(self.package), maintainer=MAINTAINER)
        return deb.inspect_final(self.root, self.archive, self.package, **dict(selected, **overrides))

    def test_final_deb_inspection_binds_actual_payload_source_control_and_selected_hashes(self):
        self.final_package()
        original = self.package.read_bytes()
        with mock.patch.object(deb, 'run') as native:
            inspected = self.inspect()
            native.assert_not_called()
        self.assertEqual(self.package.read_bytes(), original)
        self.assertEqual(inspected['record'], dict(file=self.package.name, bytes=len(original), sha256=digest(self.package),
                         platform='Linux', architecture='amd64', binary_source_commit=git(self.root, 'rev-parse', 'HEAD').decode().strip(),
                         source_sha256=digest(self.root / 'final-deb-stage/usr/share/doc/peerbrush/PeerBrush-0.2.0-source.zip')))
        self.assertEqual(inspected['executable_sha256'], digest(self.root / 'final-deb-stage/usr/bin/peerbrush'))
        self.assertEqual(inspected['portable_sha256'], digest(self.archive))
        for changes in [dict(package_sha256='0' * 64), dict(portable_sha256='0' * 64),
                        dict(commit='0' * 40), dict(version='0.30.0'), dict(maintainer='Other <other@example.invalid>')]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.inspect(**changes)
        (self.package.with_name(self.package.name + '.json')).write_text('{"release_ready":true}')
        self.assertEqual(self.inspect(), inspected, 'Builder sidecars must never grant content qualification')

    def test_final_deb_rejects_missing_extra_changed_and_wrong_mode_payload(self):
        self.final_package()
        controls = tar_bytes([('.', 0o755, None), ('control', 0o644, self.final_control.encode())])
        variants = [self.final_data + [('usr/share/unexpected', 0o644, b'extra')],
                    [entry for entry in self.final_data if entry[0] != 'usr/bin/peerbrush'],
                    [(name, 0o644 if name == 'usr/bin/peerbrush' else mode, content) for name, mode, content in self.final_data],
                    [(name, mode, b'changed' if name.endswith('.desktop') else content) for name, mode, content in self.final_data],
                    [(name, mode, b'wrong source' if name.endswith('-source.zip') else content) for name, mode, content in self.final_data]]
        for index, entries in enumerate(variants):
            with self.subTest(mutation=index):
                write_deb(self.package, tar_bytes(entries), controls)
                with self.assertRaisesRegex(ValueError, 'bytes or modes'):
                    self.inspect()

    def test_final_deb_rejects_scripts_unapproved_control_and_missing_runtime_dependencies(self):
        self.final_package()
        payload = tar_bytes(self.final_data)
        variants = [self.final_control.replace('Architecture: amd64', 'Architecture: arm64'),
                    self.final_control.replace('Installed-Size: ', 'Installed-Size: 999'),
                    self.final_control.replace('Version: 0.2.0-1', 'Version: 0.26.0-1'),
                    self.final_control.replace(', libegl1', ''),
                    self.final_control.replace('libc6 (>= 2.34)', 'libc6 [amd64]'),
                    self.final_control + 'Conflicts: another-editor\n',
                    self.final_control + 'Depends: libc6\n']
        for text in variants:
            with self.subTest(control=text):
                write_deb(self.package, payload, tar_bytes([('.', 0o755, None), ('control', 0o644, text.encode())]))
                with self.assertRaises(ValueError):
                    self.inspect()
        for name in ['postinst', 'conffiles', 'triggers']:
            with self.subTest(extra=name):
                write_deb(self.package, payload, tar_bytes([('.', 0o755, None), ('control', 0o644, self.final_control.encode()),
                                                           (name, 0o755, b'#!/bin/sh\nexit 0\n')]))
                with self.assertRaisesRegex(ValueError, 'only the exact control'):
                    self.inspect()

    def test_final_deb_rechecks_both_selected_inputs_and_requires_matching_checkout(self):
        self.final_package()
        for changed in [self.package, self.archive]:
            calls = 0
            def changing_digest(path):
                nonlocal calls
                if path == changed:
                    calls += 1
                    if calls > 1:
                        return '0' * 64
                return digest(path)
            with self.subTest(changed=changed.name), mock.patch.object(deb, 'digest', side_effect=changing_digest), \
                    self.assertRaisesRegex(ValueError, 'changed during inspection'):
                self.inspect()
        selected = git(self.root, 'rev-parse', 'HEAD').decode().strip()
        (self.root / 'docs/usage.md').write_text('Different committed source')
        self.fixture.commit()
        with self.assertRaisesRegex(ValueError, 'selected commit'):
            self.inspect(commit=selected)
        with self.assertRaises(ValueError):
            self.inspect()

    def test_exact_staging_preserves_binary_source_notices_and_consent(self):
        original = self.archive.read_bytes()
        stage = self.root / 'deb-stage'
        receipt = self.stage(stage)
        self.assertEqual(self.archive.read_bytes(), original)
        self.assertFalse(receipt['release_ready'])
        self.assertEqual(receipt['commit'], git(self.root, 'rev-parse', 'HEAD').decode().strip())
        self.assertEqual(digest(stage / 'usr/bin/peerbrush'), receipt['executable_sha256'])
        docs = stage / 'usr/share/doc/peerbrush'
        self.assertEqual(digest(docs / 'PeerBrush-0.2.0-source.zip'), receipt['source_sha256'])
        self.assertTrue((docs / 'LICENSE').is_file())
        self.assertTrue((docs / 'THIRD_PARTY_NOTICES.md').is_file())
        self.assertTrue((docs / 'licenses/rust-runtime/COPYRIGHT-library.html').is_file())
        desktop = (stage / f'usr/share/applications/{deb.APP_ID}.desktop').read_text()
        self.assertIn('Exec=/usr/bin/peerbrush -- %F\n', desktop)
        self.assertIn('MimeType=image/vnd.adobe.photoshop;\n', desktop)
        self.assertIn('Icon=/usr/share/peerbrush/peerbrush-logo.png\n', desktop)
        self.assertTrue((stage / 'usr/share/peerbrush/peerbrush-logo.png').is_file())
        mime = ET.parse(stage / f'usr/share/mime/packages/{deb.APP_ID}.xml')
        self.assertEqual(mime.find('.//{*}glob').attrib['pattern'], '*.psb')
        self.assertFalse((stage / 'DEBIAN').exists(), 'staging must not introduce install/remove scripts')
        self.assertFalse(any(p.name == 'mimeapps.list' for p in stage.rglob('*')))
        self.assertFalse(any('home' in p.relative_to(stage).parts for p in stage.rglob('*')))
        if sys.platform != 'win32':
            self.assertEqual((stage / 'usr/bin/peerbrush').stat().st_mode & 0o777, 0o755)

    def test_tampering_wrong_source_and_wrong_platform_are_rejected(self):
        windows = self.fixture.build('Windows')
        with self.assertRaisesRegex(ValueError, 'Linux x64'):
            deb.stage_payload(self.root, windows, self.root / 'wrong-platform', digest(windows))
        with zipfile.ZipFile(self.archive) as archive:
            files = {entry.filename: archive.read(entry) for entry in archive.infolist()}
        executable = next(name for name in files if name.endswith('/peerbrush'))
        files[executable] += b'tampered'
        with zipfile.ZipFile(self.archive, 'w') as archive:
            for name, data in files.items():
                archive.writestr(name, data)
        with self.assertRaises(ValueError):
            self.stage(self.root / 'tampered')
        self.assertFalse((self.root / 'tampered').exists())

    def test_existing_staging_is_never_overwritten(self):
        stage = self.root / 'existing'
        stage.mkdir()
        (stage / 'canary').write_bytes(b'keep')
        with self.assertRaisesRegex(ValueError, 'fresh directory'):
            self.stage(stage)
        self.assertEqual((stage / 'canary').read_bytes(), b'keep')

    def test_uncommitted_templates_and_version_cannot_enter_candidate(self):
        desktop = f'packaging/linux/{deb.APP_ID}.desktop'
        expected = git(self.root, 'show', f'HEAD:{desktop}')
        (self.root / desktop).write_text('Exec=unreviewed\n')
        (self.root / 'Cargo.toml').write_text('[package]\nversion="99.0.0"\n')
        stage = self.root / 'pinned-stage'
        receipt = self.stage(stage)
        self.assertEqual(receipt['version'], '0.2.0')
        self.assertEqual((stage / f'usr/share/applications/{deb.APP_ID}.desktop').read_bytes(), expected)

    def test_selected_hash_must_match_before_staging_or_native_execution(self):
        with mock.patch.object(deb, 'run') as native:
            for selected in ['0' * 64, '', 'invalid']:
                with self.subTest(selected=selected), self.assertRaisesRegex(ValueError, 'explicitly selected'):
                    self.stage(self.root / 'rejected', expected_sha256=selected)
            native.assert_not_called()
        self.assertFalse((self.root / 'rejected').exists())

    def test_output_path_reparse_points_are_rejected(self):
        with mock.patch.object(pathlib.Path, 'lstat') as metadata:
            metadata.return_value.st_mode = 0o040755
            metadata.return_value.st_file_attributes = 0x400
            with self.assertRaisesRegex(ValueError, 'links or reparse'):
                deb.output_directory(self.root / 'output/deb')
        with self.assertRaises(FileExistsError):
            deb.output_directory(self.archive)

    def test_build_refuses_changed_metadata_and_preserves_immutable_outputs(self):
        stage = None
        mutate = False
        real_read = pathlib.Path.read_text
        def read(path, *args, **kwargs):
            return 'ID=ubuntu\nVERSION_ID="24.04"\n' if path.as_posix() == '/etc/os-release' else real_read(path, *args, **kwargs)
        def run(command, **kwargs):
            nonlocal stage
            if command[:2] == ['dpkg', '--print-architecture']:
                return 'amd64'
            if command[0].endswith('peerbrush'):
                return 'PeerBrush 0.2.0'
            if command[0] == 'desktop-file-validate':
                return ''
            if command[0] == 'dpkg-shlibdeps':
                return 'shlibs:Depends=libc6 (>= 2.34)'
            if command[:5] == ['dpkg-deb', '--root-owner-group', '-Zxz', '--uniform-compression', '--build']:
                stage = pathlib.Path(command[-2])
                pathlib.Path(command[-1]).write_bytes(b'SYNTHETIC_DEB_BYTES')
                return ''
            if command[0] == 'dpkg-deb' and command[1] in ['--fsys-tarfile', '--ctrl-tarfile']:
                base = stage / 'DEBIAN' if command[1] == '--ctrl-tarfile' else stage
                data = io.BytesIO()
                with tarfile.open(fileobj=data, mode='w') as archive:
                    for path in [base, *base.rglob('*')]:
                        relative = path.relative_to(base)
                        if base == stage and relative.parts and relative.parts[0] == 'DEBIAN':
                            continue
                        entry = tarfile.TarInfo('./' if path == base else './' + relative.as_posix())
                        entry.uid = entry.gid = 0
                        entry.mode = 0o755 if path.is_dir() or path.name == 'peerbrush' else 0o644
                        entry.type = tarfile.DIRTYPE if path.is_dir() else tarfile.REGTYPE
                        content = b'' if path.is_dir() else path.read_bytes()
                        entry.size = len(content)
                        if mutate and path.name == 'peerbrush' and path.is_file():
                            entry.mode = 0o644
                        archive.addfile(entry, None if path.is_dir() else io.BytesIO(content))
                return data.getvalue()
            raise AssertionError('Unexpected native command')
        output = self.root / 'output/deb'
        with mock.patch.object(deb.sys, 'platform', 'linux'), mock.patch.object(pathlib.Path, 'read_text', autospec=True, side_effect=read), mock.patch.object(deb, 'run', side_effect=run):
            candidate = deb.build(self.root, self.archive, output, 'Synthetic CI <builds@example.invalid>', digest(self.archive))
            before = {p.name: p.read_bytes() for p in output.glob('*') if p.is_file()}
            with self.assertRaisesRegex(ValueError, 'immutable'):
                deb.build(self.root, self.archive, output, 'Synthetic CI <builds@example.invalid>', digest(self.archive))
            self.assertEqual(before, {p.name: p.read_bytes() for p in output.glob('*') if p.is_file()})
            self.assertTrue(candidate.is_file())
            mutate = True
            with self.assertRaisesRegex(ValueError, 'bytes or modes'):
                deb.build(self.root, self.archive, self.root / 'output/rejected', 'Synthetic CI <builds@example.invalid>', digest(self.archive))
            self.assertFalse(list((self.root / 'output/rejected').glob('*.deb')))

    def test_version_order_and_control_fields_are_bounded(self):
        self.assertEqual(deb.deb_version('0.30.0-dev.1'), '0.30.0~dev.1-1')
        self.assertEqual(deb.deb_version('0.30.0'), '0.30.0-1')
        text = deb.control('0.30.0', 'libc6 (>= 2.34)', 123, 'Synthetic Maintainer <builds@example.invalid>')
        self.assertIn('Architecture: amd64\n', text)
        self.assertIn('Depends: libc6 (>= 2.34)\n', text)
        for version in ['../../x', '0.30.0\nBad: value', '0.30.0+private']:
            with self.assertRaises(ValueError):
                deb.deb_version(version)
        for contact in ['missing contact', 'Name <a@b>\nBad: value']:
            with self.assertRaises(ValueError):
                deb.control('0.30.0', 'libc6', 1, contact)
        with self.assertRaises(ValueError):
            deb.control('0.30.0', 'libc6\nBad: value', 1, 'Name <a@b>')


class TarInspection(unittest.TestCase):
    def test_final_ar_requires_exact_members_compression_owners_and_complete_bounded_streams(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-ar-fixture-') as temporary:
            package = pathlib.Path(temporary) / 'synthetic.deb'
            tar = tar_bytes([('.', 0o755, None), ('control', 0o644, b'synthetic')])
            members = [('debian-binary', b'2.0\n'), ('control.tar.xz', lzma.compress(tar)), ('data.tar.xz', lzma.compress(tar))]
            package.write_bytes(ar_bytes(members))
            self.assertEqual(deb.deb_archives(package), {'control.tar.xz': tar, 'data.tar.xz': tar})
            variants = [b'not-ar', ar_bytes(members)[:-1], ar_bytes(members) + b'extra',
                        ar_bytes(members + [('extra', b'bad')]), ar_bytes([members[0], members[2], members[1]]),
                        ar_bytes([('debian-binary', b'1.0\n'), *members[1:]]),
                        ar_bytes([members[0], ('control.tar.gz', members[1][1]), members[2]]),
                        ar_bytes([members[0], ('control.tar.xz', members[1][1] + lzma.compress(b'extra')), members[2]]),
                        ar_bytes([members[0], ('control.tar.xz', b'not-xz'), members[2]])]
            # ar uid is a fixed-width field in its first member header.
            owner = bytearray(ar_bytes(members))
            owner[8 + 28:8 + 34] = b'1000  '
            variants.append(bytes(owner))
            for index, data in enumerate(variants):
                with self.subTest(mutation=index):
                    package.write_bytes(data)
                    with self.assertRaises(ValueError):
                        deb.deb_archives(package)
            package.write_bytes(ar_bytes(members))
            with mock.patch.object(deb, 'MAX_CONTROL_BYTES', 1024), self.assertRaisesRegex(ValueError, 'decoded-byte budget'):
                deb.deb_archives(package)
            with mock.patch.object(deb, 'MAX_DEB_BYTES', 64), self.assertRaisesRegex(ValueError, 'compressed-byte budget'):
                deb.deb_archives(package)

    def test_tar_rejects_extended_headers_hidden_trailing_bytes_and_resource_excess(self):
        original = tar_bytes([('.', 0o755, None), ('regular', 0o644, b'original')])
        for kind in [tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.GNUTYPE_LONGNAME, tarfile.GNUTYPE_SPARSE,
                     tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE]:
            entry = tarfile.TarInfo('extended')
            entry.type = kind
            entry.mode = 0o644
            with self.subTest(type=kind), self.assertRaises(ValueError):
                deb.tar_entries(entry.tobuf() + b'\0' * 1024)
        with self.assertRaisesRegex(ValueError, 'trailing content'):
            deb.tar_entries(original + b'x' + b'\0' * 511)
        with self.assertRaisesRegex(ValueError, 'terminator'):
            deb.tar_entries(original[:1536])
        with self.assertRaisesRegex(ValueError, 'byte budget'):
            deb.tar_entries(original, limit=1024)
        with mock.patch.object(deb, 'MAX_TAR_MEMBERS', 1), self.assertRaisesRegex(ValueError, 'member budget'):
            deb.tar_entries(original)
        invalid_header = bytearray(original)
        invalid_header[0] ^= 1
        with self.assertRaisesRegex(ValueError, 'invalid tar header'):
            deb.tar_entries(bytes(invalid_header))
        directory_alias = tarfile.TarInfo('regular/')
        directory_alias.mode = 0o644
        with self.assertRaisesRegex(ValueError, 'directory path aliases'):
            deb.tar_entries(directory_alias.tobuf() + b'\0' * 1024)

    def test_native_failures_keep_bounded_diagnostics_without_arguments_or_environment(self):
        failure = subprocess.CalledProcessError(100, ['apt-get', 'SYNTHETIC_ARGUMENT_NOT_FOR_LOGS'],
                                               output='progress\n' * 1000, stderr='E: synthetic unmet dependencies\x00\n')
        with mock.patch.object(installation.subprocess, 'run', side_effect=failure):
            with self.assertRaisesRegex(ValueError, 'apt-get exited 100') as caught:
                installation.run(['apt-get', 'SYNTHETIC_ARGUMENT_NOT_FOR_LOGS'], env={'SYNTHETIC_PRIVATE_ENV': 'do not log'})
        message = str(caught.exception)
        self.assertIn('E: synthetic unmet dependencies', message)
        self.assertNotIn('SYNTHETIC_ARGUMENT_NOT_FOR_LOGS', message)
        self.assertNotIn('SYNTHETIC_PRIVATE_ENV', message)
        self.assertNotIn('\x00', message)
        self.assertLess(len(message), 4300)
        timeout = subprocess.TimeoutExpired(['apt-get'], 600, stderr=b'synthetic timeout detail')
        with mock.patch.object(installation.subprocess, 'run', side_effect=timeout):
            with self.assertRaisesRegex(ValueError, 'apt-get timed out.*') as caught:
                installation.run(['apt-get'])
        self.assertIn('synthetic timeout detail', str(caught.exception))

    def test_installation_check_refuses_host_before_any_command_or_writes(self):
        with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(installation, 'run') as native:
            with self.assertRaisesRegex(ValueError, 'never run on the host'):
                installation.check(pathlib.Path('not-an-installed-package.deb'), {}, pathlib.Path('no-receipt.json'))
            native.assert_not_called()

    def make_tar(self, mutation=None):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode='w') as archive:
            for name, mode, content in [('.', 0o755, None), ('usr', 0o755, None),
                                         ('usr/bin', 0o755, None), ('usr/bin/peerbrush', 0o755, b'original')]:
                entry = tarfile.TarInfo('./' if name == '.' else './' + name)
                entry.uid = entry.gid = 0
                entry.mode = mode
                entry.type = tarfile.DIRTYPE if content is None else tarfile.REGTYPE
                entry.size = 0 if content is None else len(content)
                if mutation:
                    mutation(entry)
                archive.addfile(entry, None if content is None or not entry.isfile() else io.BytesIO(content))
        return data.getvalue()

    def test_file_headers_owner_modes_and_exact_content_are_required(self):
        expected = {name: {'mode': 0o755, 'directory': True} for name in ['.', 'usr', 'usr/bin']}
        expected['usr/bin/peerbrush'] = {'mode': 0o755, 'sha256': hashlib.sha256(b'original').hexdigest()}
        deb.verify_tar(self.make_tar(), expected)
        def change(field, value):
            return lambda entry: setattr(entry, field, value) if entry.name.endswith('peerbrush') else None
        for mutation in [change('uid', 1000), change('gid', 1000), change('uname', 'daemon'), change('gname', 'daemon'), change('mode', 0o644),
                         change('name', '../../escape'), change('name', './usr/bin'),
                         change('type', tarfile.SYMTYPE), change('type', tarfile.LNKTYPE),
                         change('type', tarfile.FIFOTYPE)]:
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                deb.verify_tar(self.make_tar(mutation), expected)
        with self.assertRaises(ValueError):
            deb.verify_tar(self.make_tar(), expected | {'unexpected': {'mode': 0o644, 'sha256': '0' * 64}})
        with self.assertRaises(ValueError):
            deb.verify_tar(self.make_tar(), expected | {'usr/bin/peerbrush': {'mode': 0o755, 'sha256': '0' * 64}})


class InstallationEvidence(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='peerbrush-bound-install-fixture-')
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name).resolve()
        self.package = self.root / 'peerbrush_0.30.0~dev.1-1_amd64.deb'
        self.package.write_bytes(b'SYNTHETIC_PACKAGE_NEVER_INSTALLED')
        self.output = self.root / 'installation.json'
        self.expected = dict(version='0.30.0-dev.1', commit='a' * 40, package_sha256=digest(self.package),
                             executable_sha256='b' * 64, source_sha256='c' * 64)
        self.fields = dict(Package='peerbrush', Architecture='amd64', Version='0.30.0~dev.1-1')
        self.info = dict(format='PeerBrushBuildInfoV1', schema=1, product='PeerBrush',
                         version=self.expected['version'], commit=self.expected['commit'], tree='clean',
                         end='PeerBrushBuildInfoEnd')

    def test_explicit_selection_checks_all_identity_fields_without_installing(self):
        with mock.patch.object(installation, 'run', side_effect=lambda command: self.fields[command[-1]]) as native:
            installation.selected_package(self.package, self.expected)
        self.assertEqual([call.args[0][-1] for call in native.call_args_list], ['Package', 'Architecture', 'Version'])
        self.assertTrue(all(call.args[0][:2] == ['dpkg-deb', '--field'] for call in native.call_args_list))
        self.assertFalse(self.output.exists())

    def test_changed_hash_invalid_metadata_and_wrong_package_identity_fail_before_mutation(self):
        for field, value in [('package_sha256', 'd' * 64), ('commit', 'not-a-source'),
                             ('version', '0.30.0+unsupported'), ('source_sha256', True),
                             ('executable_sha256', 'B' * 64), ('release_ready', True)]:
            expected = dict(self.expected, **{field: value})
            with self.subTest(field=field), mock.patch.object(installation, 'require_disposable'), \
                    mock.patch.object(installation, 'run') as native:
                with self.assertRaises(ValueError):
                    installation.check(self.package, expected, self.output)
                native.assert_not_called()
            self.assertFalse(self.output.exists())
        for field, value in [('Package', 'another-app'), ('Architecture', 'arm64'), ('Version', '0.26.0-1')]:
            fields = dict(self.fields, **{field: value})
            with self.subTest(field=field), mock.patch.object(installation, 'require_disposable'), \
                    mock.patch.object(installation, 'run', side_effect=lambda command: fields[command[-1]]) as native:
                with self.assertRaisesRegex(ValueError, 'identity, architecture or version'):
                    installation.check(self.package, self.expected, self.output)
                self.assertTrue(all(call.args[0][:2] == ['dpkg-deb', '--field'] for call in native.call_args_list))
            self.assertFalse(self.output.exists())
        renamed = self.package.with_name('other.deb')
        renamed.write_bytes(self.package.read_bytes())
        with mock.patch.object(installation, 'run') as native:
            with self.assertRaisesRegex(ValueError, 'filename or bytes'):
                installation.selected_package(renamed, self.expected)
            native.assert_not_called()

    def verify_installed(self, *, hashes=None, fields=None, version=None, info=None, package_version=None):
        hashes = hashes or dict(peerbrush=self.expected['executable_sha256'], source=self.expected['source_sha256'])
        fields = fields or self.fields
        self.user = mock.Mock(side_effect=lambda command: {
            '--version': version if version is not None else 'PeerBrush ' + self.expected['version'],
            '--build-info': json.dumps(info if info is not None else self.info, separators=(',', ':')),
        }[command[-1]])
        with mock.patch.object(pathlib.Path, 'is_symlink', return_value=False), \
                mock.patch.object(pathlib.Path, 'is_file', return_value=True), \
                mock.patch.object(installation, 'digest', side_effect=lambda path: hashes['peerbrush' if path.name == 'peerbrush' else 'source']), \
                mock.patch.object(installation, 'run', side_effect=lambda command: fields[command[1].removeprefix('--showformat=${').removesuffix('}')]):
            installation.installed_identity(self.expected, self.user, package_version or self.fields['Version'])

    def test_installed_identity_keeps_original_build_labels_during_private_package_upgrade(self):
        for version in [self.fields['Version'], self.fields['Version'] + '+qa1', self.fields['Version']]:
            self.verify_installed(fields=dict(self.fields, Version=version), package_version=version)
            self.assertEqual([call.args[0][-1] for call in self.user.call_args_list], ['--version', '--build-info'])

    def test_installed_hashes_and_cli_labels_must_match_before_acceptance(self):
        for name in ['peerbrush', 'source']:
            hashes = dict(peerbrush=self.expected['executable_sha256'], source=self.expected['source_sha256'])
            hashes[name] = 'd' * 64
            with self.subTest(hash=name), self.assertRaisesRegex(ValueError, 'Installed .* bytes differ'):
                self.verify_installed(hashes=hashes)
            self.user.assert_not_called()
        for field, value in [('Package', 'another-app'), ('Architecture', 'arm64'), ('Version', '0.26.0-1')]:
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'Installed package identity'):
                self.verify_installed(fields=dict(self.fields, **{field: value}))
            self.user.assert_not_called()
        with self.assertRaisesRegex(ValueError, 'application version'):
            self.verify_installed(version='PeerBrush 0.26.0')
        for field, value in [('tree', 'dirty'), ('tree', 'unknown'), ('commit', 'd' * 40),
                             ('version', '0.26.0'), ('schema', True), ('unexpected', 'not accepted')]:
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'build provenance'):
                self.verify_installed(info=dict(self.info, **{field: value}))

    def test_missing_or_linked_installed_source_never_reaches_runtime_acceptance(self):
        binary = pathlib.Path('/usr/bin/peerbrush')
        source = pathlib.Path('/usr/share/doc/peerbrush/PeerBrush-0.30.0-dev.1-source.zip')
        for path in [binary, source]:
            for linked in [False, True]:
                with self.subTest(path=path.name, linked=linked), \
                        mock.patch.object(pathlib.Path, 'is_symlink', autospec=True,
                                          side_effect=lambda candidate: linked and candidate == path), \
                        mock.patch.object(pathlib.Path, 'is_file', autospec=True,
                                          side_effect=lambda candidate: candidate != path), \
                        mock.patch.object(installation, 'digest', return_value=self.expected['executable_sha256']), \
                        mock.patch.object(installation, 'run') as native:
                    user = mock.Mock()
                    label = 'executable' if path == binary else 'corresponding source'
                    reason = 'is a symbolic link' if linked else 'is missing or not a regular file'
                    with self.assertRaisesRegex(ValueError, 'Installed ' + label + ' ' + reason):
                        installation.installed_identity(self.expected, user, self.fields['Version'])
                    native.assert_not_called()
                    user.assert_not_called()
        self.assertFalse(self.output.exists())

    def test_receipts_are_canonical_exclusive_and_cannot_replace_previous_evidence(self):
        if os.name == 'posix':
            shared = self.root / 'shared'
            shared.mkdir()
            shared.chmod(0o755)
            with self.assertRaisesRegex(ValueError, 'parent must be private'):
                installation.receipt_path(shared / 'not-private.json')
        receipt = dict(schema=1, release_ready=False, production_readiness='not-assessed', checks={'installation': 'passed'})
        installation.write_receipt(self.output, receipt)
        before = self.output.read_bytes()
        self.assertEqual(before, (json.dumps(receipt, sort_keys=True, separators=(',', ':')) + '\n').encode())
        with self.assertRaisesRegex(ValueError, 'new receipt'):
            installation.write_receipt(self.output, dict(receipt, checks={}))
        self.assertEqual(self.output.read_bytes(), before)
        with mock.patch.object(installation, 'require_disposable'), mock.patch.object(installation, 'run') as native:
            with self.assertRaisesRegex(ValueError, 'new receipt'):
                installation.check(self.package, self.expected, self.output)
            native.assert_not_called()


if __name__ == '__main__':
    unittest.main()
