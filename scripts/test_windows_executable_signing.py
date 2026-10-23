"""Offline PE/real-validator fixtures; no native signing or trust is exercised."""
import hashlib
import io
import json
from pathlib import Path
import struct
import unittest
from unittest import mock
import zipfile

import package_msix as msix
import pe_signing as pe
import qualify_signed_msix as qualified
import sign_windows_msix as signer
import prepare_appinstaller as installer
import test_appinstaller
import test_msix
import test_signed_msix as fixtures


def executable(overlay=b'original-overlay'):
    data = bytearray(528)
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 60, 128)
    data[128:132] = b'PE\0\0'
    struct.pack_into('<HH', data, 132, 0x8664, 1)
    struct.pack_into('<HH', data, 148, 240, 2)
    struct.pack_into('<H', data, 152, 0x20b)
    struct.pack_into('<I', data, 212, 512)
    struct.pack_into('<I', data, 260, 16)
    data[392:400] = b'.text\0\0\0'
    struct.pack_into('<II', data, 408, 16, 512)
    data[512:] = b'original-code-16'
    return bytes(data) + overlay


def signed_executable(data, cms=b'\x30\x03\x02\x01\x01'):
    checksum, security = pe.unsigned(data)
    result = bytearray(data)
    address = (len(data) + 7) & ~7
    result.extend(b'\0' * (address - len(data)))
    length = (8 + len(cms) + 7) & ~7
    result.extend(struct.pack('<IHH', length, 0x200, 2) + cms + b'\0' * (length - 8 - len(cms)))
    struct.pack_into('<I', result, checksum, 0x12345678)
    struct.pack_into('<II', result, security, address, length)
    return bytes(result)


class PeEquivalence(unittest.TestCase):
    def test_every_existing_byte_including_overlay_is_preserved_and_only_documented_changes_pass(self):
        for extra in range(8):
            before = executable(b'overlay' + b'x' * extra)
            after = signed_executable(before)
            pe.signing_only(before, after)
            self.assertEqual(pe.certificate(after), b'\x30\x03\x02\x01\x01')
            for offset in [0, 64, 132, 180, 392, 512, len(before) - 1]:
                changed = bytearray(after)
                changed[offset] ^= 1
                with self.subTest(alignment=extra, offset=offset), self.assertRaises(ValueError):
                    pe.signing_only(before, bytes(changed))

    def test_missing_multiple_trailing_bad_padding_and_malformed_certificate_tables_refuse(self):
        before = executable()
        after = signed_executable(before)
        _, security, address, size, _ = pe.layout(after)
        cases = [before, after + b'junk', after[:-1], signed_executable(before, b'\x30\x80\0\0'),
                 signed_executable(before, b'\x30\x81\x03\x02\x01\x01'),
                 signed_executable(before, b'\x30\x03\x02\x01\x01extra')]
        for offset, value, format in [(address + 4, 0x100, '<H'), (address + 6, 1, '<H'),
                                      (address, size - 8, '<I'), (security, address + 1, '<I')]:
            changed = bytearray(after)
            struct.pack_into(format, changed, offset, value)
            cases.append(bytes(changed))
        changed = bytearray(after)
        changed[-1] = 1
        cases.append(bytes(changed))
        multiple = bytearray(after + after[address:])
        struct.pack_into('<I', multiple, security + 4, size * 2)
        cases.append(bytes(multiple))
        for data in cases:
            with self.subTest(size=len(data)), self.assertRaises(ValueError):
                pe.signing_only(before, data)
        with self.assertRaises(ValueError):
            pe.unsigned(after)

    def test_invalid_section_bounds_and_overlapping_or_truncated_headers_refuse(self):
        data = executable()
        for offset, value, format in [(60, 0xffffffff, '<I'), (134, 0, '<H'),
                                      (148, 151, '<H'), (260, 17, '<I'), (412, 1, '<I'),
                                      (408, 0xffffffff, '<I')]:
            changed = bytearray(data)
            struct.pack_into(format, changed, offset, value)
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                pe.unsigned(bytes(changed))
        changed = bytearray(data)
        struct.pack_into('<H', changed, 134, 2)
        struct.pack_into('<II', changed, 448, 8, 512)
        with self.assertRaises(ValueError):
            pe.unsigned(bytes(changed))


