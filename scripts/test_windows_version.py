"""Synthetic resource byte/consumer checks and explicit Windows native API readback."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import sys
import tomllib
import unittest
import zipfile

import package as packaging
import package_msix as msix
import test_msix
import test_msix_runtime_tests as runtime
import test_windows_executable_signing as pe_fixtures
from verify_release import inspect_archive
import windows_version as version

ROOT = Path(__file__).resolve().parents[1]


def block(key, value=b'', kind=1, children=(), length=None):
    key = (key + '\0').encode('utf-16le')
    data = bytearray(b'\0' * 6 + key)
    data.extend(b'\0' * (-len(data) % 4))
    data.extend(value)
    for child in children:
        data.extend(b'\0' * (-len(data) % 4))
        data.extend(child)
    count = len(value) // 2 if kind else len(value)
    struct.pack_into('<HHH', data, 0, len(data), count if length is None else length, kind)
    return bytes(data)


def version_value(text='0.2.0', *, fields=None, fixed=None, translation=(0x409, 1200), duplicate=False):
    major, minor, patch, revision = version.numeric(text)
    flags = 2 if '-' in text.split('+')[0] else 0
    words = [(major << 16) | minor, (patch << 16) | revision]
    fixed = fixed or [0xfeef04bd, 0x10000, *words, *words, 0x3f, flags, 0x40004, 1, 0, 0, 0]
    fields = version.strings(text) if fields is None else fields
    strings = [block(name, (value + '\0').encode('utf-16le')) for name, value in fields.items()]
    if duplicate:
        strings.append(strings[0])
    string_info = block('StringFileInfo', children=[block('040904B0', children=strings)])
    variable_info = block('VarFileInfo', children=[block('Translation', struct.pack('<HH', *translation), 0)])
    return block('VS_VERSION_INFO', struct.pack('<13I', *fixed), 0, [string_info, variable_info])


def resource_executable(text='0.2.0', *, value=None, overlay=b'', multiple=False, root_keys=None):
    value = value if value is not None else version_value(text)
    # Independent PE/section/directory fixture. It is never executed or trusted.
    data = bytearray(pe_fixtures.executable(b'')[:528])
    struct.pack_into('<H', data, 134, 2)
    struct.pack_into('<II', data, 400, 16, 0x1000)
    keys = root_keys if root_keys is not None else [16, 16] if multiple else [16]
    first = 16 + 8 * len(keys)
    second, leaf = first + 24, first + 48
    tree = bytearray(leaf + 16)
    named = sum(isinstance(key, str) for key in keys)
    struct.pack_into('<HH', tree, 12, named, len(keys) - named)
    for index, key in enumerate(keys):
        if isinstance(key, str):
            encoded = key.encode('utf-16le')
            start = len(tree)
            tree.extend(struct.pack('<H', len(encoded) // 2) + encoded)
            key = 0x80000000 | start
        struct.pack_into('<II', tree, 16 + 8 * index, key, 0x80000000 | first)
    struct.pack_into('<H', tree, first + 14, 1)
    struct.pack_into('<II', tree, first + 16, 1, 0x80000000 | second)
    struct.pack_into('<H', tree, second + 14, 1)
    struct.pack_into('<II', tree, second + 16, 0x409, leaf)
    tree.extend(b'\0' * (-len(tree) % 4))
    struct.pack_into('<IIII', tree, leaf, 0x2000 + len(tree), len(value), 0, 0)
    tree.extend(value)
    size = (len(tree) + 511) & ~511
    data[432:440] = b'.rsrc\0\0\0'
    struct.pack_into('<IIII', data, 440, len(tree), 0x2000, size, 1024)
    struct.pack_into('<II', data, 280, 0x2000, len(tree))
    data.extend(b'\0' * (1024 - len(data)))
    data.extend(tree + b'\0' * (size - len(tree)))
    return bytes(data) + overlay


class ResourceBytes(unittest.TestCase):
    def test_resource_table_order_matches_the_native_lookup_contract(self):
        expected = version.inspect(resource_executable(), '0.2.0')
        # All leaves share the same VERSIONINFO subtree: ordering alone differs.
        for keys in [[1, 16], ['A', 'a', 16], ['\U00010000', '\ue000', 16]]:
            with self.subTest(ordered=keys):
                self.assertEqual(version.inspect(resource_executable(root_keys=keys), '0.2.0'), expected)
        for keys in [[16, 1], ['a', 'A', 16], ['\ue000', '\U00010000', 16], ['A', 'A', 16], [16, 'A']]:
            with self.subTest(unordered=keys), self.assertRaises(ValueError):
                version.inspect(resource_executable(root_keys=keys), '0.2.0')

    def test_stable_prerelease_build_suffix_and_debug_use_exact_numeric_and_public_metadata(self):
        for text in ['0.2.0', '0.30.0-dev.1', '0.30.0+source', '65535.65535.65535']:
            for debug in [False, True]:
                value = bytearray(version_value(text))
                # Fixed flags are independently found after the UTF-16 root key.
                start = ((6 + len('VS_VERSION_INFO\0'.encode('utf-16le')) + 3) & ~3)
                struct.pack_into('<I', value, start + 7 * 4, (2 if '-' in text.split('+')[0] else 0) | int(debug))
                report = version.inspect(resource_executable(text, value=bytes(value)), text)
                self.assertEqual(report['numeric_version'], version.numeric(text))
                self.assertEqual(report['debug'], debug)
                self.assertEqual(report['prerelease'], '-' in text.split('+')[0])
        for invalid in ['65536.0.0', '0.30', '0.30.0\n', '0.30.0"', '00.30.0']:
            with self.assertRaises(ValueError): version.numeric(invalid)

    def test_missing_duplicate_mismatched_and_invalid_utf16_metadata_refuse(self):
        fields = version.strings('0.2.0')
        cases = [resource_executable(multiple=True), resource_executable(value=version_value(duplicate=True)),
                 resource_executable(value=version_value('0.3.0')),
                 resource_executable(value=version_value(translation=(0x409, 1252))),
                 resource_executable()[:-1]]
        for key in ['CompanyName', 'FileVersion', 'ProductVersion', 'OriginalFilename', 'Comments']:
            changed = dict(fields, **{key:'different'})
            cases.append(resource_executable(value=version_value(fields=changed)))
        changed = bytearray(resource_executable())
        struct.pack_into('<II', changed, 280, 0, 0)
        cases.append(bytes(changed))
        value = version_value().replace('PeerBrush\0'.encode('utf-16le'), b'\0\xd8' + 'eerBrush\0'.encode('utf-16le'), 1)
        cases.append(resource_executable(value=value))
        for data in cases:
            with self.subTest(length=len(data)), self.assertRaises(ValueError): version.inspect(data, '0.2.0')

    def test_invalid_rvas_virtual_overlap_cycles_lengths_padding_and_extra_value_data_refuse(self):
        for offset, data in [(280, struct.pack('<I', 0xfffffff0)), (444, struct.pack('<I', 0x1000)),
                             (1024 + 20, struct.pack('<I', 0x80000000)),
                             (1024 + 72, struct.pack('<I', 0x2000)),
                             (1024 + 76, struct.pack('<I', 65537)),
                             (1024 + 88, struct.pack('<H', 7))]:
            changed = bytearray(resource_executable())
            changed[offset:offset + len(data)] = data
            with self.subTest(offset=offset), self.assertRaises(ValueError): version.inspect(bytes(changed), '0.2.0')
        original = version_value()
        # Corrupt root key-to-fixed-data alignment, then append nonzero garbage.
        changed = bytearray(original)
        changed[38] = 1
        for value in [bytes(changed), original + b'not-a-version-block']:
            with self.assertRaises(ValueError): version.inspect(resource_executable(value=value), '0.2.0')
        for offset, number in [(2, 65535), (4, 3), (40 + 7 * 4, 4)]:
            changed = bytearray(original)
            struct.pack_into('<H', changed, offset, number)
            with self.subTest(value_offset=offset), self.assertRaises(ValueError):
                version.inspect(resource_executable(value=bytes(changed)), '0.2.0')

    def test_source_capability_is_required_and_never_inferred_from_a_report(self):
        data = pe_fixtures.executable()
        self.assertIsNone(version.check(data, '0.2.0', {}, {}))
        for sources, manifest in [({}, dict(windows_version_resource=1)),
                ({version.TEMPLATE:b'committed template'}, {}),
                ({version.TEMPLATE:b'committed template'}, dict(windows_version_resource=True)),
                ({version.TEMPLATE:b'committed template'}, dict(windows_version_resource=1))]:
            with self.assertRaises(ValueError): version.check(data, '0.2.0', sources, manifest)


class ResourcePackages(unittest.TestCase):
    def setUp(self):
        self.fixture = test_msix.Msix()
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.root = self.fixture.root
        f = self.fixture.fixture
        path = self.root / version.TEMPLATE
        path.parent.mkdir(parents=True)
        path.write_bytes((ROOT / version.TEMPLATE).read_bytes())
        f.policy['source'].append(version.TEMPLATE)
        f.save_policy()
        f.commit()
        f.require_provenance()
        self.commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        self.binary = self.root / 'synthetic-binary'
        self.overlay = json.dumps(f.build_info(), separators=(',', ':')).encode()
        self.binary.write_bytes(resource_executable(overlay=self.overlay))
        with contextlib.redirect_stdout(io.StringIO()):
            self.archive = packaging.package(self.root, self.binary, f.output, self.root / 'registry',
                'Windows', verify_binary=False, rust_docs=f.rust_docs)

    def test_actual_package_archive_msix_and_signing_consumers_preserve_the_standard_resource(self):
        inspected = inspect_archive(self.archive, self.commit, '0.2.0', self.root)
        self.assertEqual(inspected['manifest']['windows_version_resource'], 1)
        expected, release = msix.expected_payload(self.root, self.archive, self.fixture.identity, require_provenance=True)
        before = expected['peerbrush.exe']
        after = pe_fixtures.signed_executable(before)
        result = msix.signed_executable_payload(expected, after)
        self.assertEqual(version.inspect(result['peerbrush.exe'], '0.2.0'), version.inspect(before, '0.2.0'))
        changed = bytearray(after)
        location = changed.index('PeerBrush\0'.encode('utf-16le'))
        changed[location] ^= 1
        with self.assertRaises(ValueError): msix.signed_executable_payload(expected, bytes(changed))
        self.assertEqual(release['version'], '0.2.0')

    def test_new_source_cannot_package_wrong_resource_or_hide_missing_capability_metadata(self):
        original = self.archive.read_bytes()
        self.binary.write_bytes(resource_executable('0.3.0', overlay=self.overlay))
        f = self.fixture.fixture
        with self.assertRaises(ValueError):
            packaging.package(self.root, self.binary, f.output, self.root / 'registry',
                'Windows', verify_binary=False, rust_docs=f.rust_docs)
        self.assertEqual(self.archive.read_bytes(), original)
        with zipfile.ZipFile(self.archive) as archive:
            entries = {item.filename:(item, archive.read(item)) for item in archive.infolist()}
        name = next(name for name in entries if name.endswith('/release.json'))
        info, data = entries[name]
        manifest = json.loads(data)
        manifest.pop('windows_version_resource')
        entries[name] = info, json.dumps(manifest).encode()
        with zipfile.ZipFile(self.archive, 'w') as archive:
            for item, data in entries.values(): archive.writestr(item, data)
        with self.assertRaises(ValueError): inspect_archive(self.archive, self.commit, '0.2.0', self.root)
        with self.assertRaises(ValueError): msix.expected_payload(self.root, self.archive, self.fixture.identity)


class NativeResource(unittest.TestCase):
    @unittest.skipUnless(sys.platform == 'win32', 'Actual Windows version APIs require Windows; byte consumers run on every platform')
    def test_actual_source_matched_cargo_binary_metadata_through_windows_version_apis(self):
        import ctypes
        from ctypes import wintypes
        binary = runtime.BINARY.resolve(strict=True)
        text = tomllib.loads((ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
        head = packaging.git(ROOT, 'rev-parse', 'HEAD').decode().strip()
        data = binary.read_bytes()
        self.assertEqual(packaging.binary_build_info(data, text, head)['tree'], 'clean')
        inspected = version.inspect(data, text)
        api = ctypes.WinDLL('version', use_last_error=True)
        api.GetFileVersionInfoSizeW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD)]
        api.GetFileVersionInfoSizeW.restype = wintypes.DWORD
        api.GetFileVersionInfoW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID]
        api.GetFileVersionInfoW.restype = wintypes.BOOL
        api.VerQueryValueW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(wintypes.UINT)]
        api.VerQueryValueW.restype = wintypes.BOOL
        handle = wintypes.DWORD()
        size = api.GetFileVersionInfoSizeW(str(binary), ctypes.byref(handle))
        self.assertTrue(0 < size <= 1024 * 1024)
        buffer = ctypes.create_string_buffer(size)
        self.assertTrue(api.GetFileVersionInfoW(str(binary), 0, size, buffer))
        def query(key):
            pointer, length = ctypes.c_void_p(), wintypes.UINT()
            self.assertTrue(api.VerQueryValueW(buffer, key, ctypes.byref(pointer), ctypes.byref(length)))
            self.assertTrue(pointer.value and 0 < length.value <= size)
            return pointer.value, length.value
        pointer, length = query('\\')
        self.assertEqual(length, 52)
        fixed = struct.unpack('<13I', ctypes.string_at(pointer, length))
        major, minor, patch, revision = version.numeric(text)
        self.assertEqual(fixed[2:6], ((major << 16) | minor, (patch << 16) | revision) * 2)
        self.assertEqual(fixed[7], int(inspected['debug']) | (2 if inspected['prerelease'] else 0))
        for key, expected in version.strings(text).items():
            pointer, _ = query('\\StringFileInfo\\040904B0\\' + key)
            self.assertEqual(ctypes.wstring_at(pointer), expected)
        pointer, length = query('\\VarFileInfo\\Translation')
        self.assertEqual(ctypes.string_at(pointer, length), struct.pack('<HH', 0x409, 1200))
        sdk = os.environ['WindowsSDKVersion'].rstrip('\\/')
        selected = Path(os.environ['WindowsSdkVerBinPath'])
        self.assertIn(sdk, ['10.0.18362.0', '10.0.26100.0'])
        self.assertEqual(selected.name, sdk)
        self.assertEqual(os.environ['VSCMD_ARG_HOST_ARCH'], 'x64')
        tools = {}
        for name in ['rc.exe', 'rcdll.dll']:
            path = selected / 'x64' / name
            self.assertTrue(path.is_file() and not path.is_symlink())
            tools[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        print(json.dumps(dict(format='PeerBrushWindowsResourceNativeV1', source_commit=head, version=text,
            binary_sha256=hashlib.sha256(data).hexdigest(), sdk_version=sdk, selected_tool_sha256=tools,
            windows_api_readback=True, installed_acceptance=False, native_ui_input=False,
            native_producer_authentication='not-established-by-local-test')), flush=True)


if __name__ == '__main__': unittest.main()
