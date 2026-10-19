"""Prepare an Ubuntu 24.04 amd64 DEB candidate from an exact-source Linux ZIP.

No installation, default-app changes, signing, repository setup or publication.
APT/dpkg owns installation; this candidate does not establish an update feed.
"""
import argparse
import hashlib
import io
import json
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tarfile
import tomllib
import zipfile

from package import digest, git, regular_file, relative_path, source_files
from verify_release import inspect_archive


APP_ID = 'com.peerbrush.PeerBrush'
# winit/wgpu load these at runtime, beyond the ELF-linked dependency inventory.
RUNTIME = ['libegl1', 'libvulkan1', 'libwayland-client0', 'libx11-6',
           'libxcursor1', 'libxi6', 'libxrandr2', 'libxkbcommon0', 'libxkbcommon-x11-0',
           'desktop-file-utils', 'shared-mime-info']


def deb_version(version):
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?', version):
        raise ValueError('Expected a bounded application SemVer without build metadata')
    # Debian's tilde sorts development/prerelease versions before their release.
    return version.replace('-', '~', 1) + '-1'


def control(version, depends, size, maintainer):
    if not depends or any(c in depends for c in '\n\r\x00'):
        raise ValueError('Expected native shared-library dependencies')
    if not re.fullmatch(r'[^<>\r\n\x00]+ <[^<>\s]+@[^<>\s]+>', maintainer):
        raise ValueError('Supply a public package-maintainer name and email')
    return (f'Package: peerbrush\nVersion: {deb_version(version)}\nArchitecture: amd64\n'
            f'Maintainer: {maintainer}\n'
            'Section: graphics\nPriority: optional\nHomepage: https://peerbrush.com/\n'
            f'Installed-Size: {size}\nDepends: {depends}\n'
            'Recommends: xdg-desktop-portal\n'
            'Description: Native collaborative image editor\n'
            ' Edit native 8-bit and 16-bit images with guarded local AI collaboration.\n')


def stage_payload(root, archive, stage, expected_sha256):
    if stage.exists() or stage.is_symlink():
        raise ValueError('DEB staging requires a fresh directory')
    if not re.fullmatch(r'[0-9a-f]{64}', expected_sha256 or '') or digest(archive) != expected_sha256:
        raise ValueError('Portable SHA-256 differs from the explicitly selected input')
    commit = git(root, 'rev-parse', 'HEAD').decode().strip()
    selected = source_files(root, commit)
    inputs = {name: data for name, data, _ in selected}
    version = tomllib.loads(inputs['Cargo.toml'].decode('utf-8'))['package']['version']
    before = expected_sha256
    checked = inspect_archive(archive, commit, version, root, selected)
    manifest = checked['manifest']
    if manifest['platform'] != 'Linux' or manifest['architecture'] != 'x64':
        raise ValueError('Expected an exact-source Linux x64 portable archive')
    stage.mkdir(parents=True)
    with zipfile.ZipFile(archive) as source:
        for entry in source.infolist():
            relative = pathlib.PurePosixPath(entry.filename).relative_to(checked['prefix'].rstrip('/'))
            destination = stage / 'usr/share/doc/peerbrush' / relative
            if str(relative) == manifest['executable']:
                destination = stage / 'usr/bin/peerbrush'
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(source.read(entry))
            destination.chmod(0o755 if str(relative) == manifest['executable'] else 0o644)
    for name, destination in [
        (f'packaging/linux/{APP_ID}.desktop', f'usr/share/applications/{APP_ID}.desktop'),
        (f'packaging/linux/{APP_ID}.xml', f'usr/share/mime/packages/{APP_ID}.xml'),
        ('assets/peerbrush-logo.png', 'usr/share/peerbrush/peerbrush-logo.png'),
    ]:
        target = stage / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(inputs[name])
        target.chmod(0o644)
    for directory in [stage, *(p for p in stage.rglob('*') if p.is_dir())]:
        directory.chmod(0o755)
    if digest(archive) != before:
        raise ValueError('Portable archive changed during DEB staging')
    return {'schema': 1, 'version': version, 'commit': commit, 'architecture': 'amd64',
            'portable_sha256': before, 'executable_sha256': manifest['executable_sha256'],
            'source_sha256': manifest['source_sha256'], 'release_ready': False,
            'signing': 'not-performed', 'installation_test': 'not-performed',
            'update_owner': 'apt-dpkg', 'update_repository': 'not-configured',
            'default_app': 'user-choice-only', 'target': 'Ubuntu 24.04 amd64'}


def run(command, binary=False, **kwargs):
    try:
        return subprocess.run(command, check=True, capture_output=True, text=not binary, timeout=180, **kwargs).stdout
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f'{pathlib.Path(command[0]).name} failed; no installable candidate was verified') from error


def output_directory(output):
    output = output.absolute()
    for path in [output, *output.parents]:
        try:
            info = path.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & 0x400:
            raise ValueError('DEB output paths must not use links or reparse points')
    output.mkdir(parents=True, exist_ok=True)
    return output


def verify_tar(data, expected):
    """Inspect headers and regular-file bytes without extracting untrusted paths."""
    actual = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:') as archive:
        for entry in archive:
            name = entry.name.removeprefix('./').rstrip('/')
            name = '.' if name in ('', '.') else str(relative_path(name))
            if name in actual or entry.uid != 0 or entry.gid != 0:
                raise ValueError('DEB contains duplicate paths or non-root ownership')
            if entry.isdir():
                actual[name] = {'mode': entry.mode, 'directory': True}
            elif entry.isfile() and entry.size <= 512 * 1024 * 1024:
                with archive.extractfile(entry) as stream:
                    actual[name] = {'mode': entry.mode, 'sha256': hashlib.sha256(stream.read()).hexdigest()}
            else:
                raise ValueError('DEB contains links, special entries or excessive file size')
    if actual != expected:
        raise ValueError('DEB read-back differs from the expected files, bytes or modes')