class WindowsProducer(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.SignedMsix()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        fixture = self.fixture
        # Upgrade the existing source/provenance fixture to a structural PE32+.
        with zipfile.ZipFile(fixture.portable) as archive:
            metadata = {item.filename: item for item in archive.infolist()}
            entries = {name: archive.read(name) for name in archive.namelist()}
        name = next(name for name in entries if name.endswith('/peerbrush.exe'))
        self.before = executable(entries[name][128:])
        entries[name] = self.before
        manifest = next(name for name in entries if name.endswith('/release.json'))
        release = json.loads(entries[manifest])
        release['executable_sha256'] = hashlib.sha256(self.before).hexdigest()
        entries[manifest] = (json.dumps(release, indent=2) + '\n').encode()
        with zipfile.ZipFile(fixture.portable, 'w') as archive:
            for name, data in entries.items():
                archive.writestr(metadata[name], data)
        self.makeappx = fixture.signtool.with_name('MakeAppx.exe')
        self.makeappx.write_bytes(b'not-a-real-makeappx')
        self.options = {key: value for key, value in fixture.options.items()
                        if key not in ['unsigned_sha256', 'package_sha256']}
        self.options.update(portable_sha256=fixtures.digest(fixture.portable),
            certificate_sha1='A' * 40, timestamp_uri='https://timestamp.example.invalid/rfc3161',
            makeappx=self.makeappx, makeappx_sha256=fixtures.digest(self.makeappx), revision='1',
            approve_existing_certificate_signing=True)
        self.calls = []
        self.sequence = 0

    def native(self, tool, arguments, work, label, **kwargs):
        self.calls.append(label)
        if label == 'certificate-selection':
            self.assertEqual(tool, self.fixture.powershell)
            self.assertEqual(kwargs, dict(thumbprint='A' * 40))
            return json.dumps(dict(schema=1, powershell=qualified.POWERSHELL_VERSION,
                thumbprint='A' * 40, certificate_sha256='a' * 64,
                subject=self.fixture.identity['publisher'], private_key_available=True)).encode()
        if label.startswith('sign-'):
            self.assertEqual(arguments[:11], ['sign', '/s', 'My', '/sha1', 'A' * 40, '/fd', 'SHA256',
                '/tr', self.options['timestamp_uri'], '/td', 'SHA256'])
            self.assertEqual(arguments[11], '/q')
            path = Path(arguments[-1])
            path.write_bytes(signed_executable(path.read_bytes()) if label == 'sign-executable'
                             else fixtures.signed_package(path.read_bytes()))
            return b''
        if label == 'makeappx':
            self.assertEqual(tool, self.makeappx)
            self.assertEqual(arguments[0], 'pack')
            stage, output = Path(arguments[2]), Path(arguments[4])
            payload = {path.relative_to(stage).as_posix(): path.read_bytes()
                       for path in stage.rglob('*') if path.is_file()}
            output.write_bytes(test_msix.synthetic_package(payload))
            return b''
        if label == 'signtool':
            self.assertEqual(arguments[:5], ['verify', '/pa', '/all', '/tw', '/q'])
            return b'localized output is not trust authority'
        self.assertEqual(label, 'certificate')
        if Path(kwargs['package']).name == 'peerbrush.exe':
            self.assertEqual(arguments[-1], qualified.EXECUTABLE_CERTIFICATE_SCRIPT)
            self.assertEqual(Path(kwargs['certificate']).read_bytes(), pe.certificate(Path(kwargs['package']).read_bytes()))
        else:
            self.assertEqual(arguments[-1], qualified.CERTIFICATE_SCRIPT)
        return json.dumps(self.fixture.metadata).encode()

    def produce(self, native=None, **changes):
        self.sequence += 1
        self.work = self.fixture.external / ('work-' + str(self.sequence))
        self.output = self.fixture.external / ('output-' + str(self.sequence))
        with mock.patch.object(signer.sys, 'platform', 'win32'), \
                mock.patch.object(qualified, '_command', side_effect=native or self.native):
            return signer.produce(self.fixture.root, self.fixture.portable,
                **dict(self.options, work=self.work, output=self.output, **changes))

    def test_real_source_payload_pe_binding_and_final_native_verifiers_are_all_reached(self):
        result = self.produce()
        self.assertEqual(result['executable_signing'], 'verified-embedded-default-authentication-policy')
        self.assertEqual(result['executable']['unsigned_sha256'], hashlib.sha256(self.before).hexdigest())
        self.assertFalse(result['ready'])
        self.assertFalse(result['production_ready'])
        self.assertEqual(result['installed_acceptance'], 'not-performed')
        self.assertEqual(result['native_producer_authentication'], 'unverified-requires-trusted-native-job')
        self.assertEqual(self.calls[:5], ['certificate-selection', 'sign-executable', 'signtool', 'certificate', 'makeappx'])
        self.assertEqual(self.calls[5:], ['sign-package', 'signtool', 'certificate', 'signtool', 'certificate'])
        self.assertEqual(len(list(self.output.iterdir())), 4)
        self.assertNotIn(str(self.work), json.dumps(result))
        with zipfile.ZipFile(self.output / result['package']['file']) as package:
            manifest = json.loads(package.read('release.json'))
            self.assertEqual(manifest['executable_sha256'], result['executable']['sha256'])
            self.assertEqual(manifest['windows_executable_signing']['unsigned_sha256'], result['executable']['unsigned_sha256'])
        # The untouched Store path still derives the original unsigned bytes.
        expected, _ = msix.expected_payload(self.fixture.root, self.fixture.portable, self.fixture.identity, require_provenance=True)
        self.assertEqual(expected['peerbrush.exe'], self.before)

    def test_disabled_selection_wrong_leaf_and_bad_tool_hash_refuse_before_signing(self):
        for change in [dict(approve_existing_certificate_signing=False), dict(certificate_sha1='a' * 40),
                       dict(timestamp_uri='http://timestamp.example.invalid'), dict(makeappx_sha256='c' * 64)]:
            self.calls.clear()
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.produce(**change)
            self.assertEqual(self.calls, [])
            self.assertFalse(self.output.exists())
        def wrong_certificate(*args, **kwargs):
            data = json.loads(self.native(*args, **kwargs))
            data['certificate_sha256'] = 'c' * 64
            return json.dumps(data).encode()
        with self.assertRaises(ValueError):
            self.produce(native=wrong_certificate)
        self.assertEqual(self.calls, ['certificate-selection'])

    def test_signed_executable_route_reaches_actual_appinstaller_and_legacy_payload_refuses_strict_mode(self):
        result = self.produce()
        inputs = self.fixture.external / 'installer-inputs'
        inputs.mkdir(mode=0o700)
        identity = inputs / 'public-identity.json'
        identity.write_text(json.dumps(self.fixture.identity))
        runtime = inputs / 'Microsoft.VCLibs.x64.Desktop.appx'
        runtime.write_bytes(test_appinstaller.runtime_bytes())
        license = inputs / 'runtime-license.txt'
        license.write_text('Synthetic reviewed redistribution-license fixture')
        unsigned_package = self.output / 'PeerBrush-1.26.0.0-Windows-x64.msix'
        package = self.output / result['package']['file']
        def native(tool, arguments, work, label, **kwargs):
            selected = kwargs.get('package') if label == 'certificate' else Path(arguments[-1])
            if selected.name == runtime.name:
                if label == 'signtool':
                    self.assertEqual(arguments[:5], ['verify', '/pa', '/all', '/tw', '/q'])
                    return b''
                return json.dumps(dict(self.fixture.metadata,
                    subject=test_appinstaller.PUBLISHER, certificate_sha256='c' * 64)).encode()
            return self.native(tool, arguments, work, label, **kwargs)
        options = {key: self.options[key] for key in ['commit', 'version', 'package_family', 'portable_sha256',
            'certificate_sha256', 'signtool', 'signtool_sha256', 'powershell', 'powershell_sha256']}
        with mock.patch.object(qualified.sys, 'platform', 'win32'), mock.patch.object(qualified, '_command', side_effect=native):
            descriptor = installer.prepare(self.fixture.root, self.fixture.portable, unsigned_package, package, runtime,
                **options, identity=identity, runtime_license=license, unsigned_sha256=fixtures.digest(unsigned_package),
                package_sha256=fixtures.digest(package), runtime_sha256=fixtures.digest(runtime),
                runtime_certificate_sha256='c' * 64, runtime_license_sha256=fixtures.digest(license),
                descriptor_uri='https://downloads.example.invalid/immutable/PeerBrush-1.26.0.0-Windows-x64.appinstaller',
                package_uri='https://downloads.example.invalid/immutable/' + package.name,
                runtime_uri='https://downloads.example.invalid/immutable/' + runtime.name,
                work=self.fixture.external / 'installer-work', output=self.fixture.external / 'installer-output',
                require_executable_signature=True)
        self.assertTrue(descriptor.is_file())
        self.assertNotIn(b'UpdateSettings', descriptor.read_bytes())
        self.assertEqual(len(list(descriptor.parent.iterdir())), 4)
        with self.assertRaises(ValueError):
            self.fixture.qualify(require_executable_signature=True,
                portable_sha256=self.options['portable_sha256'])

    def test_code_overlay_native_failure_and_final_package_changes_cannot_produce_output(self):
        for change in ['code', 'overlay', 'native', 'package', 'source', 'tool']:
            self.calls.clear()
            def changed(tool, arguments, work, label, **kwargs):
                if change == 'native' and label == 'signtool':
                    raise ValueError('PRIVATE_NATIVE_FAILURE')
                data = self.native(tool, arguments, work, label, **kwargs)
                if label == 'sign-executable' and change in ['code', 'overlay']:
                    path = Path(arguments[-1])
                    value = bytearray(path.read_bytes())
                    value[512 if change == 'code' else len(self.before) - 1] ^= 1
                    path.write_bytes(value)
                if label == 'sign-package' and change == 'package':
                    path = Path(arguments[-1])
                    path.write_bytes(fixtures.signed_package((work / 'packages' / 'PeerBrush-1.26.0.0-Windows-x64.msix').read_bytes(),
                        lambda entries: entries.update({'peerbrush.exe': b'changed'})))
                if label == 'certificate' and change == 'source':
                    (self.fixture.root / 'Cargo.toml').write_bytes(b'changed')
                if label == 'certificate' and change == 'tool':
                    self.makeappx.write_bytes(b'changed')
                return data
            original_tool = self.makeappx.read_bytes()
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.produce(native=changed)
            self.assertFalse(self.output.exists())
            self.makeappx.write_bytes(original_tool)
            if change == 'source':
                msix.portable.git(self.fixture.root, 'checkout', '--', 'Cargo.toml')

    def test_public_cli_is_opt_in_has_no_secret_file_or_success_report_inputs_and_redacts_failure(self):
        identity = self.fixture.signtool.with_name('public-identity.json')
        identity.write_text(json.dumps(self.fixture.identity))
        options = dict(self.options, source_root=self.fixture.root, portable=self.fixture.portable,
            identity=identity, work=self.fixture.external / 'cli-work', output=self.fixture.external / 'cli-output')
        argv = ['sign-windows-msix']
        for key, value in options.items():
            if type(value) is bool:
                if value:
                    argv.append('--' + key.replace('_', '-'))
            else:
                argv.extend(['--' + key.replace('_', '-'), str(value)])
        error = io.StringIO()
        with mock.patch.object(signer.sys, 'argv', argv), mock.patch.object(signer.sys, 'stderr', error), \
                mock.patch.object(signer, 'produce', side_effect=ValueError('PRIVATE_PROVIDER_FAILURE')):
            self.assertEqual(signer.main(), 1)
        self.assertNotIn('PRIVATE_PROVIDER_FAILURE', error.getvalue())
        for option in ['--pfx', '--private-key', '--password', '--report', '--success']:
            with mock.patch.object(signer.sys, 'argv', argv + [option, 'untrusted']), \
                    mock.patch.object(signer.sys, 'stderr', io.StringIO()), mock.patch.object(signer, 'produce') as produce, \
                    self.assertRaises(SystemExit):
                signer.main()
            produce.assert_not_called()


if __name__ == '__main__':
    unittest.main()
