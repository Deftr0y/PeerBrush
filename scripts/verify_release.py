"""Inspect all three real native archives before signing or publication."""
import argparse
import hashlib
import io
import json
import pathlib
import plistlib
import re
import shutil
import zipfile
from package import source_files, portable_files, architecture_from_stream, git, relative_path, validate_binary, equivalent_build_inputs


def sha(data):
    return hashlib.sha256(data).hexdigest()


def verify(directory, commit, version, output, source_root=None):
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Expected full source commit SHA')
    packages = {}
    source_bytes = None
    records = []
    binary_commit = None
    correction = None
    source_root = source_root or pathlib.Path(__file__).resolve().parents[1]
    if git(source_root, 'rev-parse', 'HEAD').decode().strip() != commit:
        raise ValueError('Inspection checkout differs from the selected commit')
    selected_sources = source_files(source_root, commit)
    expected_source = {name: data for name, data, _ in selected_sources}
    expected_portable = {name: data for name, data, _ in portable_files(selected_sources)}
    for path in sorted(directory.rglob(f'PeerBrush-{version}-*.zip')):
        if re.fullmatch(r'PeerBrush-' + re.escape(version) + r'-source(?:-repacked-[0-9]{8})?\.zip', path.name):
            continue
        parsed = re.fullmatch(r'PeerBrush-' + re.escape(version) + r'-(Windows|Linux|macOS)-(x64|arm64)(-repacked-[0-9]{8})?\.zip', path.name)
        if not parsed:
            raise ValueError('Unexpected release archive filename')
        suffix = parsed.group(3) or ''
        if correction is not None and correction != suffix:
            raise ValueError('Platforms must share a packaging correction label')
        correction = suffix
        with zipfile.ZipFile(path) as archive:
            if sum(i.file_size for i in archive.infolist()) > 512 * 1024 * 1024:
                raise ValueError('Release archive exceeds the inspection budget')
            if archive.testzip() is not None:
                raise ValueError('Release archive CRC failure')
            prefix = f'PeerBrush-{version}-{parsed.group(1)}-{parsed.group(2)}/'
            if len(set(archive.namelist())) != len(archive.namelist()):
                raise ValueError('Ambiguous duplicate release entries')
            for item in archive.infolist():
                name = relative_path(item.filename)
                if item.external_attr >> 16 & 0o170000 not in [0, 0o100000]:
                    raise ValueError('Release entries must be regular files')
                if not item.filename.startswith(prefix) or '..' in name.parts or '\\' in item.filename:
                    raise ValueError('Invalid release archive path')
                if name.name in ['connection.json', 'recovery.psd', '.env'] or '.git' in name.parts or '.runtime' in name.parts:
                    raise ValueError('Private runtime material in release archive')
            manifest = json.loads(archive.read(prefix + 'release.json'))
            original_commit = manifest.get('binary_source_commit', manifest['commit'])
            if not re.fullmatch(r'[0-9a-f]{40}', original_commit) or (binary_commit is not None and binary_commit != original_commit):
                raise ValueError('Platforms must share an executable source commit')
            if original_commit != commit:
                equivalent_build_inputs(source_root, selected_sources, original_commit)
            binary_commit = original_commit
            platform = manifest['platform']
            executable_paths = {'Windows': 'peerbrush.exe', 'Linux': 'peerbrush', 'macOS': 'PeerBrush.app/Contents/MacOS/peerbrush'}
            if platform not in executable_paths or manifest['executable'] != executable_paths[platform] or manifest['source'] != f'PeerBrush-{version}-source.zip':
                raise ValueError('Unexpected executable/source path')
            notices = manifest.get('notice_files')
            if not isinstance(notices, dict) or not notices:
                raise ValueError('Missing notice inventory')
            for name, checksum in notices.items():
                if not isinstance(name, str) or not isinstance(checksum, str) or not re.fullmatch(r'[0-9a-f]{64}', checksum) or relative_path(name).parts[0] != 'licenses':
                    raise ValueError('Invalid notice inventory')
                if sha(archive.read(prefix + name)) != checksum:
                    raise ValueError('License notice checksum mismatch')
            expected_entries = set(expected_portable) | set(notices) | {'release.json', 'THIRD_PARTY_NOTICES.md', manifest['executable'], manifest['source']}
            if platform == 'Windows':
                expected_entries.add('Start PeerBrush.cmd')
                if archive.read(prefix + 'Start PeerBrush.cmd') != expected_source['scripts/start-windows.cmd']:
                    raise ValueError('Launcher differs from selected source')
            if platform == 'macOS':
                expected_entries.add('PeerBrush.app/Contents/Info.plist')
            if set(archive.namelist()) != {prefix + name for name in expected_entries}:
                raise ValueError('Unexpected or missing release entries')
            if any(archive.read(prefix + name) != data for name, data in expected_portable.items()):
                raise ValueError('Portable file differs from selected source')
            expected_arch = {'Windows': 'x64', 'Linux': 'x64', 'macOS': 'arm64'}
            if platform not in expected_arch or platform in packages:
                raise ValueError('Duplicate or unsupported release platform')
            if manifest['commit'] != commit or manifest['version'] != version or manifest['architecture'] != expected_arch[platform]:
                raise ValueError('Version, source commit or architecture mismatch')
            if path.name != f'PeerBrush-{version}-{platform}-{expected_arch[platform]}{correction}.zip':
                raise ValueError('Package filename differs from its platform manifest')
            if archive.getinfo(prefix + manifest['executable']).external_attr >> 16 & 0o111 != 0o111:
                raise ValueError('Missing executable permissions')
            if platform == 'macOS':
                info = plistlib.loads(archive.read(prefix + 'PeerBrush.app/Contents/Info.plist'))
                if info.get('CFBundleShortVersionString') != version or info.get('CFBundleExecutable') != 'peerbrush':
                    raise ValueError('macOS app bundle version or executable mismatch')
            if 'GNU GENERAL PUBLIC LICENSE' not in archive.read(prefix + 'LICENSE').decode('utf-8'):
                raise ValueError('Missing GPL license')
            if not any(n.startswith(prefix + 'licenses/') for n in archive.namelist()):
                raise ValueError('Missing third-party license notices')
            executable = archive.read(prefix + manifest['executable'])
            validate_binary(executable)
            if architecture_from_stream(io.BytesIO(executable), platform) != expected_arch[platform]:
                raise ValueError('Executable architecture does not match the release manifest')
            source = archive.read(prefix + manifest['source'])
            if sha(executable) != manifest['executable_sha256'] or sha(source) != manifest['source_sha256']:
                raise ValueError('Executable/source checksum mismatch')
            if source_bytes is not None and source != source_bytes:
                raise ValueError('Platforms contain different source archives')
            source_bytes = source
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
            packages[platform] = path
            records.append({'platform': platform, 'architecture': manifest['architecture'], 'file': path.name, 'bytes': path.stat().st_size, 'sha256': sha(path.read_bytes()), 'executable_sha256': manifest['executable_sha256'], 'binary_source_commit': binary_commit, 'native_publisher_signing': manifest['native_publisher_signing']})
    if set(packages) != {'Windows', 'macOS', 'Linux'}:
        raise ValueError('All three matching platform packages are required')
    output.mkdir(parents=True, exist_ok=True)
    for path in packages.values():
        shutil.copyfile(path, output / path.name)
    source_name = f'PeerBrush-{version}-source{correction}.zip'
    (output / source_name).write_bytes(source_bytes)
    records.append({'file': source_name, 'bytes': len(source_bytes), 'sha256': sha(source_bytes)})
    (output / ('SHA256SUMS' + correction)).write_text(''.join(f"{r['sha256']}  {r['file']}\n" for r in records), encoding='utf-8')
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
