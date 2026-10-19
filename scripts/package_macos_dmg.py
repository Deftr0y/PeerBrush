"""Prepare an unsigned DMG candidate from a verified, stapled macOS release ZIP.

This never signs, notarizes, installs, publishes, or changes the original app.
The image is not release-ready: image signing/notarization needs a separate gate.
"""
import argparse
import json
import os
import pathlib
import plistlib
import re
import shutil
import stat
import sys
import tempfile

from package import digest, regular_file
from sign_macos import APP, extract_archive, outside_checkout, run, verify_native
from verify_release import MACOS_SIGNING_KIND, inspect_archive


SUPPORT = 'Documentation and source'


def payload_inventory(root):
    """Only the top-level Applications shortcut may be a link."""
    inventory = {}
    for path in sorted(root.rglob('*')):
        name = path.relative_to(root).as_posix()
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            if name != 'Applications' or os.readlink(path) != '/Applications':
                raise ValueError('Unexpected link in DMG payload')
            inventory[name] = {'link': '/Applications'}
        elif getattr(info, 'st_file_attributes', 0) & 0x400:
            raise ValueError('DMG payload must not contain reparse points')
        elif stat.S_ISDIR(info.st_mode):
            inventory[name] = {'directory': True}
        elif stat.S_ISREG(info.st_mode):
            inventory[name] = {'sha256': digest(path), 'mode': stat.S_IMODE(info.st_mode)}
        else:
            raise ValueError('DMG payload must contain only regular files and directories')
    if inventory.get('Applications') != {'link': '/Applications'}:
        raise ValueError('DMG needs its Applications shortcut')
    return inventory


def create_image(payload, image, mount, volume, verify_app=None):
    """Native read-only round trip; also used by the credential-free CI smoke."""
    if sys.platform != 'darwin':
        raise ValueError('DMG creation requires an actual macOS runner')
    if image.exists() or image.is_symlink() or mount.exists() or mount.is_symlink():
        raise ValueError('Use fresh DMG image and mount paths')
    expected = payload_inventory(payload)
    run(['/usr/bin/hdiutil', 'create', '-srcfolder', str(payload), '-volname', volume,
         '-fs', 'HFS+', '-format', 'UDZO', str(image)], timeout=600)
    verify_image(image, mount, expected, verify_app)
    return expected


def verify_image(image, mount, expected, verify_app=None):
    """Recheck a final signed/stapled image against its original payload."""
    if sys.platform != 'darwin' or mount.exists() or mount.is_symlink():
        raise ValueError('Native DMG verification needs a fresh macOS mount path')
    run(['/usr/bin/hdiutil', 'verify', str(image)], timeout=600)
    mount.mkdir()
    try:
        attached = run(['/usr/bin/hdiutil', 'attach', str(image), '-mountpoint', str(mount),
                        '-readonly', '-nobrowse', '-noautoopen', '-plist'])
        entities = plistlib.loads(attached.stdout.encode()).get('system-entities', [])
        if (not mount.is_mount() or not any(entity.get('mount-point') == str(mount) for entity in entities)):
            raise ValueError('DMG did not mount at the explicitly selected path')
        if payload_inventory(mount) != expected:
            raise ValueError('Mounted DMG differs from the original payload bytes, modes or layout')
        if verify_app is not None:
            verify_app(mount / APP)
    finally:
        # Never let TemporaryDirectory cleanup traverse a still-mounted image,
        # including when attach failed after partially mounting the volume.
        if mount.is_mount():
            run(['/usr/bin/hdiutil', 'detach', str(mount)])
            if mount.is_mount():
                raise ValueError('DMG remains mounted; preserve the work directory')


