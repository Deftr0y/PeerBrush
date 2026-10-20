"""Prepare an unsigned DMG candidate from a verified, stapled macOS release ZIP.

This never signs, notarizes, installs, publishes, or changes the original app.
The image is not release-ready: image signing/notarization needs a separate gate.
"""
import argparse
import hashlib
import json
import os
import pathlib
import plistlib
import re
import shutil
import stat
import sys
import tempfile
import zipfile

from package import digest, git, regular_file
from sign_macos import APP, extract_archive, outside_checkout, run, verify_native
from verify_release import MACOS_SIGNING_KIND, inspect_archive


SUPPORT = 'Documentation and source'
MAX_IMAGE_BYTES = 512 * 1024 * 1024
MAX_PAYLOAD_ENTRIES = 16384


def inspection_path(path):
    """Existing parents and no links, including the platform's reparse points."""
    path = pathlib.Path(path)
    if not path.is_absolute():
        raise ValueError('DMG inspection requires absolute input and work paths')
    path = pathlib.Path(os.path.abspath(path))
    for ancestor in [path, *path.parents]:
        try:
            metadata = ancestor.lstat()
        except FileNotFoundError:
            if ancestor == path:
                continue
            raise ValueError('DMG inspection requires existing parent directories') from None
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
            raise ValueError('DMG inspection paths cannot use links or reparse points')
        if ancestor != path and not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('DMG inspection requires existing parent directories')
    return path


def private_inspection_directory(path, root, *, fresh):
    """Private transient work must stay outside every Git checkout, not just root."""
    path = inspection_path(path)
    if path.is_relative_to(pathlib.Path(root).resolve()):
        raise ValueError('DMG inspection work must stay outside source checkouts')
    for ancestor in [path, *path.parents]:
        try:
            (ancestor / '.git').lstat()
        except FileNotFoundError:
            continue
        raise ValueError('DMG inspection work must stay outside every Git checkout')
    if fresh:
        if path.exists():
            raise ValueError('DMG inspection work and outputs are immutable; select fresh directories')
        parent = path.parent.stat()
        if os.name == 'posix' and (parent.st_uid != os.getuid() or stat.S_IMODE(parent.st_mode) & 0o022):
            raise ValueError('DMG inspection needs an owned parent directory without group or public writes')
    else:
        metadata = path.lstat()
        if (not stat.S_ISDIR(metadata.st_mode) or path.is_mount()
                or (os.name == 'posix' and (metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700))):
            raise ValueError('DMG inspection needs its original private owned work directory')
    return path


def bounded_file(path, limit, destination=None):
    """Hash/copy a regular file without following a changed link or growing input."""
    path = inspection_path(path)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or not 0 <= before.st_size <= limit:
        raise ValueError('DMG inspection input is not an unchanged bounded regular file')
    flags = os.O_RDONLY | getattr(os, 'O_BINARY', 0) | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0)
    with os.fdopen(os.open(path, flags), 'rb') as stream:
        metadata = os.fstat(stream.fileno())
        identity = lambda info: (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns, info.st_mode, info.st_nlink)
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1
                or not 0 <= metadata.st_size <= limit or identity(before) != identity(metadata)):
            raise ValueError('DMG inspection input is not an unchanged bounded regular file')
        checksum, total = hashlib.sha256(), 0
        target = destination.open('xb') if destination is not None else None
        try:
            while chunk := stream.read(min(64 * 1024, limit + 1 - total)):
                total += len(chunk)
                if total > limit:
                    raise ValueError('DMG inspection input exceeds its byte budget')
                checksum.update(chunk)
                if target is not None:
                    target.write(chunk)
        finally:
            if target is not None:
                target.close()
        inspection_path(path)
        if (total != metadata.st_size or identity(os.fstat(stream.fileno())) != identity(metadata)
                or identity(path.lstat()) != identity(metadata)):
            raise ValueError('DMG inspection input changed during its bounded read')
    return checksum.hexdigest(), total


