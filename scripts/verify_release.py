"""Inspect release archives. Structural checks do not replace native signature checks."""
import argparse
import hashlib
import io
import json
import pathlib
import plistlib
import re
import shutil
import stat
import zipfile
from package import source_files, portable_files, architecture_from_stream, git, relative_path, validate_binary, equivalent_build_inputs, policy_from_sources, MACOS_RESOURCES, macos_info, binary_build_info, macos_sparkle_required


MACOS_SIGNING_FILES = frozenset({
    'PeerBrush.app/Contents/_CodeSignature/CodeResources',
    'PeerBrush.app/Contents/CodeResources',
})
MACOS_SIGNING_KIND = 'developer-id-notarized'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def validate_signing_metadata(manifest, archive, prefix, suffix):
    """Allow two exact Apple-generated files, never an arbitrary metadata tree.

    Hash/manifest validation is portable; it is not cryptographic proof of Apple
    signing. sign_macos.py additionally runs the native Apple checks on the final
    extracted archive. Do not infer Gatekeeper acceptance from this function.
    """
    signing = manifest.get('macos_signing')
    kind = manifest.get('native_publisher_signing')
    if signing is None:
        if suffix or kind != 'not-configured':
            raise ValueError('Missing or inconsistent native signing metadata')
        return set()
    if manifest.get('platform') != 'macOS' or kind != MACOS_SIGNING_KIND:
        raise ValueError('Unexpected native signing metadata')
    fields = {'schema', 'revision', 'team_id', 'certificate_sha1',
              'unsigned_archive_sha256', 'unsigned_executable_sha256',
              'submission_archive_sha256', 'notary_submission_id',
              'notary_status', 'hardened_runtime', 'secure_timestamp',
              'stapled', 'metadata_files'}
    if not isinstance(signing, dict) or set(signing) != fields or type(signing['schema']) is not int or signing['schema'] != 1:
        raise ValueError('Unsupported macOS signing metadata')
    if not re.fullmatch(r'apple-signed-r[1-9][0-9]*', str(signing['revision'])) or suffix != '-' + signing['revision']:
        raise ValueError('macOS signing revision mismatch')
    if not re.fullmatch(r'[A-Z0-9]{10}', str(signing['team_id'])) or not re.fullmatch(r'[A-F0-9]{40}', str(signing['certificate_sha1'])):
        raise ValueError('Invalid macOS signing identity metadata')
    for field in ['unsigned_archive_sha256', 'unsigned_executable_sha256', 'submission_archive_sha256']:
        if not re.fullmatch(r'[0-9a-f]{64}', str(signing[field])):
            raise ValueError('Invalid macOS signing provenance checksum')
    if not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', str(signing['notary_submission_id'])):
        raise ValueError('Invalid notarization submission identifier')
    if signing['notary_status'] != 'Accepted' or any(signing[field] is not True for field in ['hardened_runtime', 'secure_timestamp', 'stapled']):
        raise ValueError('Incomplete macOS signing/notarization metadata')
    files = signing['metadata_files']
    if not isinstance(files, dict) or set(files) != MACOS_SIGNING_FILES:
        raise ValueError('Unexpected macOS signing file inventory')
    for name, checksum in files.items():
        if not isinstance(checksum, str) or not re.fullmatch(r'[0-9a-f]{64}', checksum) or sha(archive.read(prefix + name)) != checksum:
            raise ValueError('macOS signing metadata checksum mismatch')
        if not 0 < archive.getinfo(prefix + name).file_size <= 4 * 1024 * 1024:
            raise ValueError('Invalid macOS signing metadata size')
    return set(files)


