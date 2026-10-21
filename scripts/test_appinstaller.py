"""Offline candidate/byte/native-boundary fixtures; no real signing or installation."""
import hashlib
import io
import json
from pathlib import Path
import stat
import unittest
from unittest import mock
import xml.etree.ElementTree as ET
import zipfile

import prepare_appinstaller as installer
import qualify_signed_msix as signed
import test_msix
import test_signed_msix as fixtures

PUBLISHER = 'CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def runtime_bytes(changes=None, framework='true', *, applications=False, dependencies=False):
    identity = dict(Name='Microsoft.VCLibs.140.00.UWPDesktop', Publisher=PUBLISHER,
                    Version='14.0.33519.0', ProcessorArchitecture='x64')
    identity.update(changes or {})
    root = ET.Element('Package', xmlns=installer.msix.NS['f'])
    ET.SubElement(root, 'Identity', **identity)
    ET.SubElement(ET.SubElement(root, 'Properties'), 'Framework').text = framework
    if applications:
        ET.SubElement(root, 'Applications')
    if dependencies:
        ET.SubElement(ET.SubElement(root, 'Dependencies'), 'PackageDependency', Name='Other.Framework')
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w') as archive:
        archive.writestr('AppxManifest.xml', ET.tostring(root))
        archive.writestr('AppxSignature.p7x', b'PKCXoffline-untrusted-runtime-fixture')
        archive.writestr('vcruntime140.dll', b'not an executable or real Microsoft media')
    return data.getvalue()


