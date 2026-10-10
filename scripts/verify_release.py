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
from package import source_files, architecture_from_stream, git


def sha(data):
    return hashlib.sha256(data).hexdigest()


def verify(directory, commit, version, output, source_root=None):
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Expected full source commit SHA')
    packages = {}
    source_bytes = None
    records = []
    source_root = source_root or pathlib.Path(__file__).resolve().parents[1]
    if git(source_root, 'rev-parse', 'HEAD').decode().strip() != commit:
        raise ValueError('Inspection checkout differs from the selected commit')
    expected_source = {name: data for name, data, _ in source_files(source_root)}
    for path in sorted(directory.rglob(f'PeerBrush-{version}-*.zip')):
        if path.name.endswith('-source.zip'):
            continue
        with zipfile.ZipFile(path) as archive:
            if sum(i.file_size for i in archive.infolist()) > 512 * 1024 * 1024:
                raise ValueError('Release archive exceeds the inspection budget')
            if archive.testzip() is not None:
                raise ValueError('Release archive CRC failure')
            prefix = path.stem + '/'
            if len(set(archive.namelist())) != len(archive.namelist()):
                raise ValueError('Ambiguous duplicate release entries')
            for item in archive.infolist():
                name = pathlib.PurePosixPath(item.filename)
                if not item.filename.startswith(prefix) or '..' in name.parts or '\\' in item.filename:
                    raise ValueError('Invalid release archive path')
                if name.name in ['connection.json', 'recovery.psd', '.env'] or '.git' in name.parts or '.runtime' in name.parts:
                    raise ValueError('Private runtime material in release archive')
            manifest = json.loads(archive.read(prefix + 'release.json'))
            platform = manifest['platform']
            expected_arch = {'Windows': 'x64', 'Linux': 'x64', 'macOS': 'arm64'}
            if platform not in expected_arch or platform in packages:
                raise ValueError('Duplicate or unsupported release platform')
            if manifest['commit'] != commit or manifest['version'] != version or manifest['architecture'] != expected_arch[platform]:
                raise ValueError('Version, source commit or architecture mismatch')
            if path.name != f'PeerBrush-{version}-{platform}-{expected_arch[platform]}.zip':
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
            if architecture_from_stream(io.BytesIO(executable), platform) != expected_arch[platform]:
                raise ValueError('Executable architecture does not match the release manifest')
            source = archive.read(prefix + manifest['source'])
            if sha(executable) != manifest['executable_sha256'] or sha(source) != manifest['source_sha256']:
                raise ValueError('Executable/source checksum mismatch')
            if source_bytes is not None and source != source_bytes:
                raise ValueError('Platforms contain different source archives')
            source_bytes = source
            with zipfile.ZipFile(io.BytesIO(source)) as sources:
                if sources.testzip() is not None or 'Cargo.lock' not in sources.namelist():
                    raise ValueError('Invalid matching source archive')
                if any('.runtime' in pathlib.PurePosixPath(n).parts or '.git' in pathlib.PurePosixPath(n).parts for n in sources.namelist()):
                    raise ValueError('Private source entry')
                if len(set(sources.namelist())) != len(sources.namelist()) or set(sources.namelist()) != set(expected_source):
                    raise ValueError('Source archive inventory differs from the selected commit')
                if any(sources.read(name) != data for name, data in expected_source.items()):
                    raise ValueError('Source archive contents differ from the selected commit')
            packages[platform] = path
            records.append({'platform': platform, 'architecture': manifest['architecture'], 'file': path.name, 'bytes': path.stat().st_size, 'sha256': sha(path.read_bytes()), 'executable_sha256': manifest['executable_sha256'], 'native_publisher_signing': manifest['native_publisher_signing']})
    if set(packages) != {'Windows', 'macOS', 'Linux'}:
        raise ValueError('All three matching platform packages are required')
    output.mkdir(parents=True, exist_ok=True)
    for path in packages.values():
        shutil.copyfile(path, output / path.name)
    source_name = f'PeerBrush-{version}-source.zip'
    (output / source_name).write_bytes(source_bytes)
    records.append({'file': source_name, 'bytes': len(source_bytes), 'sha256': sha(source_bytes)})
    (output / 'SHA256SUMS').write_text(''.join(f"{r['sha256']}  {r['file']}\n" for r in records), encoding='utf-8')
    (output / 'release-inventory.json').write_text(json.dumps({'schema': 1, 'version': version, 'commit': commit, 'assets': records}, indent=2) + '\n', encoding='utf-8')
    print(f'Verified three matching PeerBrush {version} archives and source from {commit}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--directory', type=pathlib.Path, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    verify(args.directory, args.commit, args.version, args.output)
