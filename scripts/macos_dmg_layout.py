"""Native Finder presentation for the drag-to-Applications disk image."""
import hashlib
import plistlib
import struct

BACKGROUND = '.background/installer.png'


def presentation(root, source):
    """Only inert Finder metadata and the exact committed artwork are admitted."""
    store = root / '.DS_Store'
    info = store.lstat()
    if store.is_symlink() or not store.is_file() or not 32 <= info.st_size <= 65536:
        raise ValueError('Missing bounded Finder installation layout')
    data = store.read_bytes()
    if data[:8] != struct.pack('>I4s', 1, b'Bud1'):
        raise ValueError('Invalid Finder layout header')
    background = root / BACKGROUND
    if background.is_symlink() or background.read_bytes() != source.read_bytes():
        raise ValueError('Installer background differs from committed artwork')
    fixed = {}
    for name in ('.metadata_never_index', '.fseventsd/no_log'):
        path = root / name
        if path.is_symlink() or not path.is_file() or path.read_bytes() != b'':
            raise ValueError('Installer indexing policy differs from the selected layout')
        fixed[name] = {'sha256': hashlib.sha256(b'').hexdigest(), 'mode': path.stat().st_mode & 0o777}
    return fixed | {'.fseventsd': {'directory': True}, '.DS_Store': {'sha256': hashlib.sha256(data).hexdigest(), 'mode': info.st_mode & 0o777},
            '.background': {'directory': True},
            BACKGROUND: {'sha256': hashlib.sha256(background.read_bytes()).hexdigest(), 'mode': background.stat().st_mode & 0o777}}


def arrange(image, mount, run, support):
    """Finder writes its own native metadata on the final volume, then closes it."""
    from package_macos_dmg import attached_image_device
    mount.mkdir()
    try:
        attached = run(['/usr/bin/hdiutil', 'attach', str(image), '-mountpoint', str(mount),
                        '-nobrowse', '-noautoopen', '-plist'])
        entities = plistlib.loads(attached.stdout.encode())['system-entities']
        if (not mount.is_mount()
                or [e['mount-point'] for e in entities if 'mount-point' in e] != [str(mount)]):
            raise ValueError('Installer must mount exactly its selected volume')
        run(['/usr/bin/chflags', 'hidden', str(mount / support)])
        script = '''on run argv
set destination to POSIX file (item 1 of argv) as alias
tell application "Finder"
    open destination
    set installerWindow to container window of destination
    set current view of installerWindow to icon view
    set toolbar visible of installerWindow to false
    set statusbar visible of installerWindow to false
    set bounds of installerWindow to {160, 120, 840, 550}
    set options to icon view options of installerWindow
    set arrangement of options to not arranged
    set icon size of options to 104
    set text size of options to 14
    set label position of options to bottom
    set background picture of options to file ".background:installer.png" of destination
    set position of item "PeerBrush.app" of destination to {180, 230}
    set position of item "Applications" of destination to {500, 230}
    update destination without registering applications
    delay 2
    close installerWindow
end tell
end run'''
        run(['/usr/bin/osascript', '-e', script, str(mount)], timeout=60)
        # Read-only DMG roots open in Finder by default. The legacy bless
        # openfolder option is unsupported on Apple Silicon (macOS 12.3+).
        run(['/bin/sync'])
        # Finder may leave these OS-owned empty directories on a writable volume.
        # They contain no application payload; remove only empty directories.
        for name in ('.Trashes', '.fseventsd'):
            path = mount / name
            if path.is_dir() and not path.is_symlink():
                try: path.rmdir()
                except OSError: pass
    finally:
        device = attached_image_device(image)
        if device is not None:
            run(['/usr/bin/hdiutil', 'detach', device], timeout=120)
            if attached_image_device(image) is not None:
                raise ValueError('Installer layout volume remains attached; preserve work')
        if mount.is_mount():
            raise ValueError('Installer mount remains active; preserve work')
        if mount.exists():
            mount.rmdir()