def prepare(archive, output, commit, version, expected_sha256, revision, source_root=None):
    if sys.platform != 'darwin':
        raise ValueError('DMG preparation requires an actual macOS runner')
    if not re.fullmatch(r'[1-9][0-9]*', str(revision)):
        raise ValueError('An explicit positive DMG candidate revision is required')
    root = source_root or pathlib.Path(__file__).resolve().parents[1]
    archive = regular_file(archive.absolute().parent, archive.name)
    if not re.fullmatch(r'[0-9a-f]{64}', expected_sha256 or '') or digest(archive) != expected_sha256:
        raise ValueError('Signed archive SHA-256 differs from the explicitly selected input')
    checked = inspect_archive(archive, commit, version, root)
    manifest = checked['manifest']
    if manifest['platform'] != 'macOS' or manifest['native_publisher_signing'] != MACOS_SIGNING_KIND or not checked['signing_suffix']:
        raise ValueError('A verified Developer ID signed, notarized and stapled macOS ZIP is required')
    output = outside_checkout(output, root)
    filename = archive.stem + f'-dmg-candidate-r{revision}.dmg'
    targets = [output / filename, output / (filename + '.sha256'), output / (filename + '.verification.json')]
    if any(path.exists() or path.is_symlink() for path in targets):
        raise ValueError('Refusing to replace an existing DMG candidate or receipt; select a new revision')
    output.mkdir(parents=True, exist_ok=True)
    # Retain failed work rather than recursively clean up a possibly mounted
    # volume. Successful verification detaches before removing this directory.
    work = pathlib.Path(tempfile.mkdtemp(prefix='.peerbrush-dmg-', dir=output))
    extract_archive(archive, work / 'input')
    bundle = work / 'input' / checked['prefix'].rstrip('/')
    app = bundle / APP
    verify_native(app, manifest['macos_signing']['team_id'], notarized=True)
    payload = work / 'payload'
    payload.mkdir()
    shutil.copytree(app, payload / APP, copy_function=shutil.copy2)
    support = payload / SUPPORT
    support.mkdir()
    for path in bundle.iterdir():
        if path.name == APP:
            continue
        destination = support / path.name
        if path.is_dir():
            shutil.copytree(path, destination, copy_function=shutil.copy2)
        else:
            shutil.copy2(path, destination)
    (payload / 'Applications').symlink_to('/Applications', target_is_directory=True)
    image = work / filename
    # Prove that repackaging did not alter a signed app, its ticket or support
    # material, before asking the OS to create the image.
    for original in bundle.rglob('*'):
        if original.is_file():
            relative = original.relative_to(bundle)
            copied = payload / relative if relative.parts[0] == APP else support / relative
            if digest(original) != digest(copied) or stat.S_IMODE(original.stat().st_mode) != stat.S_IMODE(copied.stat().st_mode):
                raise ValueError('DMG staging changed original archive bytes or modes')
    expected = create_image(payload, image, work / 'mounted', f'PeerBrush {version}',
                            verify_app=lambda mounted_app: verify_native(mounted_app, manifest['macos_signing']['team_id'], notarized=True))
    if digest(archive) != expected_sha256:
        raise ValueError('Signed archive changed during DMG preparation')
    final_hash = digest(image)
    receipt = {'schema': 1, 'file': filename, 'sha256': final_hash, 'commit': commit,
               'version': version, 'architecture': manifest['architecture'],
               'binary_source_commit': checked['record']['binary_source_commit'],
               'input_archive': archive.name, 'input_archive_sha256': expected_sha256,
               'source_sha256': manifest['source_sha256'], 'payload': expected,
               'app_signing': MACOS_SIGNING_KIND,
               'image_signing': 'not-performed', 'image_notarization': 'not-performed',
               'release_ready': False,
               'native_checks': ['hdiutil verify', 'read-only mounted payload bytes, modes and Applications link',
                                 'codesign Developer ID and Team ID', 'stapler validate', 'spctl Notarized Developer ID'],
               'drag_to_applications_test': 'not-performed',
               'fresh_quarantined_download_test': 'not-performed'}
    # Exclusive creation also protects against concurrent candidate writers.
    with targets[0].open('xb') as stream:
        with image.open('rb') as source:
            shutil.copyfileobj(source, stream)
    with targets[1].open('x', encoding='utf-8') as stream:
        stream.write(f'{final_hash}  {filename}\n')
    with targets[2].open('x', encoding='utf-8') as stream:
        json.dump(receipt, stream, indent=2, sort_keys=True)
        stream.write('\n')
    shutil.rmtree(work)
    return targets[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=pathlib.Path, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    for name in ['commit', 'version', 'expected-sha256', 'revision']:
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    try:
        image = prepare(**vars(args))
        print(f'Prepared {image.name}; unsigned image candidate, not release-ready.')
    except Exception as error:
        print('DMG preparation failed: ' + (str(error) if isinstance(error, ValueError) else type(error).__name__), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
