"""Build a dormant fixed-offer Windows setup; never sign, install or publish.

An offer is constructed only through the real source/media/native-origin binders.
No raw offer/ready JSON is accepted as a build or installation permission.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tomllib
from urllib.parse import urlsplit
import zipfile

import build_release
import final_release as final
import jxl_build
import package as packaging
import pe_signing
import windows_version
from package_macos_dmg import bounded_file, private_inspection_directory
from qualify_windows_installer import read_selection
from verify_release import inspect_archive

LIMIT = 512 * 1024 * 1024
SMALL = 16 * 1024
_PREPARED, _BUILT = object(), object()


def encode(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode('ascii')


def measure(data):
    return dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())


class PreparedOffer:
    def __init__(self, root, data, inputs, token):
        if token is not _PREPARED:
            raise ValueError('Setup needs an actual source/native-origin preparation')
        self.root, self.data, self.inputs = root, encode(data), tuple(inputs)
        self.commit, self.version = data['commit'], data['version']
        self.digest = hashlib.sha256(self.data).hexdigest()

    def verify(self):
        final.signed._source(self.root, self.commit)
        if hashlib.sha256(self.data).hexdigest() != self.digest:
            raise ValueError('Prepared setup offer changed')
        for path, limit, checksum in self.inputs:
            if bounded_file(path, limit, executable=path.name.casefold() == 'gh.exe')[0] != checksum:
                raise ValueError('Selected setup/native inputs changed')


def prepare_offer(root, selection):
    required = {'commit', 'version', 'portable', 'portable_sha256', 'windows', 'discovery', 'archive_root'}
    if type(selection) is not dict or set(selection) != required:
        raise ValueError('Complete explicit source, portable, Windows/native and discovery selections required')
    root = Path(root)
    commit, version = selection['commit'], selection['version']
    final.signed._source(root, commit)
    if not re.fullmatch(r'0\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', version):
        raise ValueError('Setup currently requires a stable 0.x application source')
    numeric = final.msix.package_version(version)
    portable = final.readiness.handoff_path(selection['portable'])
    if not final.signed._sha(selection['portable_sha256']) or bounded_file(portable, LIMIT)[0] != selection['portable_sha256']:
        raise ValueError('Setup portable differs from its independent digest')
    inspected = inspect_archive(portable, commit, version, source_root=root)
    base = dict(commit=commit, version=version, assets=[inspected['record']])
    windows = selection['windows']
    if type(windows) is not dict or 'native' not in windows:
        raise ValueError('Setup requires authenticated native executable/package/runtime qualification')
    discovery = final.installer.canonical_uri(selection['discovery'], final.installer.STABLE_DESCRIPTOR)
    archive = selection['archive_root']
    if type(archive) is not str or not archive.endswith('/'):
        raise ValueError('Select the approved immutable Windows directory')
    final.installer.canonical_uri(archive + 'probe.msix', 'probe.msix')
    if urlsplit(discovery).netloc != urlsplit(archive).netloc:
        raise ValueError('Setup discovery/archive selections disagree with exact native media')
    paths, _, inputs, origin = final.windows_binding(root, base, portable.parent, windows, for_setup=True)
    if (not isinstance(origin, dict)
            or origin.get('native_producer_authentication') != 'verified-pinned-workflow-oidc-attestation'
            or origin.get('native_job_conclusion') != 'service-reconciled-success'):
        raise ValueError('Setup native origin has not been authenticated and reconciled')
    package, runtime, descriptor = paths
    identity = json.loads(final.read(Path(windows['identity']), SMALL), object_pairs_hook=final.readiness.unique_fields)
    dependency = final.installer.framework_identity(package, runtime)
    if (windows['descriptor_uri'] != archive + descriptor.name
            or windows['package_uri'] != archive + package.name):
        raise ValueError('Setup discovery/archive selections disagree with exact native media')
    with zipfile.ZipFile(package) as source:
        entries = source.infolist()
        if len(entries) > 16384 or sum(p.file_size for p in entries) > LIMIT:
            raise ValueError('Setup payload inventory exceeds its bound')
        def member(name):
            item = source.getinfo(name)
            if not 0 < item.file_size <= LIMIT: raise ValueError('Setup payload member exceeds its bound')
            with source.open(item) as stream: data = stream.read(LIMIT + 1)
            if len(data) != item.file_size: raise ValueError('Setup payload member changed')
            return data
        executable = member('peerbrush.exe')
        source_name = f'PeerBrush-{version}-source.zip'
        corresponding = member(source_name)
    native = windows['native']
    policy = dict(discovery=discovery, archive_root=archive, name=identity['name'], publisher=identity['publisher'],
        publisher_display=identity['publisher_display_name'], family=windows['package_family'],
        certificate=native['certificate_sha256'], runtime_uri=windows['runtime_uri'],
        runtime_name=dependency['Name'], runtime_publisher=dependency['Publisher'],
        runtime_certificate=native['runtime_certificate_sha256'])
    suffix = windows['package_family'][len(identity['name']) + 1:]
    data = dict(schema=1, format='PeerBrushWindowsSetupOfferV1', commit=commit, version=version, policy=policy,
        selection=dict(descriptor=windows['descriptor_uri'], version=list(map(int, numeric.split('.'))),
            package=windows['package_uri'], runtime_version=list(map(int, dependency['Version'].split('.')))),
        descriptor=dict(bytes=descriptor.stat().st_size, sha256=windows['descriptor_sha256']),
        package=dict(bytes=package.stat().st_size, sha256=windows['package_sha256']),
        runtime=dict(bytes=runtime.stat().st_size, sha256=windows['runtime_sha256']),
        target=dict(full_name=f"{identity['name']}_{numeric}_x64__{suffix}", version=list(map(int, numeric.split('.'))),
            build=commit, executable=measure(executable), source_name=source_name, source=measure(corresponding)))
    if len(encode(data)) > SMALL:
        raise ValueError('Setup public offer exceeds its bound')
    prepared = PreparedOffer(root, data, [*inputs, (portable, LIMIT, selection['portable_sha256'])], _PREPARED)
    prepared.verify()
    return prepared


def imported_libraries(data):
    checksum, _, _, _, _ = pe_signing.layout(data)
    optional = checksum - 64
    pe = struct.unpack_from('<I', data, 60)[0]
    count, size = struct.unpack_from('<H', data, pe + 6)[0], struct.unpack_from('<H', data, pe + 20)[0]
    table = optional + size
    ranges = []
    for index in range(count):
        virtual, address, raw, offset = struct.unpack_from('<IIII', data, table + 40 * index + 8)
        if (virtual or raw) and (not address or address + max(virtual, raw) > 2 ** 32):
            raise ValueError('Setup import section virtual range is invalid')
        if raw:
            ranges.append((address, address + raw, offset))
    ranges.sort()
    if any(a[1] > b[0] for a, b in zip(ranges, ranges[1:])):
        raise ValueError('Overlapping setup import virtual ranges')
    def mapped(address, length):
        matches = [(offset + address - first, offset + end - first) for first, end, offset in ranges
                   if first <= address and address + length <= end]
        if len(matches) != 1:
            raise ValueError('Setup import range is unmapped or ambiguous')
        return matches[0]
    directories = struct.unpack_from('<I', data, optional + 108)[0]
    if directories > 13 and any(struct.unpack_from('<II', data, optional + 112 + 13 * 8)):
        raise ValueError('Setup delay imports require a separately reviewed loader contract')
    address, length = struct.unpack_from('<II', data, optional + 120)
    if not address or not 40 <= length <= 65536:
        raise ValueError('Missing bounded setup import table')
    start, _ = mapped(address, length)
    names = []
    for position in range(start, start + length - 19, 20):
        entry = struct.unpack_from('<5I', data, position)
        if not any(entry):
            if not names:
                raise ValueError('Setup must have an ordinary import table')
            return sorted(names)
        name, end = mapped(entry[3], 1)
        raw = data[name:min(end, name + 256)]
        if b'\0' not in raw:
            raise ValueError('Setup library name exceeds its bound')
        try:
            value = raw.split(b'\0', 1)[0].decode('ascii').lower()
        except UnicodeError:
            raise ValueError('Setup library name is not ASCII') from None
        if (not re.fullmatch(r'[a-z0-9_.-]+\.dll', value) or value in names or len(names) >= 128
                or re.match(r'(?:vcruntime|msvcp|msvcr|concrt|ucrtbase|api-ms-win-crt|ext-ms-win-crt)', value)):
            raise ValueError('Setup has invalid imports or requires the runtime it must install')
        names.append(value)
    raise ValueError('Setup import descriptors are not terminated within their bound')


class BuiltSetup:
    def __init__(self, root, executable, checksum, prepared, report, token):
        if token is not _BUILT:
            raise ValueError('Setup observation requires an actual source build')
        self.root, self.executable, self.checksum, self.prepared, self.report = root, executable, checksum, prepared, report

    def verify(self):
        final.signed._source(self.root, self.report['commit'])
        if bounded_file(self.executable, LIMIT, executable=True)[0] != self.checksum:
            raise ValueError('Owned setup executable changed')
        if self.prepared is not None:
            self.prepared.verify()
        directory = self.executable.parent
        expected = set(self.report['payload']) | {'setup-build.json'}
        parents = {part.as_posix() for name in expected for part in Path(name).parents if part.as_posix() != '.'}
        actual = set()
        for path in directory.rglob('*'):
            if path.is_symlink() or getattr(path.lstat(), 'st_file_attributes', 0) & 0x400:
                raise ValueError('Setup output contains a link/reparse point')
            name = path.relative_to(directory).as_posix()
            if path.is_dir():
                if name not in parents: raise ValueError('Unexpected setup output directory')
            else: actual.add(name)
            if len(actual) > len(expected): raise ValueError('Unexpected setup output inventory')
        if actual != expected: raise ValueError('Setup output inventory changed')
        for name, row in self.report['payload'].items():
            if bounded_file(packaging.regular_file(directory, name), LIMIT, executable=name == 'peerbrush-setup.exe') != (row['sha256'], row['bytes']):
                raise ValueError('Setup source/notices/payload changed')
        if final.read(directory / 'setup-build.json', 512 * 1024) != final.encode(self.report):
            raise ValueError('Setup build report changed')


def verify_installed(built, work):
    if type(built) is not BuiltSetup or built.prepared is None:
        raise ValueError('Installed identity needs an actual fixed-offer source build')
    built.verify()
    private_inspection_directory(work, built.root, fresh=True).mkdir(mode=0o700)
    raw = final.signed._command(built.executable, ['--verify-installed'], Path(work), 'setup-installed-identity')
    if len(raw) > SMALL:
        raise ValueError('Installed identity output exceeds its bound')
    value = json.loads(raw, object_pairs_hook=final.readiness.unique_fields)
    offer = json.loads(built.prepared.data)
    matched = value.get('installed_identity_verified') if type(value) is dict else None
    expected = dict(schema=1, format='PeerBrushWindowsInstalledIdentityV1',
        offer_sha256=hashlib.sha256(built.prepared.data).hexdigest(),
        **{name: offer[name] for name in ['commit', 'version', 'package', 'runtime', 'target']},
        installed_identity_verified=matched, installed_acceptance='not-established', update_eligible=False, ready=False)
    if type(matched) is not bool or encode(value) != encode(expected):
        raise ValueError('Fresh native installed identity differs from the fixed source/media or safety gates')
    built.verify()
    # This is process/source/byte binding only. A website must also authenticate
    # actual native acceptance provenance; neither this JSON nor a true flag is it.
    return value


def build(root, work, output, *, commit, prepared=None, offline=False, jobs=None, registry=None, rust_docs=None):
    root = Path(root).resolve()
    final.signed._source(root, commit)
    if sys.platform != 'win32':
        raise ValueError('Native setup production requires Windows x64/MSVC')
    if prepared is not None:
        if type(prepared) is not PreparedOffer or prepared.root.resolve() != root or prepared.commit != commit:
            raise ValueError('Setup requires an actual matching native/source preparation')
        prepared.verify()
    environment = build_release.release_environment(root)
    environment['CARGO_ENCODED_RUSTFLAGS'] += '\x1f-Ctarget-feature=+crt-static'
    if prepared is not None:
        environment['PEERBRUSH_SETUP_OFFER'] = prepared.data.decode('ascii')
    environment, _ = jxl_build.configure(environment)
    work = private_inspection_directory(work, root, fresh=True)
    output = private_inspection_directory(output, root, fresh=True)
    if work.is_relative_to(output) or output.is_relative_to(work):
        raise ValueError('Setup work and output must be separate fresh private directories')
    if prepared is not None and any(area.is_relative_to(path.parent) for area in [work, output] for path, _, _ in prepared.inputs):
        raise ValueError('Setup outputs must be separate from selected inputs')
    sources = packaging.source_files(root, commit)
    version = tomllib.loads(dict((n, d) for n, d, _ in sources)['Cargo.toml'].decode())['package']['version']
    if prepared is not None and prepared.version != version:
        raise ValueError('Setup source version differs from its fixed offer')
    work.mkdir(mode=0o700)
    command = ['cargo', 'build', '--locked', '--release', '--bin', 'peerbrush-setup', '--features', 'windows-setup',
               '--target', 'x86_64-pc-windows-msvc', '--target-dir', str(work / 'target')]
    if offline: command += ['--offline']
    if jobs is not None:
        if type(jobs) is not int or not 1 <= jobs <= 128: raise ValueError('Bounded positive Cargo jobs required')
        command += ['--jobs', str(jobs)]
    subprocess.run(command, cwd=root, env=environment, check=True)
    binary = packaging.regular_file(work / 'target/x86_64-pc-windows-msvc/release', 'peerbrush-setup.exe')
    checksum, size = bounded_file(binary, LIMIT, executable=True)
    data = binary.read_bytes()
    if len(data) != size or hashlib.sha256(data).hexdigest() != checksum: raise ValueError('Built setup changed')
    pe_signing.unsigned(data)
    imports = imported_libraries(data)
    packaging.validate_binary(data)
    build_info = packaging.binary_build_info(data, version, commit)
    windows_version.inspect(data, version, setup=True)
    before = {p.name for p in work.iterdir()}
    raw = final.signed._command(binary, ['--setup-info'], work, 'setup-info')
    if len(raw) > SMALL: raise ValueError('Setup info exceeds its public bound')
    info = json.loads(raw, object_pairs_hook=final.readiness.unique_fields)
    expected = dict(schema=1, format='PeerBrushWindowsSetupInfoV1', build=build_info,
        offer_sha256=hashlib.sha256(prepared.data).hexdigest() if prepared else None,
        installation_enabled=False, installed_acceptance='not-established', update_eligible=False, ready=False)
    if encode(info) != encode(expected) or {p.name for p in work.iterdir()} != before | {'setup-info.stdout'}:
        raise ValueError('Setup info differs from its source/offer/disabled gate or wrote state')
    final.signed._source(root, commit)
    if bounded_file(binary, LIMIT, executable=True) != (checksum, size): raise ValueError('Built setup changed after observation')
    if prepared is not None: prepared.verify()
    output.mkdir(mode=0o700)
    if bounded_file(binary, LIMIT, output / binary.name, executable=True) != (checksum, size): raise ValueError('Setup copy changed')
    epoch = int(packaging.git(root, 'show', '-s', '--format=%ct', commit).strip())
    source = output / f'PeerBrush-{version}-source.zip'
    with source.open('x+b') as stream:
        packaging.write_zip(stream, sources, epoch, zipfile.ZIP_STORED)
        stream.flush(); os.fsync(stream.fileno()); stream.seek(0)
        source_digest = hashlib.sha256(stream.read()).hexdigest()
    if bounded_file(source, LIMIT)[0] != source_digest: raise ValueError('Setup source output changed')
    staged = work / 'source'; staged.mkdir()
    for name, content, _ in sources:
        target = staged / packaging.relative_path(name); target.parent.mkdir(parents=True, exist_ok=True); target.write_bytes(content)
    policy = packaging.policy_from_sources(sources)
    registry = Path(registry) if registry is not None else Path(environment.get('CARGO_HOME', Path.home() / '.cargo')) / 'registry/src'
    notices = packaging.add_notices(staged, output, registry, policy, rust_docs=rust_docs)
    with (output / 'LICENSE').open('xb') as stream: stream.write(packaging.regular_file(root, 'LICENSE').read_bytes())
    if prepared is not None:
        with (output / 'setup-offer.json').open('xb') as stream: stream.write(prepared.data)
    report = dict(schema=1, format='PeerBrushWindowsSetupBuildV1', commit=commit, version=version,
        executable=dict(file=binary.name, bytes=size, sha256=checksum), build_info=build_info, imported_libraries=imports,
        static_crt_import_check='no-dynamic-crt-imports', source=dict(file=source.name, sha256=source_digest),
        notices=notices, offer_sha256=expected['offer_sha256'], installation_enabled=False,
        signing='unsigned', native_producer_authentication='not-authenticated', installed_acceptance='not-established',
        update_eligible=False, ready=False, production_ready=False)
    expected_files = {binary.name, source.name, 'LICENSE', 'THIRD_PARTY_NOTICES.md'} | set(notices)
    if prepared is not None: expected_files.add('setup-offer.json')
    report['payload'] = {name: dict(zip(['sha256', 'bytes'], bounded_file(packaging.regular_file(output, name), LIMIT, executable=name == 'peerbrush-setup.exe'))) for name in sorted(expected_files)}
    with (output / 'setup-build.json').open('xb') as stream: stream.write(final.encode(report))
    final.signed._source(root, commit)
    result = BuiltSetup(root, output / binary.name, checksum, prepared, report, _BUILT)
    result.verify()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'work', 'output']: parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--selection', type=Path)
    parser.add_argument('--selection-sha256')
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--jobs', type=int)
    args = parser.parse_args()
    try:
        if (args.selection is None) != (args.selection_sha256 is None): raise ValueError('Select both public selection and digest')
        prepared = prepare_offer(args.source_root, read_selection(args.selection, args.selection_sha256)) if args.selection else None
        build(args.source_root, args.work, args.output, commit=args.commit, prepared=prepared, offline=args.offline, jobs=args.jobs)
        if args.selection: read_selection(args.selection, args.selection_sha256)
    except Exception:
        print('Setup build failed or is unavailable; preserve private work. No signing, installation or publication occurred.', file=sys.stderr)
        return 1
    print('Dormant unsigned setup source/CRT/info checks passed. Installed acceptance and production readiness remain held.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
