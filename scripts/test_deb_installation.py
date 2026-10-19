"""Install/upgrade/remove actual DEB bytes only inside an explicitly disposable container.

The higher-version upgrade is a private packaging fixture using the same app
binary; this does not establish an application-version/settings migration or GUI
acceptance. No signed repository, update service or public package is created.
"""
import argparse
import hashlib
import os
import pathlib
import re
import subprocess
import sys
import tempfile


APP_ID = 'com.peerbrush.PeerBrush'
MIME = 'image/vnd.adobe.photoshop'


def run(command, **kwargs):
    try:
        return subprocess.run(command, check=True, capture_output=True, text=True, timeout=600, **kwargs).stdout.strip()
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f'{pathlib.Path(command[0]).name} failed in the disposable installation check') from error


def require_disposable():
    if (sys.platform != 'linux' or os.environ.get('PEERBRUSH_DISPOSABLE_PACKAGE_TEST') != '1'
            or not pathlib.Path('/.dockerenv').is_file() or os.geteuid() != 0):
        raise ValueError('Installation checks require an explicitly disposable root Docker container; never run on the host')
    release = pathlib.Path('/etc/os-release').read_text()
    if not re.search(r'^ID=ubuntu$', release, re.M) or not re.search(r'^VERSION_ID="24\.04"$', release, re.M):
        raise ValueError('Installation acceptance is bounded to Ubuntu 24.04')


def check(package):
    require_disposable()  # Must precede user creation, package changes or fixture writes.
    package = package.resolve(strict=True)
    if run(['dpkg-deb', '--field', str(package), 'Package']) != 'peerbrush':
        raise ValueError('Expected the PeerBrush installation candidate')
    with tempfile.TemporaryDirectory(prefix='peerbrush-install-qa-') as temporary:
        work = pathlib.Path(temporary)
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
        original_package = hashlib.sha256(package.read_bytes()).hexdigest()
        def retained():
            for path, checksum in original.items():
                if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != checksum:
                    raise ValueError('Installation lifecycle changed user defaults, settings or artwork')
            if user(['xdg-mime', 'query', 'default', MIME]) != existing.name:
                raise ValueError('Installation lifecycle changed the existing default editor')
        def installed():
            retained()
            user(['/usr/bin/peerbrush', '--state-dir', str(home / 'PeerBrush QA state'), '--version'])
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
        run(['apt-get', 'install', '-y', str(package)], env=dict(os.environ, DEBIAN_FRONTEND='noninteractive'))
        installed()
        # Private higher-revision fixture validates dpkg upgrade semantics while
        # retaining exact executable bytes. Never upload it as a release file.
        unpacked = work / 'upgrade'
        run(['dpkg-deb', '--raw-extract', str(package), str(unpacked)])
        control = unpacked / 'DEBIAN/control'
        current = run(['dpkg-deb', '--field', str(package), 'Version'])
        later = current + '+qa1'
        run(['dpkg', '--compare-versions', later, 'gt', current])
        control.write_text(control.read_text().replace('Version: ' + current + '\n', 'Version: ' + later + '\n'))
        upgrade = work / 'private-upgrade-fixture.deb'
        run(['dpkg-deb', '--root-owner-group', '--build', str(unpacked), str(upgrade)])
        run(['dpkg', '--install', str(upgrade)])
        if run(['dpkg-query', '--showformat=${Version}', '--show', 'peerbrush']) != later:
            raise ValueError('The private higher-revision upgrade was not installed')
        installed()
        run(['apt-get', 'remove', '-y', 'peerbrush'])
        removed()
        run(['apt-get', 'purge', '-y', 'peerbrush'])
        removed()
        run(['apt-get', 'install', '-y', str(package)], env=dict(os.environ, DEBIAN_FRONTEND='noninteractive'))
        installed()
        run(['apt-get', 'purge', '-y', 'peerbrush'])
        removed()
        if hashlib.sha256(package.read_bytes()).hexdigest() != original_package:
            raise ValueError('Installation tests changed the original candidate')
    return {'installation': 'passed', 'same-binary-package-upgrade': 'passed',
            'remove-purge-reinstall-retention': 'passed', 'mime-and-default-consent': 'passed',
            'native-gui-and-real-version-migration': 'not-performed'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', required=True, type=pathlib.Path)
    args = parser.parse_args()
    try:
        print(check(args.package))
    except ValueError as error:
        parser.exit(1, str(error) + '\n')