class AppInstaller(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.SignedMsix()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.parent = self.fixture.external
        self.inputs = self.parent / 'inputs'
        self.inputs.mkdir(mode=0o700)
        self.identity = self.inputs / 'public-direct-identity.json'
        self.identity.write_text(json.dumps(self.fixture.identity), encoding='utf-8')
        self.runtime = self.inputs / 'Microsoft.VCLibs.x64.Desktop.appx'
        self.runtime.write_bytes(runtime_bytes())
        self.license = self.inputs / 'reviewed-runtime-license.txt'
        self.license.write_text('SYNTHETIC_PRIVATE_LICENSE_SELECTION_NOT_FOR_UPLOAD', encoding='utf-8')
        self.options = dict(self.fixture.options, identity=self.identity, runtime_license=self.license,
            runtime_sha256=digest(self.runtime), runtime_certificate_sha256='c' * 64,
            runtime_license_sha256=digest(self.license),
            descriptor_uri='https://downloads.example.invalid/immutable/1.26.0.0/PeerBrush-1.26.0.0-Windows-x64.appinstaller',
            package_uri='https://downloads.example.invalid/immutable/1.26.0.0/' + self.fixture.package.name,
            runtime_uri='https://downloads.example.invalid/immutable/14.0.33519.0/' + self.runtime.name)
        self.runtime_metadata = dict(self.fixture.metadata, subject=PUBLISHER, certificate_sha256='c' * 64)
        self.sequence = 0
        self.native_calls = []

    def native(self, executable, arguments, work, label, **kwargs):
        package = kwargs.get('package') if label == 'certificate' else Path(arguments[-1])
        self.native_calls.append((label, package.name))
        if package.name == self.fixture.package.name:
            return self.fixture.native(executable, arguments, work, label, **kwargs)
        self.assertEqual(package, work / self.runtime.name)
        if label == 'signtool':
            self.assertEqual(executable, self.fixture.signtool)
            self.assertEqual(arguments, ['verify', '/pa', '/all', '/tw', '/q', str(package)])
            return b'PRIVATE_NATIVE_OUTPUT_IS_NOT_AN_ARTIFACT'
        self.assertEqual(label, 'certificate')
        self.assertEqual(executable, self.fixture.powershell)
        self.assertEqual(arguments, ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', signed.CERTIFICATE_SCRIPT])
        return json.dumps(self.runtime_metadata).encode()

    def selection(self, **changes):
        self.sequence += 1
        self.work, self.output = self.parent / ('work-' + str(self.sequence)), self.parent / ('output-' + str(self.sequence))
        return dict(self.options, work=self.work, output=self.output, **changes)

    def prepare(self, native=None, **changes):
        selection = self.selection(**changes)
        with mock.patch.object(installer.sys, 'platform', 'win32'), \
                mock.patch.object(signed, '_command', side_effect=native or self.native):
            return installer.prepare(self.fixture.root, self.fixture.portable, self.fixture.unsigned,
                                     self.fixture.package, self.runtime, **selection)

    def test_exact_four_files_bind_real_validators_and_emit_no_update_configuration(self):
        inputs = [self.fixture.portable, self.fixture.unsigned, self.fixture.package, self.runtime, self.identity, self.license]
        original = {path: path.read_bytes() for path in inputs}
        descriptor = self.prepare()
        self.assertEqual(self.native_calls, [('signtool', self.fixture.package.name), ('certificate', self.fixture.package.name),
                                           ('signtool', self.runtime.name), ('certificate', self.runtime.name)])
        self.assertEqual({path.name for path in self.output.iterdir()},
                         {descriptor.name, self.fixture.package.name, self.runtime.name, 'SHA256SUMS'})
        descriptor_bytes = descriptor.read_bytes()
        self.assertTrue(descriptor_bytes.startswith(b"<?xml version='1.0' encoding='UTF-8'?>"))
        self.assertTrue(descriptor_bytes.isascii())
        self.assertNotIn(b'&', descriptor_bytes)
        root = ET.fromstring(descriptor_bytes)
        ns = '{' + installer.INSTALLER_NS + '}'
        self.assertEqual(root.tag, ns + 'AppInstaller')
        self.assertEqual(root.attrib, dict(Version='1.26.0.0', Uri=self.options['descriptor_uri']))
        self.assertEqual([node.tag for node in root], [ns + 'MainPackage', ns + 'Dependencies'])
        self.assertEqual(root[0].attrib, dict(Name=self.fixture.identity['name'], Publisher=self.fixture.identity['publisher'],
            Version='1.26.0.0', ProcessorArchitecture='x64', Uri=self.options['package_uri']))
        self.assertEqual(root[1][0].attrib, dict(Name='Microsoft.VCLibs.140.00.UWPDesktop', Publisher=PUBLISHER,
            Version='14.0.33519.0', ProcessorArchitecture='x64', Uri=self.options['runtime_uri']))
        expected = {path.name: digest(self.output / path.name) for path in [descriptor, self.fixture.package, self.runtime]}
        self.assertEqual((self.output / 'SHA256SUMS').read_bytes(),
            ''.join(expected[name] + '  ' + name + '\n' for name in sorted(expected)).encode())
        self.assertEqual((self.output / self.fixture.package.name).read_bytes(), original[self.fixture.package])
        self.assertEqual((self.output / self.runtime.name).read_bytes(), original[self.runtime])
        self.assertEqual({path: path.read_bytes() for path in inputs}, original)
        for path in self.output.iterdir():
            self.assertNotIn(b'SYNTHETIC_PRIVATE_LICENSE_SELECTION_NOT_FOR_UPLOAD', path.read_bytes())
            self.assertNotIn(b'PRIVATE_NATIVE_OUTPUT_IS_NOT_AN_ARTIFACT', path.read_bytes())

    def test_valid_signed_msix_publishers_unsupported_by_appinstaller_refuse_without_changing_originals(self):
        for publisher in ['CN=PeerBrush \u05d0\u05d1 \u4f5c\u54c1', 'CN=PeerBrush & Team', 'CN="PeerBrush <Studio>"']:
            with self.subTest(publisher=publisher):
                identity = dict(self.fixture.identity, publisher=publisher)
                payload, release = installer.msix.expected_payload(self.fixture.root, self.fixture.portable,
                    identity, require_provenance=True)
                self.fixture.unsigned.write_bytes(test_msix.synthetic_package(payload))
                self.fixture.package.write_bytes(fixtures.signed_package(self.fixture.unsigned.read_bytes()))
                self.fixture.identity = identity
                self.fixture.payload = payload
                self.fixture.metadata = dict(self.fixture.metadata, subject=publisher)
                self.identity.write_text(json.dumps(identity, ensure_ascii=False), encoding='utf-8')
                family = installer.msix.derived_package_family(identity)
                self.options.update(package_family=family,
                    unsigned_sha256=digest(self.fixture.unsigned), package_sha256=digest(self.fixture.package))
                qualified = self.fixture.qualify(identity=identity, package_family=family,
                    unsigned_sha256=digest(self.fixture.unsigned), package_sha256=digest(self.fixture.package))
                self.assertTrue(qualified['content_verified'])
                self.assertEqual(qualified['identity']['publisher'], publisher)
                self.assertEqual(qualified['native_signature'], 'verified-embedded-default-authentication-policy')
                self.assertEqual(release['commit'], self.options['commit'])
                originals = {path: path.read_bytes() for path in [self.fixture.portable, self.fixture.unsigned,
                    self.fixture.package, self.runtime, self.identity, self.license]}
                self.native_calls.clear()
                with self.assertRaisesRegex(ValueError, 'App Installer Publisher requires literal printable ASCII'):
                    self.prepare()
                self.assertFalse(self.output.exists())
                self.assertEqual(self.native_calls, [('signtool', self.fixture.package.name), ('certificate', self.fixture.package.name),
                                                   ('signtool', self.runtime.name), ('certificate', self.runtime.name)])
                self.assertEqual({path: path.read_bytes() for path in originals}, originals)

    def test_explicit_url_schema_and_exact_filenames_refuse_aliases_and_private_selections(self):
        filename = 'PeerBrush-1.26.0.0-Windows-x64.appinstaller'
        good = 'https://downloads.example.invalid/immutable/' + filename
        self.assertEqual(installer.canonical_uri(good, filename), good)
        for bad in [good.replace('https:', 'http:'), good.replace('downloads.', 'USER@downloads.'),
                    good.replace('downloads.', 'Downloads.'), good.replace('.invalid/', '.invalid:443/'),
                    good.replace('downloads.example.invalid', '127.0.0.1'),
                    good + '?token=private', good + '?', good + '#fragment', good + '#',
                    good.replace('/immutable/', '/a/../'), good.replace('/immutable/', '//'),
                    good.replace('/immutable/', '/%69mmutable/'), good.replace('/immutable/', '/a\\b/'),
                    good.replace(filename, 'PeerBrush.appinstaller'), good + '\n', ' ' + good]:
            with self.subTest(uri=bad), self.assertRaises(ValueError):
                installer.canonical_uri(bad, filename)
        with self.assertRaises(ValueError):
            self.prepare(package_uri=good)
        self.assertFalse(self.output.exists())

    def test_missing_source_hash_license_or_native_host_cannot_produce_candidate(self):
        for changes in [dict(commit='d' * 40), dict(version='0.26.0-dev.1'), dict(runtime_sha256='d' * 64),
                        dict(runtime_certificate_sha256=''), dict(runtime_license_sha256='d' * 64),
                        dict(signtool_sha256='d' * 64)]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.prepare(**changes)
            self.assertFalse(self.output.exists())
        selection = self.selection()
        with mock.patch.object(installer.sys, 'platform', 'linux'), mock.patch.object(signed, '_command') as native:
            with self.assertRaisesRegex(ValueError, 'Windows'):
                installer.prepare(self.fixture.root, self.fixture.portable, self.fixture.unsigned,
                                  self.fixture.package, self.runtime, **selection)
        native.assert_not_called()
        self.assertFalse(self.output.exists())

    def test_framework_exact_identity_native_version_and_standalone_layout_are_required(self):
        cases = [dict(changes={'Name': 'Microsoft.VCLibs.140.00.DebugUWPDesktop'}),
                 dict(changes={'Publisher': 'CN=Other'}), dict(changes={'ProcessorArchitecture': 'x86'}),
                 dict(changes={'Version': '14.0.30703.0'}), dict(changes={'Version': '014.0.33519.0'}),
                 dict(changes={'Version': '65536.0.0.0'}), dict(changes={'ResourceId': 'resources'}),
                 dict(framework='false'), dict(applications=True), dict(dependencies=True)]
        for changes in cases:
            self.runtime.write_bytes(runtime_bytes(**changes))
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.prepare(runtime_sha256=digest(self.runtime))
            self.assertFalse(self.output.exists())

    def test_runtime_archive_budget_links_case_aliases_and_entities_are_refused(self):
        original = runtime_bytes()
        for damage in ['large', 'entity', 'utf16', 'duplicate', 'traversal', 'link']:
            with zipfile.ZipFile(io.BytesIO(original)) as source:
                entries = {name: source.read(name) for name in source.namelist()}
            if damage == 'large':
                entries['AppxManifest.xml'] = b' ' * (installer.MANIFEST_LIMIT + 1)
            elif damage == 'entity':
                entries['AppxManifest.xml'] = b'<!DOCTYPE Package [<!ENTITY a "x">]>' + entries['AppxManifest.xml']
            elif damage == 'utf16':
                entries['AppxManifest.xml'] = entries['AppxManifest.xml'].decode().encode('utf-16')
            elif damage == 'duplicate':
                entries['appxmanifest.xml'] = entries['AppxManifest.xml']
            elif damage == 'traversal':
                entries['../private'] = b'not allowed'
            data = io.BytesIO()
            with zipfile.ZipFile(data, 'w') as archive:
                for name, content in entries.items():
                    archive.writestr(name, content)
                if damage == 'link':
                    link = zipfile.ZipInfo('unsafe-link')
                    link.external_attr = (stat.S_IFLNK | 0o777) << 16
                    archive.writestr(link, b'/private')
            self.runtime.write_bytes(data.getvalue())
            with self.subTest(damage=damage), self.assertRaises((ValueError, UnicodeError)):
                self.prepare(runtime_sha256=digest(self.runtime))
            self.assertFalse(self.output.exists())

    def test_runtime_native_failure_and_strict_publisher_leaf_timestamp_policy_stay_mandatory(self):
        def refuse(executable, arguments, work, label, **kwargs):
            if label == 'signtool' and Path(arguments[-1]).name == self.runtime.name:
                raise ValueError('PRIVATE_NATIVE_ERROR')
            return self.native(executable, arguments, work, label, **kwargs)
        with self.assertRaisesRegex(ValueError, 'PRIVATE_NATIVE_ERROR'):
            self.prepare(native=refuse)
        self.assertFalse(self.output.exists())
        original = dict(self.runtime_metadata)
        for key, value in [('subject', 'CN=Other'), ('certificate_sha256', 'd' * 64),
                           ('timestamp_certificate_sha256', ''), ('status', 1), ('signers', 2), ('digest', 'sha1')]:
            self.runtime_metadata = dict(original, **{key: value})
            with self.subTest(field=key), self.assertRaises(ValueError):
                self.prepare()
            self.assertFalse(self.output.exists())

    def test_all_original_owned_license_identity_source_and_tool_changes_refuse_output(self):
        paths = [self.runtime, self.identity, self.license, self.fixture.package,
                 self.fixture.signtool, self.fixture.root / 'Cargo.toml']
        originals = {path: path.read_bytes() for path in paths}
        for change in ['runtime', 'owned-runtime', 'identity', 'license', 'owned-license', 'package', 'tool', 'source']:
            def mutate(executable, arguments, work, label, **kwargs):
                data = self.native(executable, arguments, work, label, **kwargs)
                if label == 'certificate' and kwargs['package'].name == self.runtime.name:
                    target = {'runtime': self.runtime, 'owned-runtime': kwargs['package'], 'identity': self.identity,
                              'license': self.license, 'owned-license': work.parent / 'runtime-license.txt',
                              'package': self.fixture.package, 'tool': self.fixture.signtool,
                              'source': self.fixture.root / 'Cargo.toml'}[change]
                    target.chmod(0o600)
                    target.write_bytes(b'changed selected bytes')
                return data
            try:
                with self.subTest(change=change), self.assertRaises(ValueError):
                    self.prepare(native=mutate)
                self.assertFalse(self.output.exists())
            finally:
                for path, data in originals.items():
                    path.write_bytes(data)

    def test_copied_output_mutation_is_detected_and_existing_outputs_are_never_replaced(self):
        real = installer.bounded_file
        def mutate(path, limit, destination=None):
            result = real(path, limit, destination)
            if destination is not None and destination.parent == self.output and destination.name == self.runtime.name:
                destination.write_bytes(b'changed output')
            return result
        with mock.patch.object(installer, 'bounded_file', side_effect=mutate), self.assertRaises(ValueError):
            self.prepare()
        self.assertTrue(self.output.is_dir(), 'Failure preserves private incomplete work, never a publication authority.')
        selection = self.selection()
        self.output.mkdir(mode=0o700)
        marker = self.output / 'existing'
        marker.write_bytes(b'preserve')
        with mock.patch.object(installer.sys, 'platform', 'win32'), mock.patch.object(signed, '_command') as native:
            with self.assertRaises(ValueError):
                installer.prepare(self.fixture.root, self.fixture.portable, self.fixture.unsigned,
                                  self.fixture.package, self.runtime, **selection)
        self.assertEqual(marker.read_bytes(), b'preserve')
        native.assert_not_called()

    def test_actual_cli_requires_explicit_inputs_refuses_reports_and_redacts_native_errors(self):
        selection = dict(self.selection(), source_root=self.fixture.root, portable=self.fixture.portable,
            unsigned=self.fixture.unsigned, package=self.fixture.package, runtime=self.runtime)
        argv = ['prepare-appinstaller']
        for key, value in selection.items():
            argv.extend(['--' + key.replace('_', '-'), str(value)])
        error = io.StringIO()
        with mock.patch.object(installer.sys, 'argv', argv), mock.patch.object(installer.sys, 'platform', 'win32'), \
                mock.patch.object(installer.sys, 'stderr', error), \
                mock.patch.object(signed, '_command', side_effect=ValueError('PRIVATE_NATIVE_ERROR')):
            self.assertEqual(installer.main(), 1)
        self.assertNotIn('PRIVATE_NATIVE_ERROR', error.getvalue())
        self.assertFalse(self.output.exists())
        for arguments in [argv + ['--qualification', 'untrusted.json'], ['prepare-appinstaller']]:
            with mock.patch.object(installer.sys, 'argv', arguments), mock.patch.object(installer.sys, 'stderr', io.StringIO()), \
                    mock.patch.object(installer, 'prepare') as prepare, self.assertRaises(SystemExit):
                installer.main()
            prepare.assert_not_called()
        selection = dict(self.selection(), source_root=self.fixture.root, portable=self.fixture.portable,
            unsigned=self.fixture.unsigned, package=self.fixture.package, runtime=self.runtime)
        argv = ['prepare-appinstaller']
        for key, value in selection.items():
            argv.extend(['--' + key.replace('_', '-'), str(value)])
        message = io.StringIO()
        with mock.patch.object(installer.sys, 'argv', argv), mock.patch.object(installer.sys, 'platform', 'win32'), \
                mock.patch.object(installer.sys, 'stdout', message), mock.patch.object(signed, '_command', side_effect=self.native):
            self.assertEqual(installer.main(), 0)
        self.assertIn('no automatic-update configuration emitted', message.getvalue())
        self.assertIn('remain unverified', message.getvalue())
        self.assertEqual(len(list(self.output.iterdir())), 4)


if __name__ == '__main__':
    unittest.main()