def inspect_archive(path, commit, version, source_root=None, selected_sources=None):
    """Inspect one archive against committed source without extracting or executing it."""
    path = pathlib.Path(path)
    source_root = source_root or pathlib.Path(__file__).resolve().parents[1]
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Expected full source commit SHA')
    if git(source_root, 'rev-parse', 'HEAD').decode().strip() != commit:
        raise ValueError('Inspection checkout differs from the selected commit')
    selected_sources = selected_sources or source_files(source_root, commit)
    expected_source = {name: data for name, data, _ in selected_sources}
    expected_portable = {name: data for name, data, _ in portable_files(selected_sources)}
    parsed = re.fullmatch(r'PeerBrush-' + re.escape(version) + r'-(Windows|Linux|macOS)-(x64|arm64)(-repacked-[0-9]{8})?(-apple-signed-r[1-9][0-9]*)?\.zip', path.name)
    if not parsed:
        raise ValueError('Unexpected release archive filename')
    suffix, signing_suffix = parsed.group(3) or '', parsed.group(4) or ''
    with zipfile.ZipFile(path) as archive:
        if sum(i.file_size for i in archive.infolist()) > 512 * 1024 * 1024:
            raise ValueError('Release archive exceeds the inspection budget')
        if archive.testzip() is not None:
            raise ValueError('Release archive CRC failure')
        prefix = f'PeerBrush-{version}-{parsed.group(1)}-{parsed.group(2)}/'
        if len({name.casefold() for name in archive.namelist()}) != len(archive.namelist()):
            raise ValueError('Ambiguous duplicate release entries')
        for item in archive.infolist():
            name = relative_path(item.filename)
            if item.external_attr >> 16 & 0o170000 == 0o120000:
                from sparkle_bundle import validate_link
                validate_link(item.filename[len(prefix):], archive.read(item), expected_source)
            elif item.external_attr >> 16 & 0o170000 not in [0, 0o100000]:
                raise ValueError('Release entries must be regular files or exact pinned Sparkle links')
            if not item.filename.startswith(prefix) or '..' in name.parts or '\\' in item.filename:
                raise ValueError('Invalid release archive path')
            if name.name in ['connection.json', 'recovery.psd', '.env'] or '.git' in name.parts or '.runtime' in name.parts:
                raise ValueError('Private runtime material in release archive')
        manifest = json.loads(archive.read(prefix + 'release.json'))
        if manifest.get('schema') != 1 or manifest.get('product') != 'PeerBrush' or manifest.get('license') != 'GPL-3.0-only':
            raise ValueError('Unsupported release manifest')
        original_commit = manifest.get('binary_source_commit', manifest['commit'])
        if not re.fullmatch(r'[0-9a-f]{40}', original_commit):
            raise ValueError('Expected full executable source commit')
        if original_commit != commit:
            equivalent_build_inputs(source_root, selected_sources, original_commit)
        platform = manifest['platform']
        executable_paths = {'Windows': 'peerbrush.exe', 'Linux': 'peerbrush', 'macOS': 'PeerBrush.app/Contents/MacOS/peerbrush'}
        if platform not in executable_paths or manifest['executable'] != executable_paths[platform] or manifest['source'] != f'PeerBrush-{version}-source.zip':
            raise ValueError('Unexpected executable/source path')
        notices = manifest.get('notice_files')
        if not isinstance(notices, dict) or not notices:
            raise ValueError('Missing notice inventory')
        required_rust = {'licenses/rust-runtime/' + name for name in policy_from_sources(selected_sources)['rust_notices']}
        if not required_rust <= set(notices):
            raise ValueError('Missing required Rust runtime notices')
        for name, checksum in notices.items():
            if not isinstance(name, str) or not isinstance(checksum, str) or not re.fullmatch(r'[0-9a-f]{64}', checksum) or relative_path(name).parts[0] != 'licenses':
                raise ValueError('Invalid notice inventory')
            if sha(archive.read(prefix + name)) != checksum:
                raise ValueError('License notice checksum mismatch')
            if name in required_rust and archive.getinfo(prefix + name).file_size == 0:
                raise ValueError('Required Rust runtime notice is empty')
        expected_entries = set(expected_portable) | set(notices) | {'release.json', 'THIRD_PARTY_NOTICES.md', manifest['executable'], manifest['source']}
        if platform == 'Windows':
            expected_entries.add('Start PeerBrush.cmd')
            if archive.read(prefix + 'Start PeerBrush.cmd') != expected_source['scripts/start-windows.cmd']:
                raise ValueError('Launcher differs from selected source')
        if platform == 'macOS':
            expected_entries.add('PeerBrush.app/Contents/Info.plist')
            if 'assets/peerbrush.icns' in expected_source and manifest.get('macos_bundle_resources') != 1:
                raise ValueError('Missing required installed macOS resources')
            if manifest.get('macos_bundle_resources') is not None:
                if manifest['macos_bundle_resources'] != 1:
                    raise ValueError('Unsupported macOS bundle resource layout')
                expected_entries.update(MACOS_RESOURCES + name for name in ['PeerBrush.icns', 'LICENSE', 'THIRD_PARTY_NOTICES.md', *notices])
        sparkle_required = platform == 'macOS' and macos_sparkle_required(selected_sources)
        if sparkle_required != (type(manifest.get('macos_sparkle_layout')) is int and manifest.get('macos_sparkle_layout') == 1):
            raise ValueError('macOS Sparkle layout must match the exact committed source contract')
        if manifest.get('macos_sparkle_layout') is not None:
            if not sparkle_required or manifest['macos_sparkle_layout'] != 1:
                raise ValueError('Unsupported macOS Sparkle layout')
            from sparkle_bundle import inspect_members, NOTICE
            expected_entries.update(inspect_members(archive, prefix, expected_source, bool(signing_suffix)))
            if notices.get(NOTICE) != sha(expected_source['packaging/sparkle/LICENSE']):
                raise ValueError('Missing exact Sparkle upstream/external notice')
        expected_entries.update(validate_signing_metadata(manifest, archive, prefix, signing_suffix))
        if set(archive.namelist()) != {prefix + name for name in expected_entries}:
            raise ValueError('Unexpected or missing release entries')
        if any(archive.read(prefix + name) != data for name, data in expected_portable.items()):
            raise ValueError('Portable file differs from selected source')
        expected_arch = {'Windows': 'x64', 'Linux': 'x64', 'macOS': 'arm64'}
        if platform not in expected_arch:
            raise ValueError('Duplicate or unsupported release platform')
        if manifest['commit'] != commit or manifest['version'] != version or manifest['architecture'] != expected_arch[platform]:
            raise ValueError('Version, source commit or architecture mismatch')
        if path.name != f'PeerBrush-{version}-{platform}-{expected_arch[platform]}{suffix}{signing_suffix}.zip':
            raise ValueError('Package filename differs from its platform manifest')
        if archive.getinfo(prefix + manifest['executable']).external_attr >> 16 & 0o111 != 0o111:
            raise ValueError('Missing executable permissions')
        if platform == 'macOS':
            info = plistlib.loads(archive.read(prefix + 'PeerBrush.app/Contents/Info.plist'))
            short_version = macos_info(version)['CFBundleShortVersionString'] if manifest.get('macos_bundle_resources') == 1 else version
            if info.get('CFBundleShortVersionString') != short_version or info.get('CFBundleExecutable') != 'peerbrush' or info.get('CFBundleIdentifier') != 'com.peerbrush.PeerBrush' or info.get('CFBundlePackageType') != 'APPL':
                raise ValueError('macOS app bundle version or executable mismatch')
            if manifest.get('macos_bundle_resources') == 1:
                from sparkle_bundle import CHECK_MARKER, check_pins
                pins = check_pins(expected_source, version) if sparkle_required else None
                check_layout = CHECK_MARKER in expected_source.get('src/sparkle.rs', b'')
                if info != macos_info(version, sparkle=sparkle_required, check_pins=pins, check_layout=check_layout):
                    raise ValueError('macOS document consent, icon or bundle metadata differs from source')
                if archive.read(prefix + MACOS_RESOURCES + 'PeerBrush.icns') != expected_source['assets/peerbrush.icns']:
                    raise ValueError('macOS icon differs from committed artwork')
                for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md', *notices]:
                    if archive.read(prefix + MACOS_RESOURCES + name) != archive.read(prefix + name):
                        raise ValueError('macOS installed license/notices differ from the verified distribution')
        if 'GNU GENERAL PUBLIC LICENSE' not in archive.read(prefix + 'LICENSE').decode('utf-8'):
            raise ValueError('Missing GPL license')
        if not any(n.startswith(prefix + 'licenses/') for n in archive.namelist()):
            raise ValueError('Missing third-party license notices')
        executable = archive.read(prefix + manifest['executable'])
        validate_binary(executable)
        if platform == 'Windows':
            from windows_version import check
            check(executable, version, selected_sources, manifest)
        if policy_from_sources(selected_sources)['version'] == 2:
            info = binary_build_info(executable, version, original_commit)
            if manifest.get('build_info') != info:
                raise ValueError('Release manifest differs from actual embedded build provenance')
        if architecture_from_stream(io.BytesIO(executable), platform) != expected_arch[platform]:
            raise ValueError('Executable architecture does not match the release manifest')
        source = archive.read(prefix + manifest['source'])
        if sha(executable) != manifest['executable_sha256'] or sha(source) != manifest['source_sha256']:
            raise ValueError('Executable/source checksum mismatch')
        with zipfile.ZipFile(io.BytesIO(source)) as sources:
            if sum(i.file_size for i in sources.infolist()) > 512 * 1024 * 1024:
                raise ValueError('Source archive exceeds the inspection budget')
            if sources.testzip() is not None or 'Cargo.lock' not in sources.namelist():
                raise ValueError('Invalid matching source archive')
            if any('.runtime' in pathlib.PurePosixPath(n).parts or '.git' in pathlib.PurePosixPath(n).parts for n in sources.namelist()):
                raise ValueError('Private source entry')
            if len(set(sources.namelist())) != len(sources.namelist()) or set(sources.namelist()) != set(expected_source):
                raise ValueError('Source archive inventory differs from the selected commit')
            if any(sources.read(name) != data for name, data in expected_source.items()):
                raise ValueError('Source archive contents differ from the selected commit')
        return {'manifest': manifest, 'prefix': prefix, 'source': source,
                'packaging_correction': suffix, 'signing_suffix': signing_suffix,
                'record': {'platform': platform, 'architecture': manifest['architecture'],
                           'file': path.name, 'bytes': path.stat().st_size,
                           'sha256': sha(path.read_bytes()),
                           'executable_sha256': manifest['executable_sha256'],
                           'source_sha256': manifest['source_sha256'],
                           'binary_source_commit': original_commit,
                           'native_publisher_signing': manifest['native_publisher_signing'],
                           'signing_verification': 'metadata-and-hashes-only' if signing_suffix else 'unsigned'}}