def payload_inventory(root):
    """Only the top-level Applications shortcut may be a link."""
    inventory = {}
    total = 0
    def visit(path):
        nonlocal total
        name = path.relative_to(root).as_posix()
        info = path.lstat()
        if len(inventory) >= MAX_PAYLOAD_ENTRIES or len(name) > 1024 or len(path.relative_to(root).parts) > 32:
            raise ValueError('DMG payload exceeds its entry or path budget')
        if stat.S_ISLNK(info.st_mode):
            if name != 'Applications' or os.readlink(path) != '/Applications':
                raise ValueError('Unexpected link in DMG payload')
            inventory[name] = {'link': '/Applications'}
        elif getattr(info, 'st_file_attributes', 0) & 0x400:
            raise ValueError('DMG payload must not contain reparse points')
        elif stat.S_ISDIR(info.st_mode):
            inventory[name] = {'directory': True}
            with os.scandir(path) as children:
                for child in children:
                    visit(pathlib.Path(child.path))
        elif stat.S_ISREG(info.st_mode):
            checksum, size = bounded_file(path, MAX_IMAGE_BYTES - total)
            total += size
            inventory[name] = {'sha256': checksum, 'mode': stat.S_IMODE(info.st_mode)}
        else:
            raise ValueError('DMG payload must contain only regular files and directories')
    with os.scandir(root) as children:
        for child in children:
            visit(pathlib.Path(child.path))
    if inventory.get('Applications') != {'link': '/Applications'}:
        raise ValueError('DMG needs its Applications shortcut')
    return inventory


