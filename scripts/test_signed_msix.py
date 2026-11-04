"""Offline byte/binding/native-command fixtures; never signs or validates real trust."""
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
import xml.etree.ElementTree as ET
import zipfile

import package_macos_dmg as files
import package_msix as msix
import qualify_signed_msix as signed
import test_msix
import test_submit_store_release as fixtures


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def signed_package(unsigned, mutate=None):
    with zipfile.ZipFile(io.BytesIO(unsigned)) as archive:
        entries = {name: archive.read(name) for name in archive.namelist()}
    types = ET.fromstring(entries[signed.TYPES])
    ET.SubElement(types, signed.TYPES_NS + 'Override',
        PartName='/' + signed.SIGNATURE, ContentType='application/vnd.ms-appx.signature')
    entries[signed.TYPES] = ET.tostring(types)
    # Deliberately not a real signature. Only the fake native process accepts it.
    entries[signed.SIGNATURE] = b'PKCXoffline-untrusted-certificate-fixture'
    if mutate:
        mutate(entries)
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w') as archive:
        for name, content in entries.items():
            archive.writestr(name, content)
    return data.getvalue()


class SignedMsix(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.PackageTests()
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.root, self.portable = self.fixture.source, self.fixture.portable
        self.identity = dict(name='PeerBrush.DirectFixture', publisher='CN=PeerBrush Direct Fixture',
                             publisher_display_name='PeerBrush fixture')
        self.family = msix.derived_package_family(self.identity)
        self.payload, release = msix.expected_payload(self.root, self.portable, self.identity, require_provenance=True)
        self.unsigned = self.fixture.msix.fixture.output / 'PeerBrush-1.26.0.0-Windows-x64.msix'
        self.unsigned.write_bytes(test_msix.synthetic_package(self.payload))
        self.package = self.unsigned.with_name(self.unsigned.stem + '-signed-r1.msix')
        self.package.write_bytes(signed_package(self.unsigned.read_bytes()))
        self.signtool = self.unsigned.parent / 'signtool.exe'
        self.powershell = self.unsigned.parent / 'pwsh.exe'
        self.signtool.write_bytes(b'not-a-real-sdk-executable')
        self.powershell.write_bytes(b'not-a-real-powershell-executable')
        external = tempfile.TemporaryDirectory(prefix='peerbrush-signed-msix-test-')
        self.addCleanup(external.cleanup)
        self.external = Path(external.name).resolve()
        self.sequence = 0
        self.options = dict(commit=release['commit'], version='0.26.0', identity=self.identity,
            package_family=self.family, portable_sha256=digest(self.portable), unsigned_sha256=digest(self.unsigned),
            package_sha256=digest(self.package), certificate_sha256='a' * 64,
            signtool=self.signtool, signtool_sha256=digest(self.signtool),
            powershell=self.powershell, powershell_sha256=digest(self.powershell))
        self.metadata = dict(schema=1, powershell=signed.POWERSHELL_VERSION, status=0, kind=1,
            subject=self.identity['publisher'], certificate_sha256='a' * 64,
            timestamp_certificate_sha256='b' * 64, signers=1, timestamp_attributes=1, digest='sha256')

    def qualify(self, native=None, **options):
        self.sequence += 1
        selected = dict(self.options, **options, work=self.external / str(self.sequence))
        with mock.patch.object(signed.sys, 'platform', 'win32'), \
                mock.patch.object(signed, '_command', side_effect=native or self.native):
            return signed.inspect_final(self.root, self.portable, self.unsigned, self.package, **selected)

    def native(self, executable, arguments, work, label, **kwargs):
        if label == 'signtool':
            self.assertEqual(arguments[:5], ['verify', '/pa', '/all', '/tw', '/q'])
            self.assertEqual(Path(arguments[5]).name, self.package.name)
            self.assertEqual(executable, self.signtool)
            self.assertFalse(kwargs)
            return b'arbitrary localized native output is not authority'
        self.assertEqual(label, 'certificate')
        self.assertEqual(executable, self.powershell)
        self.assertEqual(arguments, ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', signed.CERTIFICATE_SCRIPT])
        self.assertEqual(kwargs, dict(package=work / self.package.name))
        return json.dumps(self.metadata).encode()

    def test_exact_source_unsigned_signed_bytes_and_native_policy_are_bound(self):
        result = self.qualify()
        self.assertTrue(result['content_verified'])
        self.assertEqual(result['certificate_sha256'], 'a' * 64)
        self.assertEqual(result['timestamp_certificate_sha256'], 'b' * 64)
        self.assertEqual(result['native_producer_authentication'], 'unverified-requires-trusted-native-job')
        self.assertEqual(result['installed_acceptance'], 'not-performed')
        self.assertEqual(result['executable_signing'], 'not-established')
        self.assertFalse(result['ready'])
        self.assertFalse(result['production_ready'])
        self.assertNotIn(str(self.root), json.dumps(result))
        # Signed content inspection is not an implicit switch for Store inputs.
        with self.assertRaisesRegex(ValueError, 'unexpected signature'):
            msix.verify_payload(self.package, self.payload)
        msix.verify_payload(self.unsigned, self.payload)

    def test_input_hash_identity_family_source_and_tool_pins_cannot_be_bypassed(self):
        for field, wrong in [('commit', 'c' * 40), ('version', '0.26.1'),
            ('portable_sha256', 'c' * 64), ('unsigned_sha256', 'c' * 64),
            ('package_sha256', 'c' * 64), ('certificate_sha256', ''),
            ('package_family', 'PeerBrush.DirectFixture_wrong'), ('signtool_sha256', 'c' * 64),
            ('powershell_sha256', 'c' * 64), ('signtool', self.powershell)]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.qualify(**{field: wrong})
        with mock.patch.object(signed, '_command') as command, mock.patch.object(signed.sys, 'platform', 'linux'):
            with self.assertRaisesRegex(ValueError, 'Windows host'):
                signed.inspect_final(self.root, self.portable, self.unsigned, self.package,
                    **dict(self.options, work=self.external / 'unsupported'))
            command.assert_not_called()

    def test_valid_signature_claim_cannot_hide_payload_blockmap_or_content_type_changes(self):
        original = self.package.read_bytes()
        def changed_types(entries):
            tree = ET.fromstring(entries[signed.TYPES])
            ET.SubElement(tree, signed.TYPES_NS + 'Default', Extension='unused', ContentType='text/plain')
            entries[signed.TYPES] = ET.tostring(tree)
        def changed_blocks(entries):
            entries['AppxBlockMap.xml'] = entries['AppxBlockMap.xml'].replace(b'</BlockMap>', b' </BlockMap>')
        for mutate in [lambda entries: entries.update({'peerbrush.exe': b'changed'}),
                       lambda entries: entries.update({'private.txt': b'not allowed'}),
                       lambda entries: entries.update({signed.SIGNATURE: b''}),
                       lambda entries: entries.update({signed.SIGNATURE: b'x' * (msix.FOOTPRINT_LIMIT + 1)}),
                       changed_types, changed_blocks]:
            with self.subTest(mutation=mutate.__name__):
                self.package.write_bytes(signed_package(self.unsigned.read_bytes(), mutate))
                with self.assertRaises(ValueError):
                    self.qualify(package_sha256=digest(self.package))
        self.package.write_bytes(original)

    def test_probe_schema_status_catalog_signers_timestamp_and_certificates_are_strict(self):
        valid = dict(self.metadata)
        for field, value in [('schema', True), ('status', 1), ('kind', 2),
            ('powershell', '7.5.4'), ('subject', 'CN=Other'), ('certificate_sha256', 'c' * 64),
            ('timestamp_certificate_sha256', None), ('timestamp_certificate_sha256', ''),
            ('signers', 2), ('timestamp_attributes', 2), ('digest', 'sha1'), ('success', True)]:
            self.metadata = dict(valid, **{field: value})
            with self.subTest(field=field, value=value), self.assertRaisesRegex(ValueError, 'policy differs'):
                self.qualify()
        self.metadata = valid
        for data in [b'[]', b'{}', b'null', b'not JSON', b' ' * (signed.OUTPUT_LIMIT + 1),
                     json.dumps(valid).replace('"schema": 1', '"schema": 1, "schema": 1').encode()]:
            with self.subTest(bytes=len(data)), self.assertRaises(ValueError):
                signed._certificate(data, self.identity['publisher'], 'a' * 64)

    def test_native_failure_or_late_mutation_cannot_produce_a_report(self):
        def failure(*args, **kwargs):
            raise ValueError('native fixed failure')
        with self.assertRaisesRegex(ValueError, 'native fixed failure'):
            self.qualify(native=failure)
        for change in ['original', 'owned', 'tool', 'source']:
            original = self.package.read_bytes(), self.signtool.read_bytes()
            def mutate(executable, arguments, work, label, **kwargs):
                data = self.native(executable, arguments, work, label, **kwargs)
                if label == 'certificate':
                    if change == 'original':
                        self.package.write_bytes(b'changed')
                    elif change == 'owned':
                        path = kwargs['package']
                        path.chmod(0o600)
                        path.write_bytes(b'changed')
                    elif change == 'tool':
                        self.signtool.write_bytes(b'changed')
                    else:
                        (self.root / 'Cargo.toml').write_bytes(b'changed')
                return data
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.qualify(native=mutate)
            self.package.write_bytes(original[0])
            self.signtool.write_bytes(original[1])
            if change == 'source':
                msix.portable.git(self.root, 'checkout', '--', 'Cargo.toml')

    def test_private_output_is_exclusive_and_readiness_stays_false(self):
        result = self.qualify()
        output = self.external / 'report'
        signed.write_report(result, output, self.root)
        self.assertEqual(json.loads((output / 'signed-msix-qualification.json').read_bytes()), result)
        with self.assertRaises(ValueError):
            signed.write_report(result, output, self.root)
        with self.assertRaises(ValueError):
            signed.write_report(result, self.root / 'public-report', self.root)
        self.assertFalse((self.root / 'public-report').exists())

    def test_actual_cli_has_no_report_or_success_bypass_and_redacts_native_failure(self):
        identity = self.unsigned.parent / 'direct-public-identity.json'
        identity.write_text(json.dumps(self.identity), encoding='utf-8')
        arguments = dict(self.options, identity=identity, source_root=self.root,
            portable=self.portable, unsigned=self.unsigned, package=self.package,
            work=self.external / 'cli-work', output=self.external / 'cli-report')
        argv = ['qualify-signed-msix']
        for key, value in arguments.items():
            argv.extend(['--' + key.replace('_', '-'), str(value)])
        error = io.StringIO()
        with mock.patch.object(signed.sys, 'argv', argv), mock.patch.object(signed.sys, 'platform', 'win32'), \
                mock.patch.object(signed.sys, 'stderr', error), \
                mock.patch.object(signed, '_command', side_effect=ValueError('PRIVATE_NATIVE_CANARY')):
            self.assertEqual(signed.main(), 1)
        self.assertNotIn('PRIVATE_NATIVE_CANARY', error.getvalue())
        self.assertFalse(arguments['output'].exists())
        mutated = dict(arguments, work=self.external / 'cli-mutated-work', output=self.external / 'cli-mutated-report')
        mutated_argv = ['qualify-signed-msix']
        for key, value in mutated.items():
            mutated_argv.extend(['--' + key.replace('_', '-'), str(value)])
        def mutate_identity(executable, native_arguments, work, label, **kwargs):
            value = self.native(executable, native_arguments, work, label, **kwargs)
            if label == 'certificate':
                identity.write_text(json.dumps(dict(self.identity, publisher='CN=Changed fixture')), encoding='utf-8')
            return value
        with mock.patch.object(signed.sys, 'argv', mutated_argv), mock.patch.object(signed.sys, 'platform', 'win32'), \
                mock.patch.object(signed.sys, 'stderr', io.StringIO()), \
                mock.patch.object(signed, '_command', side_effect=mutate_identity) as native:
            self.assertEqual(signed.main(), 1)
        self.assertEqual(native.call_count, 2, 'Both native stages completed before the identity recheck.')
        self.assertFalse(mutated['output'].exists())
        with mock.patch.object(signed.sys, 'argv', argv + ['--report', 'untrusted.json']), \
                mock.patch.object(signed.sys, 'stderr', io.StringIO()), \
                mock.patch.object(signed, 'inspect_final') as inspect, self.assertRaises(SystemExit):
            signed.main()
        inspect.assert_not_called()


class NativeCommand(unittest.TestCase):
    def test_exit_zero_only_fixed_environment_and_no_stdout_text_authority(self):
        for code in [0, 1, 2]:
            with tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary).resolve()
                child = mock.Mock()
                child.poll.return_value = code
                with mock.patch.object(signed.subprocess, 'Popen', return_value=child) as launch:
                    if code == 0:
                        self.assertEqual(signed._command(Path('signtool.exe'), ['verify', '/pa', '/all', '/tw', '/q'], work, 'native'), b'')
                    else:
                        with self.assertRaisesRegex(ValueError, 'without warnings'):
                            signed._command(Path('signtool.exe'), ['verify'], work, 'native')
                    command = launch.call_args
                    self.assertFalse(command.kwargs.get('shell', False))
                    self.assertEqual(command.kwargs['stdin'], signed.subprocess.DEVNULL)
                    self.assertEqual(command.kwargs['stderr'], signed.subprocess.DEVNULL)
                    self.assertNotIn('GITHUB_TOKEN', command.kwargs['env'])
                    child.kill.assert_not_called()

    def test_timeout_and_output_overrun_kill_and_reap_the_owned_child(self):
        for overrun in [False, True]:
            with tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary).resolve()
                child = mock.Mock()
                child.poll.return_value = None
                def launch(*args, **kwargs):
                    if overrun:
                        kwargs['stdout'].write(b'x' * (signed.OUTPUT_LIMIT + 1))
                        kwargs['stdout'].flush()
                    return child
                with mock.patch.object(signed.subprocess, 'Popen', side_effect=launch), \
                        mock.patch.object(signed.time, 'monotonic', side_effect=[0, 121]):
                    with self.assertRaisesRegex(ValueError, 'budget'):
                        signed._command(Path('tool.exe'), ['verify'], work, 'native')
                    child.kill.assert_called_once_with()
                    child.wait.assert_called_once_with(timeout=10)


