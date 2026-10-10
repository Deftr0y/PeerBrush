"""Package a committed native build, matching GPL source and license notices."""
import argparse
import hashlib
import io
import json
import os
import pathlib
import plistlib
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import tomllib
import zipfile


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args])


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def binary_architecture(path, platform):
    with path.open('rb') as stream:
        return architecture_from_stream(stream, platform)


def architecture_from_stream(stream, platform):
    header = stream.read(64)
    if platform == 'Windows' and len(header) >= 64 and header[:2] == b'MZ':
        stream.seek(struct.unpack_from('<I', header, 60)[0])
        pe = stream.read(6)
        if pe[:4] == b'PE\0\0' and pe[4:] == b'\x64\x86':
            return 'x64'
    elif platform == 'Linux' and len(header) >= 20 and header[:6] == b'\x7fELF\x02\x01':
        if struct.unpack_from('<H', header, 18)[0] == 62:
            return 'x64'
    elif platform == 'macOS' and len(header) >= 8 and header[:4] == b'\xcf\xfa\xed\xfe':
        cpu = struct.unpack_from('<I', header, 4)[0]
        if cpu in [0x1000007, 0x100000c]:
            return 'x64' if cpu == 0x1000007 else 'arm64'
    raise ValueError('Unsupported or mismatched native executable architecture')


def write_zip(path, files, epoch, compression=zipfile.ZIP_DEFLATED):
    stamp = time.gmtime(max(epoch, 315532800))[:6]
    with zipfile.ZipFile(path, 'w', compression, compresslevel=9) as archive:
        for name, data, mode in sorted(files):
            info = zipfile.ZipInfo(name, stamp)
            info.create_system = 3
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = compression
            archive.writestr(info, data)


def source_files(root):
    roots = {'src', 'tests', 'scripts', 'docs', 'assets', '.github', 'vendor', 'examples'}
    top = {'Cargo.toml', 'Cargo.lock', 'LICENSE', 'README.md', 'AGENTS.md', 'FOLLOWUPS.MD', '.gitignore'}
    entries = []
    for row in git(root, 'ls-tree', '-rz', 'HEAD').split(b'\0'):
        if not row:
            continue
        meta, encoded = row.split(b'\t', 1)
        mode, kind, object_id = meta.split()
        name = encoded.decode('utf-8')
        path = pathlib.PurePosixPath(name)
        if name not in top and path.parts[0] not in roots:
            continue
        if kind != b'blob' or mode not in [b'100644', b'100755']:
            raise ValueError(f'Unsupported source entry: {name}')
        if path.name in ['development-handoff.md', 'outreach-plan.md'] or '__pycache__' in path.parts:
            continue
        entries.append((name, object_id, int(mode, 8) & 0o777))
    # One batch reads committed blobs; untracked checkout/QA files never enter.
    output = subprocess.check_output(['git', '-C', str(root), 'cat-file', '--batch'], input=b'\n'.join(obj for _, obj, _ in entries) + b'\n')
    stream = io.BytesIO(output)
    files = []
    for name, object_id, mode in entries:
        actual, kind, size = stream.readline().split()
        if actual != object_id or kind != b'blob':
            raise ValueError('Source object mismatch')
        data = stream.read(int(size))
        if len(data) != int(size) or stream.read(1) != b'\n':
            raise ValueError('Truncated source object')
        files.append((name, data, mode))
    return files


def add_notices(root, bundle, registry):
    notices = bundle / 'licenses'
    notices.mkdir()
    rows = ['# Third-party notices', '', 'Dependencies retain their original licenses. Sources and checksums are pinned by Cargo.lock in the included source archive. Downloaded platform dependencies may include crates not linked into this binary.', '']
    lock = tomllib.loads((root / 'Cargo.lock').read_text(encoding='utf-8'))
    for package in lock['package']:
        if 'source' not in package:
            continue
        key = f"{package['name']}-{package['version']}"
        matches = list(registry.glob('*/' + key))
        if not matches:
            continue
        crate = matches[0]
        meta = tomllib.loads((crate / 'Cargo.toml').read_text(encoding='utf-8'))['package']
        rows.append(f"- {key}: {meta.get('license', meta.get('license-file', 'See source license'))}")
        candidates = [p for p in crate.iterdir() if p.is_file() and p.name.upper().startswith(('LICENSE', 'COPYING', 'NOTICE', 'UNLICENSE'))]
        if (crate / 'fonts').exists():
            candidates.extend((crate / 'fonts').rglob('*.txt'))
        if meta.get('license-file'):
            candidates.append(crate / meta['license-file'])
        for path in sorted(set(candidates)):
            if path.is_file():
                dest = notices / key / path.relative_to(crate)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, dest)
    for crate in sorted((root / 'vendor').iterdir()):
        meta = tomllib.loads((crate / 'Cargo.toml').read_text(encoding='utf-8'))['package']
        key = f"{meta['name']}-{meta['version']}"
        rows.append(f"- {key} (vendored input patch): {meta.get('license', 'See source license')}")
        for path in crate.glob('LICENSE*'):
            dest = notices / key / path.name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, dest)
    shutil.copytree(bundle / 'assets' / 'fonts', notices / 'ubuntu-sans')
    (bundle / 'THIRD_PARTY_NOTICES.md').write_text('\n'.join(rows) + '\n', encoding='utf-8')


