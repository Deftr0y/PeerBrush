"""Install/upgrade/remove actual DEB bytes only inside an explicitly disposable container.

The higher-version upgrade is a private packaging fixture using the same app
binary; this does not establish an application-version/settings migration or GUI
acceptance. No signed repository, update service or public package is created.
"""
import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile

from package import digest
from package_deb import deb_version


APP_ID = 'com.peerbrush.PeerBrush'
MIME = 'image/vnd.adobe.photoshop'


def run(command, **kwargs):
    try:
        return subprocess.run(command, check=True, capture_output=True, text=True, timeout=600, **kwargs).stdout.strip()
    except (OSError, subprocess.SubprocessError) as error:
        # check() guards this disposable synthetic container before any command.
        # Report bounded process diagnostics, never arguments or environment.
        output = []
        for stream in [getattr(error, 'stdout', None), getattr(error, 'stderr', None)]:
            if isinstance(stream, bytes):
                stream = stream.decode('utf-8', errors='replace')
            if stream:
                output.append(stream)
        detail = '\n'.join('\n'.join(output).splitlines()[-24:])[-4096:]
        detail = ''.join(char for char in detail if char.isprintable() or char == '\n')
        status = (' timed out' if isinstance(error, subprocess.TimeoutExpired)
                  else f' exited {error.returncode}' if isinstance(error, subprocess.CalledProcessError)
                  else ' could not start')
        raise ValueError(f'{pathlib.Path(command[0]).name}{status} in the disposable installation check'
                         + (':\n' + detail if detail else '')) from error


def require_disposable():
    if (sys.platform != 'linux' or os.environ.get('PEERBRUSH_DISPOSABLE_PACKAGE_TEST') != '1'
            or not pathlib.Path('/.dockerenv').is_file() or os.geteuid() != 0):
        raise ValueError('Installation checks require an explicitly disposable root Docker container; never run on the host')
    release = pathlib.Path('/etc/os-release').read_text()
    if not re.search(r'^ID=ubuntu$', release, re.M) or not re.search(r'^VERSION_ID="24\.04"$', release, re.M):
        raise ValueError('Installation acceptance is bounded to Ubuntu 24.04')


def validate_expected(expected):
    if (not isinstance(expected, dict)
            or set(expected) != {'version', 'commit', 'package_sha256', 'executable_sha256', 'source_sha256'}
            or type(expected['version']) is not str):
        raise ValueError('Supply exact independently selected candidate metadata')
    deb_version(expected['version'])
    for name, size in [('commit', 40), ('package_sha256', 64), ('executable_sha256', 64), ('source_sha256', 64)]:
        if type(expected[name]) is not str or not re.fullmatch('[0-9a-f]{' + str(size) + '}', expected[name]):
            raise ValueError('Supply exact independently selected candidate metadata')


def selected_package(package, expected):
    validate_expected(expected)
    if (package.name != f'peerbrush_{deb_version(expected["version"])}_amd64.deb'
            or package.is_symlink() or not package.is_file() or digest(package) != expected['package_sha256']):
        raise ValueError('DEB filename or bytes differ from the selected candidate')
    for field, value in [('Package', 'peerbrush'), ('Architecture', 'amd64'), ('Version', deb_version(expected['version']))]:
        if run(['dpkg-deb', '--field', str(package), field]) != value:
            raise ValueError('DEB identity, architecture or version differs from the selected candidate')


def installed_identity(expected, user, package_version):
    binary = pathlib.Path('/usr/bin/peerbrush')
    source = pathlib.Path('/usr/share/doc/peerbrush') / f'PeerBrush-{expected["version"]}-source.zip'
    for path, checksum in [(binary, expected['executable_sha256']), (source, expected['source_sha256'])]:
        if path.is_symlink() or not path.is_file() or digest(path) != checksum:
            raise ValueError('Installed executable or corresponding source differs from the selected candidate')
    for field, value in [('Package', 'peerbrush'), ('Architecture', 'amd64'), ('Version', package_version)]:
        if run(['dpkg-query', '--showformat=${' + field + '}', '--show', 'peerbrush']) != value:
            raise ValueError('Installed package identity differs from the selected candidate')
    if user([str(binary), '--version']) != 'PeerBrush ' + expected['version']:
        raise ValueError('Installed application version differs from the selected candidate')
    info = dict(format='PeerBrushBuildInfoV1', schema=1, product='PeerBrush', version=expected['version'],
                commit=expected['commit'], tree='clean', end='PeerBrushBuildInfoEnd')
    if user([str(binary), '--build-info']) != json.dumps(info, separators=(',', ':')):
        raise ValueError('Installed build provenance differs from the clean selected source')