def inspect_final(root, archive, image, *, commit, version, archive_sha256, image_sha256, team_id, work):
    """Independently qualify pinned final bytes; never read a signer receipt.

    Native verification happens on private owned copies. Failures retain work,
    especially a mount that the OS could not detach; never recursively clean it.
    """
    if sys.platform != 'darwin':
        raise ValueError('Final DMG inspection requires an actual macOS runner')
    if (not isinstance(version, str) or len(version) > 32
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version)
            or not isinstance(commit, str) or not re.fullmatch('[0-9a-f]{40}', commit)
            or not isinstance(team_id, str) or not re.fullmatch('[A-Z0-9]{10}', team_id)
            or any(not isinstance(value, str) or not re.fullmatch('[0-9a-f]{64}', value)
                   for value in [archive_sha256, image_sha256])):
        raise ValueError('Final DMG inspection needs exact stable source, hashes and publisher Team ID')
    root, archive, image = inspection_path(root), inspection_path(archive), inspection_path(image)
    if (not re.fullmatch('PeerBrush-' + re.escape(version) + r'-macOS-arm64-apple-signed-r[1-9][0-9]*\.zip', archive.name)
            or not re.fullmatch(re.escape(archive.stem) + r'-dmg-signed-r[1-9][0-9]*\.dmg', image.name)):
        raise ValueError('Final DMG and signed ZIP filenames must select the same stable candidate')
    work = private_inspection_directory(work, root, fresh=True)
    if any(work.is_relative_to(path.parent) for path in [archive, image]):
        raise ValueError('DMG inspection work must be separate from input directories')
    # No native command may observe a changing public input. Copy and verify both
    # hashes before archive inspection, image assessment or mounting.
    work.mkdir(mode=0o700)
    os.chmod(work, 0o700)
    private_inspection_directory(work, root, fresh=False)
    inputs = work / 'inputs'
    inputs.mkdir(mode=0o700)
    copied_archive, copied_image = inputs / archive.name, inputs / image.name
    for source, destination, expected in [(archive, copied_archive, archive_sha256), (image, copied_image, image_sha256)]:
        checksum, size = bounded_file(source, MAX_IMAGE_BYTES, destination)
        if size == 0 or checksum != expected:
            raise ValueError('Final DMG or signed ZIP differs from its independently selected SHA-256')
        destination.chmod(0o400)
    with zipfile.ZipFile(copied_archive) as source:
        entries = source.infolist()
        if (len(entries) > MAX_PAYLOAD_ENTRIES or sum(entry.file_size for entry in entries) > MAX_IMAGE_BYTES
                or any(len(entry.filename) > 1024 or len(pathlib.PurePosixPath(entry.filename).parts) > 32 for entry in entries)):
            raise ValueError('Signed ZIP exceeds the bounded DMG payload contract')
    checked = inspect_archive(copied_archive, commit, version, root)
    manifest = checked['manifest']
    if (checked['packaging_correction'] or checked['record']['binary_source_commit'] != commit
            or checked['record']['sha256'] != archive_sha256 or manifest['platform'] != 'macOS'
            or manifest['architecture'] != 'arm64' or not checked['signing_suffix']
            or manifest['native_publisher_signing'] != MACOS_SIGNING_KIND
            or manifest['macos_signing']['team_id'] != team_id):
        raise ValueError('Final DMG requires the exact-source signed Mac ZIP for the selected publisher')
    extract_archive(copied_archive, work / 'archive')
    bundle = work / 'archive' / checked['prefix'].rstrip('/')
    expected = {'Applications': {'link': '/Applications'}}
    total = 0
    # The selected ZIP itself determines every byte and file mode. Its manifest,
    # committed source and native app trust are separately checked above/below.
    with zipfile.ZipFile(copied_archive) as source:
        for entry in source.infolist():
            relative = pathlib.PurePosixPath(entry.filename[len(checked['prefix']):])
            mapped = relative if relative.parts[0] == APP else pathlib.PurePosixPath(SUPPORT) / relative
            checksum, size = hashlib.sha256(), 0
            with source.open(entry) as stream:
                while chunk := stream.read(min(64 * 1024, entry.file_size + 1 - size)):
                    size += len(chunk)
                    if size > entry.file_size:
                        raise ValueError('Signed ZIP member exceeds its declared byte budget')
                    checksum.update(chunk)
            if size != entry.file_size:
                raise ValueError('Signed ZIP member is truncated')
            total += size
            if total > MAX_IMAGE_BYTES:
                raise ValueError('Signed ZIP expands beyond the bounded DMG byte budget')
            expected[mapped.as_posix()] = {'sha256': checksum.hexdigest(), 'mode': stat.S_IMODE(entry.external_attr >> 16)}
            for parent in mapped.parents:
                if parent != pathlib.PurePosixPath('.'):
                    expected[parent.as_posix()] = {'directory': True}
            if len(expected) > MAX_PAYLOAD_ENTRIES:
                raise ValueError('Signed ZIP expands beyond the bounded DMG layout')
    private_inspection_directory(work, root, fresh=False)
    verify_native(bundle / APP, team_id, notarized=True)
    # This is verification only: no identity lookup, credentials, signing,
    # notarization submission, stapling, app execution or installation.
    from sign_macos import verify_native_image
    verify_native_image(copied_image, team_id, notarized=True)
    mount = work / 'mounted'
    verify_image(copied_image, mount, expected, verify_app=lambda app: verify_native(app, team_id, notarized=True))
    if mount.is_mount():
        raise ValueError('DMG remains mounted; preserve inspection work')
    for path, checksum in [(archive, archive_sha256), (image, image_sha256),
                           (copied_archive, archive_sha256), (copied_image, image_sha256)]:
        if bounded_file(path, MAX_IMAGE_BYTES)[0] != checksum:
            raise ValueError('Selected DMG or signed ZIP changed during final inspection')
    if git(root, 'rev-parse', 'HEAD').decode().strip() != commit:
        raise ValueError('Selected source changed during final DMG inspection')
    result = dict(schema=1, format='PeerBrushDmgQualificationV1', version=version, commit=commit,
                  record=dict(file=image.name, bytes=copied_image.stat().st_size, sha256=image_sha256,
                              platform='macOS', architecture='arm64', binary_source_commit=commit,
                              source_sha256=manifest['source_sha256']),
                  signed_zip=dict(file=archive.name, sha256=archive_sha256),
                  executable_sha256=manifest['executable_sha256'], publisher_team_id=team_id,
                  candidate_verified=True, ready=False, production_ready=False, public_availability='unverified',
                  content_checks=['exact-source-signed-zip-and-corresponding-source', 'exact-mounted-files-modes-and-Applications-link'],
                  native_checks=['Developer-ID-publisher-and-secure-timestamps', 'app-hardened-runtime-and-empty-entitlements',
                                 'image-and-app-stapled-tickets-and-Gatekeeper', 'hdiutil-integrity-and-read-only-mount'],
                  limits={'drag-to-Applications-and-launch': 'not-performed', 'fresh-quarantined-download': 'not-performed',
                          'upgrade-settings-and-unsaved-work': 'not-performed', 'public-availability': 'not-assessed'},
                  native_job_provenance='requires-trusted-native-job-at-consumption')
    private_inspection_directory(work, root, fresh=False)
    copied_archive.chmod(0o600)
    copied_image.chmod(0o600)
    shutil.rmtree(work)
    return result


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
    if attached_image_device(image) is not None:
        raise ValueError('DMG inspection requires an image that is not already attached')
    mount.mkdir()
    try:
        attached = run(['/usr/bin/hdiutil', 'attach', str(image), '-mountpoint', str(mount),
                        '-readonly', '-nobrowse', '-noautoopen', '-plist'])
        data = attached.stdout.encode('utf-8')
        if len(data) > 2 * 1024 * 1024:
            raise ValueError('Native DMG attachment report exceeds its byte budget')
        entities = plistlib.loads(data).get('system-entities', [])
        if (not isinstance(entities, list) or not 1 <= len(entities) <= 32
                or any(not isinstance(entity, dict) for entity in entities)
                or not mount.is_mount()
                or [entity['mount-point'] for entity in entities if 'mount-point' in entity] != [str(mount)]):
            raise ValueError('DMG must mount exactly one volume at the explicitly selected path')
        if payload_inventory(mount) != expected:
            raise ValueError('Mounted DMG differs from the original payload bytes, modes or layout')
        if verify_app is not None:
            verify_app(mount / APP)
    finally:
        # Bind the device to this exact image before detaching the whole image,
        # including an unexpected second volume or a partially failed attach.
        device = attached_image_device(image)
        if device is not None:
            run(['/usr/bin/hdiutil', 'detach', device])
            if attached_image_device(image) is not None:
                raise ValueError('DMG device remains attached; preserve the work directory')
        if mount.is_mount():
            raise ValueError('DMG remains mounted; preserve the work directory')


