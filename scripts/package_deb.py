"""Prepare an Ubuntu 24.04 amd64 DEB candidate from an exact-source Linux ZIP.

No installation, default-app changes, signing, repository setup or publication.
APT/dpkg owns installation; this candidate does not establish an update feed.
"""
import argparse
import hashlib
import json
import lzma
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
MAX_DEB_BYTES = 512 * 1024 * 1024
MAX_DATA_BYTES = 544 * 1024 * 1024
MAX_CONTROL_BYTES = 64 * 1024
MAX_TAR_MEMBERS = 20000


def dependencies(depends, require_runtime=False):
    if not isinstance(depends, str) or not 0 < len(depends) <= 8192:
        raise ValueError('Expected bounded native shared-library dependencies')
    native = depends
    if require_runtime:
        suffix = ', ' + ', '.join(RUNTIME)
        if not depends.endswith(suffix):
            raise ValueError('DEB is missing the mandatory runtime dependency list')
        native = depends[:-len(suffix)]
    dependency = r'[a-z0-9][a-z0-9+.-]+(?::(?:amd64|any))?(?: \((?:<<|<=|=|>=|>>) [0-9A-Za-z.+:~\-]{1,128}\))?'
    groups = native.split(', ')
    if (not 1 <= len(groups) <= 128
            or any(not re.fullmatch(dependency + r'(?: \| ' + dependency + r'){0,7}', group) for group in groups)):
        raise ValueError('Expected canonical native shared-library dependencies')
    return depends


def deb_version(version):
    if not isinstance(version, str) or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?', version):
        raise ValueError('Expected a bounded application SemVer without build metadata')
    # Debian's tilde sorts development/prerelease versions before their release.
    return version.replace('-', '~', 1) + '-1'


def control(version, depends, size, maintainer):
    dependencies(depends)
    if (not isinstance(maintainer, str) or not 0 < len(maintainer) <= 256
            or any(ord(c) < 32 or ord(c) == 127 for c in maintainer)
            or not re.fullmatch(r'[^<>]+ <[^<>\s]+@[^<>\s]+>', maintainer)):
        raise ValueError('Supply a public package-maintainer name and email')
    return (f'Package: peerbrush\nVersion: {deb_version(version)}\nArchitecture: amd64\n'
            f'Maintainer: {maintainer}\n'
            'Section: graphics\nPriority: optional\nHomepage: https://peerbrush.com/\n'
            f'Installed-Size: {size}\nDepends: {depends}\n'
            'Recommends: xdg-desktop-portal\n'
            'Description: Native collaborative image editor\n'
            ' Edit native 8-bit and 16-bit images with guarded local AI collaboration.\n')


def portable_payload(root, archive, expected_sha256, commit=None, version=None):
    if (not isinstance(expected_sha256, str) or not re.fullmatch(r'[0-9a-f]{64}', expected_sha256)
            or not 0 < archive.stat().st_size <= MAX_DEB_BYTES or digest(archive) != expected_sha256):
        raise ValueError('Portable SHA-256 differs from the explicitly selected input')
    head = git(root, 'rev-parse', 'HEAD').decode().strip()
    commit = commit or head
    if commit != head:
        raise ValueError('DEB inspection checkout differs from the selected commit')
    selected = source_files(root, commit)
    inputs = {name: data for name, data, _ in selected}
    actual_version = tomllib.loads(inputs['Cargo.toml'].decode('utf-8'))['package']['version']
    if version is not None and version != actual_version:
        raise ValueError('DEB version differs from the selected source')
    version = actual_version
    checked = inspect_archive(archive, commit, version, root, selected)
    manifest = checked['manifest']
    if manifest['platform'] != 'Linux' or manifest['architecture'] != 'x64':
        raise ValueError('Expected an exact-source Linux x64 portable archive')
    if (checked['record']['binary_source_commit'] != commit or checked['packaging_correction']
            or checked['signing_suffix']):
        raise ValueError('DEB requires an unrelabelled exact-source Linux portable archive')
    return checked, inputs


def payload_files(archive, checked, inputs):
    """One mapping for both native staging and independent final-byte inspection."""
    manifest = checked['manifest']
    with zipfile.ZipFile(archive) as source:
        for entry in source.infolist():
            relative = pathlib.PurePosixPath(entry.filename).relative_to(checked['prefix'].rstrip('/'))
            destination = pathlib.PurePosixPath('usr/share/doc/peerbrush') / relative
            if str(relative) == manifest['executable']:
                destination = pathlib.PurePosixPath('usr/bin/peerbrush')
            yield destination.as_posix(), source.read(entry), 0o755 if str(relative) == manifest['executable'] else 0o644
    for name, destination in [
        (f'packaging/linux/{APP_ID}.desktop', f'usr/share/applications/{APP_ID}.desktop'),
        (f'packaging/linux/{APP_ID}.xml', f'usr/share/mime/packages/{APP_ID}.xml'),
        ('assets/peerbrush-logo.png', 'usr/share/peerbrush/peerbrush-logo.png'),
    ]:
        yield destination, inputs[name], 0o644