def package(root, binary, output, registry, platform, verify_binary=True):
    if git(root, 'status', '--porcelain', '--untracked-files=no').strip():
        raise ValueError('Commit tracked changes before packaging a release')
    version = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    commit = git(root, 'rev-parse', 'HEAD').decode().strip()
    epoch = int(git(root, 'show', '-s', '--format=%ct', 'HEAD'))
    architecture = binary_architecture(binary, platform)
    if verify_binary:
        content = binary.read_bytes()
        for home in {str(pathlib.Path.home()), pathlib.Path.home().as_posix()}:
            if any(home.encode(encoding) in content for encoding in ['utf-8', 'utf-16le']):
                raise ValueError('Executable contains local home paths; rebuild with scripts/build_release.py')
        actual = subprocess.check_output([str(binary.resolve()), '--version'], text=True, timeout=30).strip()
        if actual != f'PeerBrush {version}':
            raise ValueError('Executable version does not match the committed source')
    output.mkdir(parents=True, exist_ok=True)
    source = output / f'PeerBrush-{version}-source.zip'
    sources = source_files(root)
    # Stored source entries avoid host zlib differences: the source archive must
    # have one checksum across Windows, macOS and Linux for this commit.
    write_zip(source, sources, epoch, zipfile.ZIP_STORED)
    name = f'PeerBrush-{version}-{platform}-{architecture}'
    # Fresh staging prevents files from earlier packages leaking into an update.
    with tempfile.TemporaryDirectory(prefix='peerbrush-package-') as temporary:
        bundle = pathlib.Path(temporary) / name
        bundle.mkdir()
        exe = 'peerbrush.exe' if platform == 'Windows' else 'peerbrush'
        relative = exe
        if platform == 'macOS':
            relative = 'PeerBrush.app/Contents/MacOS/peerbrush'
            contents = bundle / 'PeerBrush.app' / 'Contents'
            (contents / 'MacOS').mkdir(parents=True)
            with (contents / 'Info.plist').open('wb') as stream:
                plistlib.dump({'CFBundleIdentifier': 'com.peerbrush.PeerBrush', 'CFBundleName': 'PeerBrush', 'CFBundleDisplayName': 'PeerBrush', 'CFBundleExecutable': 'peerbrush', 'CFBundlePackageType': 'APPL', 'CFBundleShortVersionString': version, 'CFBundleVersion': version, 'LSMinimumSystemVersion': '11.0', 'NSHighResolutionCapable': True}, stream)
        shutil.copy2(binary, bundle / relative)
        (bundle / relative).chmod(0o755)
        for filename, data, _ in sources:
            if filename in ['LICENSE', 'README.md', 'FOLLOWUPS.MD'] or filename.startswith(('docs/', 'assets/')):
                destination = bundle / filename
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(data)
        if platform == 'Windows':
            launcher = next(data for filename, data, _ in sources if filename == 'scripts/start-windows.cmd')
            (bundle / 'Start PeerBrush.cmd').write_bytes(launcher)
        shutil.copy2(source, bundle / source.name)
        add_notices(root, bundle, registry)
        manifest = {'schema': 1, 'product': 'PeerBrush', 'version': version, 'commit': commit, 'platform': platform, 'architecture': architecture, 'executable': relative, 'executable_sha256': digest(bundle / relative), 'source': source.name, 'source_sha256': digest(source), 'license': 'GPL-3.0-only', 'native_publisher_signing': 'not-configured', 'package_signature': 'See the matching external Sigstore bundle when supplied; this manifest alone is not a signature.'}
        (bundle / 'release.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
        files = [(f'{name}/{p.relative_to(bundle).as_posix()}', p.read_bytes(), 0o755 if p == bundle / relative else 0o644) for p in bundle.rglob('*') if p.is_file()]
        archive = output / (name + '.zip')
        write_zip(archive, files, epoch)
    sums = output / f'{name}.sha256'
    sums.write_text(f'{digest(archive)}  {archive.name}\n{digest(source)}  {source.name}\n', encoding='utf-8')
    print(json.dumps({'package': archive.name, 'version': version, 'commit': commit, 'architecture': architecture, 'sha256': digest(archive)}))
    return archive


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=pathlib.Path)
    parser.add_argument('--registry', type=pathlib.Path)
    parser.add_argument('--binary', type=pathlib.Path)
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    platform = {'win32': 'Windows', 'darwin': 'macOS'}.get(sys.platform, 'Linux')
    exe = 'peerbrush.exe' if platform == 'Windows' else 'peerbrush'
    cargo_home = pathlib.Path(os.environ.get('CARGO_HOME', str(pathlib.Path.home() / '.cargo')))
    package(root, args.binary or root / 'target' / 'release' / exe, (args.output or root / 'dist').resolve(), args.registry or cargo_home / 'registry' / 'src', platform)


if __name__ == '__main__':
    main()