def attached_image_device(image):
    """Only detach a whole BSD disk associated with this exact image by hdiutil."""
    response = run(['/usr/bin/hdiutil', 'info', '-plist'])
    data = response.stdout.encode('utf-8')
    if len(data) > 2 * 1024 * 1024:
        raise ValueError('Native DMG attachment inventory exceeds its byte budget')
    info = plistlib.loads(data)
    images = info.get('images') if isinstance(info, dict) else None
    if not isinstance(images, list) or len(images) > 256 or any(not isinstance(row, dict) for row in images):
        raise ValueError('Cannot safely identify attached DMG images; preserve inspection work')
    selected = [row for row in images if row.get('image-path') == str(image)]
    if not selected:
        return None
    if len(selected) != 1:
        raise ValueError('DMG image has ambiguous attachments; preserve inspection work')
    entries = selected[0].get('system-entities')
    if not isinstance(entries, list) or not 1 <= len(entries) <= 32:
        raise ValueError('DMG image has an unsupported device inventory; preserve inspection work')
    devices, whole = set(), set()
    for entry in entries:
        name = entry.get('dev-entry') if isinstance(entry, dict) else None
        match = re.fullmatch(r'(/dev/disk[0-9]{1,12})(?:s[0-9]{1,12})*', name or '') if isinstance(name, str) else None
        if match is None:
            raise ValueError('DMG image has an unsupported device identity; preserve inspection work')
        devices.add(match.group(1))
        if name == match.group(1):
            whole.add(name)
    if len(devices) != 1 or devices != whole:
        raise ValueError('DMG image has an ambiguous whole device; preserve inspection work')
    return next(iter(whole))


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