def receipt_path(output):
    output = output.absolute()
    if output.exists() or output.is_symlink() or not output.parent.is_dir():
        raise ValueError('Choose a new receipt under an existing private output directory')
    if any(path.is_symlink() for path in output.parents):
        raise ValueError('Receipt output directories must not use symbolic links')
    if os.name == 'posix' and stat.S_IMODE(output.parent.stat().st_mode) & 0o077:
        raise ValueError('Receipt output parent must be private')
    return output


def write_receipt(output, receipt):
    output = receipt_path(output)
    with output.open('x', encoding='utf-8', newline='\n') as stream:
        # The runner supplies a private parent; keep a container-root-owned file
        # readable by its host owner without granting broader directory access.
        output.chmod(0o644)
        stream.write(json.dumps(receipt, sort_keys=True, separators=(',', ':')) + '\n')


def check(package, expected, output):
    require_disposable()  # Must precede user creation, package changes or fixture writes.
    validate_expected(expected)
    output = receipt_path(output)
    package = package.absolute()
    if package.is_symlink() or not package.is_file() or digest(package) != expected['package_sha256']:
        raise ValueError('DEB bytes differ from the selected candidate')
    with tempfile.TemporaryDirectory(prefix='peerbrush-install-qa-') as temporary:
        work = pathlib.Path(temporary)
        # Host-mounted input can change independently of the container. Install
        # only a byte-verified copy under this private disposable test directory.
        selected = work / package.name
        shutil.copyfile(package, selected)
        selected_package(selected, expected)
        run(['useradd', '--create-home', '--shell', '/bin/sh', 'peerbrush-qa'])
        home = pathlib.Path('/home/peerbrush-qa')
        environment = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
                           XDG_DATA_HOME=str(home / '.local/share'), XDG_CACHE_HOME=str(home / '.cache'))
        def user(command):
            return run(['runuser', '-u', 'peerbrush-qa', '--', *command], env=environment)
        applications = home / '.local/share/applications'
        applications.mkdir(parents=True)
        existing = applications / 'peerbrush-qa-existing.desktop'
        existing.write_text('[Desktop Entry]\nType=Application\nName=Existing QA editor\nExec=/bin/true %F\nMimeType=' + MIME + ';\n')
        config = home / '.config'
        config.mkdir()
        defaults = config / 'mimeapps.list'
        defaults.write_text('[Default Applications]\n' + MIME + '=peerbrush-qa-existing.desktop\n')
        # Include legacy explicit state and XDG preferences/document artwork.
        # Package lifecycle must not remove either, regardless of migration.
        preserved = [home / 'PeerBrush QA state/tool-settings.json',
                     config / 'peerbrush/settings-v1/tool-settings.json', home / 'Artwork 作品.psd']
        for path in preserved:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'SYNTHETIC_USER_OWNED_RETENTION_FIXTURE\n')
        run(['chown', '-R', 'peerbrush-qa:peerbrush-qa', str(home)])
        original = {path: hashlib.sha256(path.read_bytes()).hexdigest() for path in [defaults, existing, *preserved]}
        original_package = expected['package_sha256']
        def retained():
            for path, checksum in original.items():
                if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != checksum:
                    raise ValueError('Installation lifecycle changed user defaults, settings or artwork')
            if user(['xdg-mime', 'query', 'default', MIME]) != existing.name:
                raise ValueError('Installation lifecycle changed the existing default editor')
        def installed(package_version):
            retained()
            installed_identity(expected, user, package_version)
            if not pathlib.Path(f'/usr/share/applications/{APP_ID}.desktop').is_file():
                raise ValueError('Installed Open With desktop entry is missing')
            cache = pathlib.Path('/usr/share/applications/mimeinfo.cache').read_text()
            if APP_ID + '.desktop' not in cache:
                raise ValueError('Desktop MIME registration trigger did not run')
            for extension, version in [('psd', 1), ('psb', 2)]:
                sample = home / ('Open With 作品.' + extension)
                sample.write_bytes(b'8BPS' + version.to_bytes(2, 'big') + bytes(6) + b'\x00\x03' + b'\x00\x00\x00\x01' * 2 + b'\x00\x08\x00\x03')
                run(['chown', 'peerbrush-qa:peerbrush-qa', str(sample)])
                if user(['xdg-mime', 'query', 'filetype', str(sample)]) != MIME:
                    raise ValueError('PSD/PSB MIME registration is missing')
            if ':image/vnd.adobe.photoshop:*.psb' not in pathlib.Path('/usr/share/mime/globs2').read_text():
                raise ValueError('The shared MIME cache trigger did not run')
            if 'usr/bin/peerbrush' not in run(['dpkg-query', '--listfiles', 'peerbrush']):
                raise ValueError('Installed package identity is missing')
        def removed():
            retained()
            if pathlib.Path('/usr/bin/peerbrush').exists() or pathlib.Path(f'/usr/share/applications/{APP_ID}.desktop').exists():
                raise ValueError('Package-owned launch integration was not removed')
            cache = pathlib.Path('/usr/share/applications/mimeinfo.cache').read_text()
            if APP_ID + '.desktop' in cache or pathlib.Path(f'/usr/share/mime/packages/{APP_ID}.xml').exists():
                raise ValueError('Package-owned MIME integration was not removed')
        retained()
        run(['apt-get', 'install', '-y', str(selected)], env=dict(os.environ, DEBIAN_FRONTEND='noninteractive'))
        installed(deb_version(expected['version']))
        # Private higher-revision fixture validates dpkg upgrade semantics while
        # retaining exact executable bytes. Never upload it as a release file.
        unpacked = work / 'upgrade'
        run(['dpkg-deb', '--raw-extract', str(selected), str(unpacked)])
        control = unpacked / 'DEBIAN/control'
        current = deb_version(expected['version'])
        later = current + '+qa1'
        run(['dpkg', '--compare-versions', later, 'gt', current])
        control.write_text(control.read_text().replace('Version: ' + current + '\n', 'Version: ' + later + '\n'))
        upgrade = work / 'private-upgrade-fixture.deb'
        run(['dpkg-deb', '--root-owner-group', '--build', str(unpacked), str(upgrade)])
        run(['dpkg', '--install', str(upgrade)])
        if run(['dpkg-query', '--showformat=${Version}', '--show', 'peerbrush']) != later:
            raise ValueError('The private higher-revision upgrade was not installed')
        installed(later)
        run(['apt-get', 'remove', '-y', 'peerbrush'])
        removed()
        run(['apt-get', 'purge', '-y', 'peerbrush'])
        removed()
        run(['apt-get', 'install', '-y', str(selected)], env=dict(os.environ, DEBIAN_FRONTEND='noninteractive'))
        installed(current)
        run(['apt-get', 'purge', '-y', 'peerbrush'])
        removed()
        if digest(package) != original_package or digest(selected) != original_package:
            raise ValueError('Installation tests changed the original candidate')
    receipt = dict(schema=1, format='PeerBrushDebInstallationV1', product='PeerBrush',
                   version=expected['version'], commit=expected['commit'],
                   package=dict(file=package.name, sha256=original_package),
                   executable_sha256=expected['executable_sha256'], source_sha256=expected['source_sha256'],
                   target='Ubuntu 24.04 amd64', architecture='amd64',
                   checks={'installation': 'passed', 'installed-build-identity': 'passed',
                           'same-binary-package-upgrade': 'passed', 'remove-purge-reinstall-retention': 'passed',
                           'mime-and-default-consent': 'passed'},
                   limits={'native-gui': 'not-performed', 'real-application-version-migration': 'not-performed',
                           'unsaved-work-and-update-interruption': 'not-performed',
                           'publisher-signature': 'not-performed', 'update-repository': 'not-configured',
                           'public-availability': 'not-assessed'},
                   release_ready=False, production_readiness='not-assessed')
    write_receipt(output, receipt)
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', required=True, type=pathlib.Path)
    for name in ['package-sha256', 'commit', 'version', 'executable-sha256', 'source-sha256']:
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    args = parser.parse_args()
    try:
        expected = {name: getattr(args, name) for name in ['version', 'commit', 'package_sha256', 'executable_sha256', 'source_sha256']}
        check(args.package, expected, args.output)
        print('Source-bound disposable DEB checks passed; release readiness is not established.')
    except (ValueError, OSError) as error:
        message = str(error) if isinstance(error, ValueError) else 'Installation evidence I/O failed; acceptance was not established'
        parser.exit(1, message + '\n')
