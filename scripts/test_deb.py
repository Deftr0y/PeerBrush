"""DEB staging checks with synthetic committed source, never real installation."""
import hashlib
import io
import pathlib
import os
import sys
import tarfile
import unittest
from unittest import mock
import xml.etree.ElementTree as ET
import zipfile

import package_deb as deb
import test_deb_installation as installation
import test_package
from package import digest, git


ROOT = pathlib.Path(__file__).resolve().parents[1]


class DebianPackage(unittest.TestCase):
    def setUp(self):
        self.fixture = test_package.Packages()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
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
        expected = (self.root / desktop).read_bytes()
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
            if command[:3] == ['dpkg-deb', '--root-owner-group', '--build']:
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
    def test_installation_check_refuses_host_before_any_command_or_writes(self):
        with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(installation, 'run') as native:
            with self.assertRaisesRegex(ValueError, 'never run on the host'):
                installation.check(pathlib.Path('not-an-installed-package.deb'))
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
        for mutation in [change('uid', 1000), change('gid', 1000), change('mode', 0o644),
                         change('name', '../../escape'), change('name', './usr/bin'),
                         change('type', tarfile.SYMTYPE), change('type', tarfile.LNKTYPE),
                         change('type', tarfile.FIFOTYPE)]:
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                deb.verify_tar(self.make_tar(mutation), expected)
        with self.assertRaises(ValueError):
            deb.verify_tar(self.make_tar(), expected | {'unexpected': {'mode': 0o644, 'sha256': '0' * 64}})
        with self.assertRaises(ValueError):
            deb.verify_tar(self.make_tar(), expected | {'usr/bin/peerbrush': {'mode': 0o755, 'sha256': '0' * 64}})


if __name__ == '__main__':
    unittest.main()
