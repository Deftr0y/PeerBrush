"""Credential-free native fixture: inspect/extract/mount an unsigned dormant app.

The image is a local component fixture, not a signed or installed release. No app
is launched, no updater is loaded and no output is published by this script.
"""
import argparse
import os
import pathlib
import shutil
import sys
import zipfile

from package import digest
from package_macos_dmg import create_image, private_inspection_directory
from prepare_sparkle import tree
from sign_macos import extract_archive
from sparkle_bundle import PREFIX, pin
from verify_release import inspect_archive


def check(root, archive, work, commit, version):
    if sys.platform != 'darwin':
        raise ValueError('Bundled Sparkle native fixture requires macOS 12+')
    import platform
    if int(platform.mac_ver()[0].split('.')[0] or 0) < 12:
        raise ValueError('Bundled Sparkle native fixture requires macOS 12+')
    work = private_inspection_directory(work, root, fresh=True)
    expected = digest(archive)
    selected = inspect_archive(archive, commit, version, root)
    if (selected['manifest']['platform'] != 'macOS' or selected['manifest'].get('macos_sparkle_layout') != 1
            or selected['manifest']['native_publisher_signing'] != 'not-configured' or selected['signing_suffix']):
        raise ValueError('Native fixture requires the exact unsigned dormant app layout')
    work.mkdir(mode=0o700)
    os.chmod(work, 0o700)
    extract_archive(archive, work / 'archive')
    app = work / 'archive' / selected['prefix'] / 'PeerBrush.app'
    def verify_app(path):
        if tree(path / 'Contents/Frameworks/Sparkle.framework') != pin()['framework_entries']:
            raise ValueError('Bundled/mounted Sparkle changed from the exact upstream pin')
    verify_app(app)
    payload = work / 'payload'
    payload.mkdir(mode=0o700)
    shutil.copytree(app, payload / 'PeerBrush.app', symlinks=True)
    (payload / 'Applications').symlink_to('/Applications', target_is_directory=True)
    create_image(payload, work / 'unsigned-component-fixture.dmg', work / 'mounted',
                 'PeerBrush dormant updater fixture', verify_app=verify_app)
    if digest(archive) != expected:
        raise ValueError('Original unsigned candidate changed during native fixture')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['archive', 'work']:
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    for name in ['commit', 'version']:
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    try:
        check(pathlib.Path(__file__).resolve().parents[1], **vars(args))
    except Exception:
        print('Bundled Sparkle component failed; preserve private fixture for inspection.', file=sys.stderr)
        return 1
    print('Exact dormant unsigned Sparkle bundle preserved through ZIP and mounted image; no signing/install/update acceptance claimed.')
    return 0


if __name__ == '__main__':
    sys.exit(main())
