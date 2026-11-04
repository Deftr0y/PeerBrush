"""Native SDK checks of selected Store bytes; no installation, signing or Store access."""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import stat
import sys
import zipfile

from package import architecture_from_stream, regular_file
from package_macos_dmg import bounded_file, private_inspection_directory
import production_readiness as readiness
import qualify_signed_msix as tools
import qualify_store_msix as content
from verify_macos_qualification import _command as run_native

REPORT = 'store-sdk-qualification.json'
FORMAT = 'PeerBrushStoreSdkQualificationV1'
LIMIT = 16 * 1024
INPUT_FIELDS = {'commit', 'version', 'previous_version', 'portable', 'portable_sha256',
                'package', 'package_sha256', 'report', 'report_sha256',
                'makeappx_sha256', 'makeappx_version'}


def encode(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode('ascii')


def sdk_version(value):
    return (type(value) is str and len(value) <= 23 and re.fullmatch(r'(0|[1-9][0-9]*)(?:\.(0|[1-9][0-9]*)){3}', value)
            and all(int(part) <= 65535 for part in value.split('.')))


def tool_version(path):
    """Read VS_FIXEDFILEINFO through Microsoft's version API, without executing the tool."""
    api = ctypes.WinDLL('version', use_last_error=True)
    size_fn = api.GetFileVersionInfoSizeW
    size_fn.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD)]
    size_fn.restype = wintypes.DWORD
    read_fn = api.GetFileVersionInfoW
    read_fn.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
    read_fn.restype = wintypes.BOOL
    query = api.VerQueryValueW
    query.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(wintypes.UINT)]
    query.restype = wintypes.BOOL
    size = size_fn(str(path), None)
    if not 0 < size <= 1024 * 1024:
        raise ValueError('Selected SDK version resource is absent or oversized.')
    data = ctypes.create_string_buffer(size)
    pointer, length = ctypes.c_void_p(), wintypes.UINT()
    if (not read_fn(str(path), 0, size, data) or not query(data, '\\', ctypes.byref(pointer), ctypes.byref(length))
            or length.value != 13 * ctypes.sizeof(wintypes.DWORD)):
        raise ValueError('Selected SDK fixed version resource cannot be read.')
    fixed = ctypes.cast(pointer, ctypes.POINTER(wintypes.DWORD * 13)).contents
    if fixed[0] != 0xFEEF04BD:
        raise ValueError('Selected SDK fixed version resource differs.')
    return '.'.join(str(value) for value in [fixed[2] >> 16, fixed[2] & 65535, fixed[3] >> 16, fixed[3] & 65535])


def checked_sdk(value, makeappx_sha256, makeappx_version):
    if not sdk_version(makeappx_version):
        raise ValueError('Independently selected canonical SDK tool version required.')
    path = tools._selected_tool(content.store.msix.find_makeappx(value), makeappx_sha256, 'makeappx.exe')
    with path.open('rb') as stream:
        architecture_from_stream(stream, 'Windows')
    if tool_version(path) != makeappx_version:
        raise ValueError('Selected SDK tool version differs from its independent pin.')
    return path


def report_for(checked, *, makeappx_sha256, makeappx_version):
    if not tools._sha(makeappx_sha256) or not sdk_version(makeappx_version):
        raise ValueError('Exact independently selected native SDK tool pins required.')
    return dict(schema=1, format=FORMAT, commit=checked['commit'], version=checked['version'],
        content=checked, sdk=dict(tool='MakeAppx.exe', sha256=makeappx_sha256, version=makeappx_version,
            command='unpack', semantic_validation='enabled-limited-sdk-validation', payload='exact-source-derived-bytes'),
        native_producer_authentication='unverified-requires-trusted-native-job',
        installed_acceptance='not-performed', store_submission='not-performed',
        public_availability='unverified', ready=False, production_ready=False)


def verify_unpacked(unpacked, expected, root):
    private_inspection_directory(unpacked, root, fresh=False)
    names, directories = set(), set()
    allowed_directories = {str(parent).replace('\\', '/') for name in expected for parent in Path(name).parents if str(parent) != '.'}
    for parent, children, files in os.walk(unpacked, followlinks=False):
        for name in children + files:
            path = Path(parent) / name
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
                raise ValueError('SDK extracted links or reparse points are refused.')
            relative = path.relative_to(unpacked).as_posix()
            if name in children:
                directories.add(relative)
            else:
                names.add(relative)
                path = regular_file(unpacked, relative)
                executable = path.suffix.casefold() in {'.exe', '.com', '.cmd', '.bat'}
                if relative not in expected or bounded_file(path, content.store.msix.PACKAGE_LIMIT, executable=executable) != (
                        hashlib.sha256(expected[relative]).hexdigest(), len(expected[relative])):
                    raise ValueError('SDK extracted payload differs from exact selected bytes.')
    if names != set(expected) or directories != allowed_directories:
        raise ValueError('SDK extracted inventory differs from the complete source-derived payload.')