def stage_payload(root, archive, stage, expected_sha256):
    if stage.exists() or stage.is_symlink():
        raise ValueError('DEB staging requires a fresh directory')
    checked, inputs = portable_payload(root, archive, expected_sha256)
    manifest = checked['manifest']
    stage.mkdir(parents=True)
    for destination, data, mode in payload_files(archive, checked, inputs):
        target = stage / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        target.chmod(mode)
    for directory in [stage, *(p for p in stage.rglob('*') if p.is_dir())]:
        directory.chmod(0o755)
    if digest(archive) != expected_sha256:
        raise ValueError('Portable archive changed during DEB staging')
    return {'schema': 1, 'version': manifest['version'], 'commit': manifest['commit'], 'architecture': 'amd64',
            'portable_sha256': expected_sha256, 'executable_sha256': manifest['executable_sha256'],
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


def tar_entries(data, limit=MAX_DATA_BYTES):
    """Read bounded ordinary tar headers; never apply PAX, sparse or link metadata."""
    if len(data) > limit or len(data) % 512:
        raise ValueError('DEB tar exceeds its byte budget or has incomplete blocks')
    entries = {}
    offset = 0
    while offset + 512 <= len(data):
        header = data[offset:offset + 512]
        if not any(header):
            if len(data) - offset < 1024 or any(memoryview(data)[offset:]):
                raise ValueError('DEB tar has trailing content or an incomplete terminator')
            return entries
        if len(entries) >= MAX_TAR_MEMBERS:
            raise ValueError('DEB tar exceeds its member budget')
        try:
            entry = tarfile.TarInfo.frombuf(header, 'utf-8', 'strict')
        except (tarfile.HeaderError, UnicodeError, ValueError) as error:
            raise ValueError('DEB contains an invalid tar header') from error
        name = entry.name.removeprefix('./').rstrip('/')
        name = '.' if entry.name in ('.', './') else str(relative_path(name))
        if (name in entries or entry.uid != 0 or entry.gid != 0
                or entry.uname not in ('', 'root') or entry.gname not in ('', 'root')):
            raise ValueError('DEB contains duplicate paths or non-root ownership')
        start = offset + 512
        end = start + entry.size
        offset = start + ((entry.size + 511) // 512) * 512
        if entry.size < 0 or offset > len(data) or any(memoryview(data)[end:offset]):
            raise ValueError('DEB contains a truncated file or unexpected tar padding')
        if entry.type == tarfile.DIRTYPE and entry.size == 0:
            content = None
        elif entry.type in (tarfile.REGTYPE, tarfile.AREGTYPE) and entry.size <= MAX_DEB_BYTES:
            if entry.name.endswith('/'):
                raise ValueError('DEB regular files must not use directory path aliases')
            content = memoryview(data)[start:end]
        else:
            raise ValueError('DEB contains links, extended headers, special entries or excessive file size')
        entries[name] = (entry.mode, content)
    raise ValueError('DEB tar is missing its terminator')


def verify_tar(data, expected):
    """Inspect headers and regular-file bytes without extracting untrusted paths."""
    actual = {}
    for name, (mode, content) in tar_entries(data).items():
        actual[name] = ({'mode': mode, 'directory': True} if content is None else
                        {'mode': mode, 'sha256': hashlib.sha256(content).hexdigest()})
    if actual != expected:
        raise ValueError('DEB read-back differs from the expected files, bytes or modes')


def deb_archives(package):
    """Accept only the explicitly selected, bounded xz DEB format we produce."""
    result = {}
    with package.open('rb') as stream:
        if stream.read(8) != b'!<arch>\n':
            raise ValueError('Expected a Debian ar archive')
        for expected, limit in [('debian-binary', 4), ('control.tar.xz', MAX_CONTROL_BYTES), ('data.tar.xz', MAX_DATA_BYTES)]:
            header = stream.read(60)
            if len(header) != 60 or header[58:] != b'`\n' or any(byte < 32 or byte > 126 for byte in header[:58]):
                raise ValueError('DEB has a truncated or invalid ar header')
            try:
                name = header[:16].decode('ascii').rstrip().removesuffix('/')
                fields = [header[a:b].decode('ascii').strip() for a, b in [(16, 28), (28, 34), (34, 40), (40, 48), (48, 58)]]
            except UnicodeError as error:
                raise ValueError('DEB has invalid ar metadata') from error
            if (name != expected or any(not re.fullmatch(r'[0-9]+', field) for field in fields[:3] + fields[4:])
                    or not re.fullmatch(r'[0-7]+', fields[3]) or int(fields[1]) != 0 or int(fields[2]) != 0
                    or int(fields[3], 8) not in (0o644, 0o100644)):
                raise ValueError('DEB has unexpected members, compression or ownership')
            size = int(fields[4])
            if not 0 < size <= min(MAX_DEB_BYTES, limit):
                raise ValueError('DEB member exceeds its compressed-byte budget')
            encoded = stream.read(size)
            if len(encoded) != size or (size % 2 and stream.read(1) != b'\n'):
                raise ValueError('DEB has a truncated ar member or invalid padding')
            if name == 'debian-binary':
                if encoded != b'2.0\n':
                    raise ValueError('Unsupported Debian package version')
                continue
            try:
                decoder = lzma.LZMADecompressor(format=lzma.FORMAT_XZ, memlimit=128 * 1024 * 1024)
                decoded = decoder.decompress(encoded, max_length=limit + 1)
            except lzma.LZMAError as error:
                raise ValueError('DEB has invalid or excessive xz data') from error
            if len(decoded) > limit or not decoder.eof or decoder.unused_data:
                raise ValueError('DEB exceeds its decoded-byte budget or has trailing xz data')
            result[name] = decoded
        if stream.read(1):
            raise ValueError('DEB has extra or trailing ar members')
    return result


def inspect_final(root, portable, package, *, commit, version, portable_sha256, package_sha256, maintainer):
    """Qualify final package content, without executing or installing any input."""
    portable = regular_file(portable.absolute().parent, portable.name)
    package = regular_file(package.absolute().parent, package.name)
    if (not isinstance(commit, str) or not re.fullmatch(r'[0-9a-f]{40}', commit)
            or not isinstance(package_sha256, str) or not re.fullmatch(r'[0-9a-f]{64}', package_sha256)
            or package.name != f'peerbrush_{deb_version(version)}_amd64.deb'
            or not 0 < package.stat().st_size <= MAX_DEB_BYTES or digest(package) != package_sha256):
        raise ValueError('DEB filename, source identity or bytes differ from the selected candidate')
    checked, inputs = portable_payload(root, portable, portable_sha256, commit, version)
    expected = {'.': {'mode': 0o755, 'directory': True}}
    total = 0
    for name, data, mode in payload_files(portable, checked, inputs):
        if name in expected:
            raise ValueError('DEB payload mapping contains duplicate paths')
        expected[name] = {'mode': mode, 'sha256': hashlib.sha256(data).hexdigest()}
        total += len(data)
        for directory in pathlib.PurePosixPath(name).parents:
            if str(directory) != '.':
                directory = str(directory)
                if directory in expected and expected[directory] != {'mode': 0o755, 'directory': True}:
                    raise ValueError('DEB payload mapping contains a file/directory conflict')
                expected[directory] = {'mode': 0o755, 'directory': True}
    archives = deb_archives(package)
    verify_tar(archives['data.tar.xz'], expected)
    controls = tar_entries(archives['control.tar.xz'], MAX_CONTROL_BYTES)
    if (set(controls) != {'.', 'control'} or controls['.'] != (0o755, None)
            or controls['control'][0] != 0o644 or controls['control'][1] is None):
        raise ValueError('DEB control archive must contain only the exact control file and directory')
    try:
        text = bytes(controls['control'][1]).decode('utf-8')
    except UnicodeError as error:
        raise ValueError('DEB control is not UTF-8') from error
    found = re.findall(r'^Depends: ([^\n\r]+)$', text, re.M)
    if len(found) != 1:
        raise ValueError('DEB requires exactly one native dependency field')
    depends = dependencies(found[0], require_runtime=True)
    if text != control(version, depends, (total + 1023) // 1024, maintainer):
        raise ValueError('DEB control differs from approved identity, version, dependencies or exact fields')
    if digest(package) != package_sha256 or digest(portable) != portable_sha256:
        raise ValueError('Selected DEB or portable archive changed during inspection')
    return {'record': dict(file=package.name, bytes=package.stat().st_size, sha256=package_sha256,
                           platform='Linux', architecture='amd64', binary_source_commit=commit,
                           source_sha256=checked['manifest']['source_sha256']),
            'executable_sha256': checked['manifest']['executable_sha256'], 'portable_sha256': portable_sha256,
            'content_checks': ['exact-source-portable-and-installed-payload', 'root-ownership-and-exact-modes',
                               'approved-maintainer-and-exact-control-fields', 'bounded-xz-ar-and-tar-without-installation-scripts',
                               'native-dependency-syntax-and-mandatory-runtime-packages']}


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
        depends = dependencies(found.removeprefix('shlibs:Depends=') + ', ' + ', '.join(RUNTIME), require_runtime=True)
        size = (sum(p.stat().st_size for p in stage.rglob('*') if p.is_file()) + 1023) // 1024
        (stage / 'DEBIAN').mkdir(mode=0o755)
        (stage / 'DEBIAN').chmod(0o755)
        (stage / 'DEBIAN/control').write_text(control(receipt['version'], depends, size, maintainer), encoding='utf-8')
        (stage / 'DEBIAN/control').chmod(0o644)
        filename = f'peerbrush_{deb_version(receipt["version"])}_amd64.deb'
        candidate = work / filename
        run(['dpkg-deb', '--root-owner-group', '-Zxz', '--uniform-compression', '--build', str(stage), str(candidate)])
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