def stage_inventory(stage):
    base = stage
    expected = {'.': {'mode': 0o755, 'directory': True}}
    for path in base.rglob('*'):
        relative = path.relative_to(base)
        if relative.parts[0] == 'DEBIAN':
            continue
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            expected[relative.as_posix()] = {'mode': 0o755, 'directory': True}
        elif stat.S_ISREG(info.st_mode):
            mode = 0o755 if relative.as_posix() == 'usr/bin/peerbrush' else 0o644
            expected[relative.as_posix()] = {'mode': mode, 'sha256': digest(path)}
        else:
            raise ValueError('DEB staging must contain only regular files and directories')
    return expected


def build(root, archive, output, maintainer, expected_sha256):
    if sys.platform != 'linux' or run(['dpkg', '--print-architecture']).strip() != 'amd64':
        raise ValueError('DEB preparation requires a native Ubuntu amd64 runner')
    release = pathlib.Path('/etc/os-release').read_text()
    if not re.search(r'^ID=ubuntu$', release, re.M) or not re.search(r'^VERSION_ID="24\.04"$', release, re.M):
        raise ValueError('Build on the declared Ubuntu 24.04 baseline')
    root = root.resolve(strict=True)
    archive = regular_file(archive.absolute().parent, archive.name)
    output = output_directory(output)
    with tempfile.TemporaryDirectory(prefix='.peerbrush-deb-', dir=output) as temporary:
        work = pathlib.Path(temporary)
        stage = work / 'payload'
        receipt = stage_payload(root, archive, stage, expected_sha256)
        binary = stage / 'usr/bin/peerbrush'
        if run([str(binary), '--version']).strip() != 'PeerBrush ' + receipt['version']:
            raise ValueError('Executable version differs from the selected source')
        run(['desktop-file-validate', str(stage / f'usr/share/applications/{APP_ID}.desktop')])
        # dpkg-shlibdeps requires source control metadata even for -O output.
        (work / 'debian').mkdir()
        (work / 'debian/control').write_text('Source: peerbrush\n\nPackage: peerbrush\nArchitecture: amd64\n', encoding='utf-8')
        found = run(['dpkg-shlibdeps', '-O', '-e' + str(binary)], cwd=work).strip()
        if not found.startswith('shlibs:Depends=') or '\n' in found:
            raise ValueError('Native dependency inspection returned unexpected metadata')
        depends = found.removeprefix('shlibs:Depends=') + ', ' + ', '.join(RUNTIME)
        size = (sum(p.stat().st_size for p in stage.rglob('*') if p.is_file()) + 1023) // 1024
        (stage / 'DEBIAN').mkdir(mode=0o755)
        (stage / 'DEBIAN').chmod(0o755)
        (stage / 'DEBIAN/control').write_text(control(receipt['version'], depends, size, maintainer), encoding='utf-8')
        (stage / 'DEBIAN/control').chmod(0o644)
        filename = f'peerbrush_{deb_version(receipt["version"])}_amd64.deb'
        candidate = work / filename
        run(['dpkg-deb', '--root-owner-group', '--build', str(stage), str(candidate)])
        verify_tar(run(['dpkg-deb', '--fsys-tarfile', str(candidate)], binary=True), stage_inventory(stage))
        verify_tar(run(['dpkg-deb', '--ctrl-tarfile', str(candidate)], binary=True),
                   {'.': {'mode': 0o755, 'directory': True},
                    'control': {'mode': 0o644, 'sha256': digest(stage / 'DEBIAN/control')}})
        if digest(binary) != receipt['executable_sha256']:
            raise ValueError('Executable changed after archive inspection')
        receipt |= {'file': filename, 'sha256': digest(candidate), 'depends': depends,
                    'native_checks': ['dpkg-shlibdeps', 'desktop-file-validate', 'dpkg-deb exact files, modes and root ownership', 'control archive without installation scripts']}
        targets = [output / filename, output / (filename + '.sha256'), output / (filename + '.json')]
        if any(p.exists() or p.is_symlink() for p in targets):
            raise ValueError('Existing DEB candidates and receipts are immutable; choose a new version')
        with targets[0].open('xb') as stream, candidate.open('rb') as source:
            shutil.copyfileobj(source, stream)
        with targets[1].open('x', encoding='utf-8') as stream:
            stream.write(f'{receipt["sha256"]}  {filename}\n')
        with targets[2].open('x', encoding='utf-8') as stream:
            json.dump(receipt, stream, indent=2, sort_keys=True)
            stream.write('\n')
    return targets[0]


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--portable', type=pathlib.Path, required=True)
    parser.add_argument('--output', type=pathlib.Path, default=pathlib.Path('dist/deb'))
    parser.add_argument('--portable-sha256', required=True, help='Independently selected SHA-256 before executing the input ELF')
    parser.add_argument('--maintainer', required=True, help='Public package-maintainer name <email>; included in package metadata')
    args = parser.parse_args()
    try:
        print('Prepared unsigned installation candidate: ' + build(pathlib.Path(__file__).resolve().parents[1], args.portable, args.output, args.maintainer, args.portable_sha256).name)
    except ValueError as error:
        parser.exit(1, str(error) + '\n')