def qualify(root, output, work, *, portable, package, report, commit, version, previous_version,
            portable_sha256, package_sha256, report_sha256, makeappx_sha256, makeappx_version, makeappx=None):
    if sys.platform != 'win32' or platform.machine().lower() not in {'amd64', 'x86_64'}:
        raise ValueError('Store SDK qualification requires native Windows x64.')
    output = private_inspection_directory(output, root, fresh=True)
    work = private_inspection_directory(work, root, fresh=True)
    if output.is_relative_to(work) or work.is_relative_to(output):
        raise ValueError('Native SDK output and work must be separate fresh private directories.')
    options = dict(commit=commit, version=version, previous_version=previous_version,
        portable_sha256=portable_sha256, package_sha256=package_sha256, report_sha256=report_sha256)
    checked = content.qualify(root, portable, package, report, **options)
    sdk = checked_sdk(makeappx, makeappx_sha256, makeappx_version)
    inputs = [(Path(portable), content.store.msix.PACKAGE_LIMIT, portable_sha256),
              (Path(package), content.store.msix.PACKAGE_LIMIT, package_sha256), (Path(report), 65536, report_sha256)]
    if any(area.is_relative_to(path.parent) for area in [work, output] for path, _, _ in inputs):
        raise ValueError('Native SDK work/output must be separate from selected inputs.')
    work.mkdir(mode=0o700)
    work.chmod(0o700)
    frozen = []
    for path, limit, checksum in inputs:
        owned = work / path.name
        if bounded_file(path, limit, owned)[0] != checksum:
            raise ValueError('Selected SDK input changed before its owned copy.')
        owned.chmod(0o400)
        frozen.append(owned)
    producer = json.loads(frozen[2].read_bytes(), object_pairs_hook=readiness.unique_fields)
    expected, _ = content.store.msix.expected_payload(root, frozen[0], producer['identity'],
        msix_version=checked['package_version'], require_provenance=True)
    with zipfile.ZipFile(frozen[1]) as archive:
        expected['AppxBlockMap.xml'] = archive.read('AppxBlockMap.xml')
    unpacked = work / 'unpacked'
    # /nv is deliberately absent. /no refuses an unexpected existing destination.
    for name in ['home', 'cache', 'config']:
        (work / name).mkdir(mode=0o700)
    run_native(sdk, ['unpack', '/p', str(frozen[1]), '/d', str(unpacked), '/no'], work, 'makeappx-unpack', _windows_host=True)
    verify_unpacked(unpacked, expected, root)

    def unchanged():
        tools._source(root, commit)
        checked_sdk(sdk, makeappx_sha256, makeappx_version)
        for (original, limit, checksum), owned in zip(inputs, frozen):
            if bounded_file(original, limit)[0] != checksum or bounded_file(owned, limit)[0] != checksum:
                raise ValueError('Native SDK original or owned input changed.')
        if content.qualify(root, *frozen, **options) != checked:
            raise ValueError('Owned SDK input content differs after native validation.')
        verify_unpacked(unpacked, expected, root)
    unchanged()
    result = report_for(checked, makeappx_sha256=makeappx_sha256, makeappx_version=makeappx_version)
    data = encode(result)
    if len(data) > LIMIT:
        raise ValueError('Native SDK report exceeds its bounded schema.')
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    private_inspection_directory(output, root, fresh=False)
    with (output / REPORT).open('xb') as stream:
        stream.write(data)
    unchanged()
    if bounded_file(output / REPORT, LIMIT)[0] != hashlib.sha256(data).hexdigest():
        raise ValueError('Retained SDK report changed.')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'output', 'work', 'selection']:
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--selection-sha256', required=True)
    args = parser.parse_args()
    try:
        raw = content.selected(args.selection, args.selection_sha256, 32768)[1]
        selection = json.loads(raw, object_pairs_hook=readiness.unique_fields)
        if not isinstance(selection, dict) or set(selection) != INPUT_FIELDS:
            raise ValueError('Exact native SDK input selection required.')
        qualify(args.source_root, args.output, args.work, **selection)
        if content.selected(args.selection, args.selection_sha256, 32768)[1] != raw:
            raise ValueError('Native SDK selection changed.')
    except Exception:
        print('Store native SDK qualification failed; no installation, authentication or publication occurred.', file=sys.stderr)
        return 1
    print('Store SDK bytes qualified; installed acceptance, authenticated origin and public availability remain separate.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