def verify(directory, commit, version, output, source_root=None):
    packages, records = {}, []
    source_bytes = binary_commit = correction = None
    source_root = source_root or pathlib.Path(__file__).resolve().parents[1]
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Expected full source commit SHA')
    if git(source_root, 'rev-parse', 'HEAD').decode().strip() != commit:
        raise ValueError('Inspection checkout differs from the selected commit')
    selected_sources = source_files(source_root, commit)
    for path in sorted(directory.rglob(f'PeerBrush-{version}-*.zip')):
        if re.fullmatch(r'PeerBrush-' + re.escape(version) + r'-source(?:-repacked-[0-9]{8})?\.zip', path.name):
            continue
        inspected = inspect_archive(path, commit, version, source_root, selected_sources)
        manifest, record = inspected['manifest'], inspected['record']
        platform = manifest['platform']
        if platform in packages:
            raise ValueError('Duplicate or unsupported release platform')
        if correction is not None and correction != inspected['packaging_correction']:
            raise ValueError('Platforms must share a packaging correction label')
        if binary_commit is not None and binary_commit != record['binary_source_commit']:
            raise ValueError('Platforms must share an executable source commit')
        if source_bytes is not None and source_bytes != inspected['source']:
            raise ValueError('Platforms contain different source archives')
        correction = inspected['packaging_correction']
        binary_commit = record['binary_source_commit']
        source_bytes = inspected['source']
        packages[platform] = path
        records.append(record)
    if set(packages) != {'Windows', 'macOS', 'Linux'}:
        raise ValueError('All three matching platform packages are required')
    output.mkdir(parents=True, exist_ok=True)
    for path in packages.values():
        shutil.copyfile(path, output / path.name)
    source_name = f'PeerBrush-{version}-source{correction}.zip'
    (output / source_name).write_bytes(source_bytes)
    records.append({'file': source_name, 'bytes': len(source_bytes), 'sha256': sha(source_bytes)})
    (output / ('SHA256SUMS' + correction)).write_text(''.join(f"{r['sha256']}  {r['file']}\n" for r in records), encoding='utf-8', newline='\n')
    (output / ('release-inventory' + correction + '.json')).write_text(json.dumps({'schema': 1, 'version': version, 'commit': commit, 'binary_source_commit': binary_commit, 'packaging_correction': correction or None, 'assets': records}, indent=2) + '\n', encoding='utf-8')
    print(f'Verified three matching PeerBrush {version} archives and source from {commit}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--directory', type=pathlib.Path, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    verify(args.directory, args.commit, args.version, args.output)