class ExecutableFileIdentity(unittest.TestCase):
    def test_real_regular_executable_named_file_is_hashed_without_running_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / 'synthetic.exe'
            path.write_bytes(b'bounded non-executable fixture bytes')
            checksum, size = files.bounded_file(path, 1024, executable=True)
            self.assertEqual((checksum, size), (digest(path), path.stat().st_size))

    def test_only_windows_cross_api_execute_bits_differ_full_states_stay_stable(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / 'synthetic.exe'
            path.write_bytes(b'1234')
            metadata = path.stat()
            fields = ['st_dev', 'st_ino', 'st_size', 'st_mtime_ns', 'st_ctime_ns', 'st_nlink']
            base = {name: getattr(metadata, name) for name in fields}
            base.update(st_mode=stat.S_IFREG | 0o600, st_birthtime_ns=10)
            opened = SimpleNamespace(**base)
            named = SimpleNamespace(**dict(base, st_mode=base['st_mode'] | 0o111))
            for enabled, altered in [(False, None), (True, None), (True, 'st_ino'),
                (True, 'st_dev'), (True, 'st_mtime_ns'), (True, 'st_nlink'),
                (True, 'st_birthtime_ns'), (True, 'read_only'), (True, 'path_mode'), (True, 'handle_mode')]:
                current = copy.copy(opened)
                later_named, later_opened = copy.copy(named), copy.copy(opened)
                if altered in fields or altered == 'st_birthtime_ns':
                    setattr(current, altered, getattr(current, altered) + 1)
                elif altered == 'read_only':
                    current.st_mode &= ~0o200
                elif altered == 'path_mode':
                    later_named.st_mode &= ~0o100
                elif altered == 'handle_mode':
                    later_opened.st_mode |= 0o100
                fake_os = SimpleNamespace(**{name: getattr(os, name) for name in dir(os)})
                fake_os.name = 'nt'
                fake_os.fstat = mock.Mock(side_effect=[current, later_opened])
                with self.subTest(enabled=enabled, altered=altered), \
                        mock.patch.object(files, 'os', fake_os), \
                        mock.patch.object(files, 'inspection_path', side_effect=lambda value: Path(value)), \
                        mock.patch.object(Path, 'lstat', side_effect=[named, later_named]):
                    if enabled and altered is None:
                        self.assertEqual(files.bounded_file(path, 4, executable=enabled),
                            (hashlib.sha256(b'1234').hexdigest(), 4))
                    else:
                        with self.assertRaises(ValueError):
                            files.bounded_file(path, 4, executable=enabled)


if __name__ == '__main__':
    unittest.main()
