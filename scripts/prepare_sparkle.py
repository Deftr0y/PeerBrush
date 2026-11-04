"""Offline immutable Sparkle framework preparation; never starts an updater.

The operator supplies the official archive. No download, key generation,
credential access, application modification, signing or installation occurs.
"""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import stat
import subprocess
import sys
import zipfile

from package import digest, regular_file, relative_path


ROOT = pathlib.Path(__file__).resolve().parents[1]
PIN = ROOT / 'packaging/sparkle/sparkle-2.10.0.json'


def load_pin():
    return json.loads(PIN.read_text(encoding='utf-8'))


def inspect(archive, pin):
    archive = regular_file(archive.absolute().parent, archive.name)
    if archive.stat().st_size != pin['archive_bytes'] or digest(archive) != pin['archive_sha256']:
        raise ValueError('Sparkle archive differs from the committed official checksum')
    with zipfile.ZipFile(archive) as source:
        if sum(entry.file_size for entry in source.infolist()) > 64 * 1024 * 1024:
            raise ValueError('Sparkle archive exceeds the reviewed size budget')
        names, framework = set(), {}
        for entry in source.infolist():
            name = entry.filename.rstrip('/')
            relative_path(name)
            if name.casefold() in names:
                raise ValueError('Sparkle archive has ambiguous duplicate paths')
            names.add(name.casefold())
            if not entry.filename.startswith(pin['framework_prefix']):
                continue
            relative = entry.filename.removeprefix(pin['framework_prefix']).rstrip('/') or '.'
            mode = entry.external_attr >> 16
            if stat.S_ISDIR(mode):
                value = {'kind': 'directory', 'mode': mode & 0o7777}
            elif stat.S_ISLNK(mode):
                value = {'kind': 'symlink', 'target': source.read(entry).decode('utf-8')}
                if value != pin['framework_entries'].get(relative):
                    raise ValueError('Sparkle framework symlink differs from the exact pin')
            elif stat.S_ISREG(mode):
                value = {'kind': 'file', 'mode': mode & 0o7777, 'bytes': entry.file_size,
                         'sha256': hashlib.sha256(source.read(entry)).hexdigest()}
            else:
                raise ValueError('Sparkle framework contains an unsupported entry type')
            framework[relative] = value
        if framework != pin['framework_entries']:
            raise ValueError('Sparkle framework bytes, modes or exact inventory differ from the pin')
        if hashlib.sha256(source.read('LICENSE')).hexdigest() != pin['license_sha256']:
            raise ValueError('Sparkle license differs from the reviewed notice')
    return archive


def external_output(path, root):
    path = path.absolute()
    for ancestor in [path, *path.parents]:
        try:
            metadata = ancestor.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
            raise ValueError('Sparkle output paths must not use links or reparse points')
    roots = {root.resolve()}
    if (root / '.git').exists():
        try:
            worktrees = subprocess.check_output(['git', '-C', str(root), 'worktree', 'list', '--porcelain', '-z'],
                                               stderr=subprocess.DEVNULL).decode('utf-8')
        except (OSError, subprocess.SubprocessError, UnicodeError):
            raise ValueError('Cannot identify every source checkout before preparing private Sparkle output') from None
        roots.update(pathlib.Path(record[9:]).resolve() for record in worktrees.split('\0') if record.startswith('worktree '))
    if any(path.resolve().is_relative_to(checkout) for checkout in roots):
        raise ValueError('Sparkle preparation output must be outside every source checkout')
    if path.exists():
        raise ValueError('Sparkle preparation outputs are immutable; select a fresh external directory')
    return path


def tree(root):
    """Never traverse symlinks when validating staged native code."""
    found = {}
    def visit(path, name):
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            if not path.resolve(strict=True).is_relative_to(root.resolve(strict=True)):
                raise ValueError('Sparkle symlink escapes its framework')
            found[name] = {'kind': 'symlink', 'target': os.readlink(path)}
        elif stat.S_ISDIR(metadata.st_mode):
            found[name] = {'kind': 'directory', 'mode': metadata.st_mode & 0o7777}
            for child in path.iterdir():
                visit(child, child.relative_to(root).as_posix())
        elif stat.S_ISREG(metadata.st_mode):
            if metadata.st_nlink != 1:
                raise ValueError('Sparkle staged files must not be hard links')
            found[name] = {'kind': 'file', 'mode': metadata.st_mode & 0o7777,
                           'bytes': metadata.st_size, 'sha256': digest(path)}
        else:
            raise ValueError('Sparkle staged code contains a special entry')
    visit(root, '.')
    return found


def verify_stage(output, pin):
    if {path.name for path in output.iterdir()} != {'Sparkle.framework', 'LICENSE', 'preparation.json'}:
        raise ValueError('Unexpected Sparkle preparation output inventory')
    if tree(output / 'Sparkle.framework') != pin['framework_entries']:
        raise ValueError('Staged Sparkle framework differs from the committed pin')
    if digest(regular_file(output, 'LICENSE')) != pin['license_sha256']:
        raise ValueError('Staged Sparkle notice differs from the committed pin')
    if json.loads(regular_file(output, 'preparation.json').read_text(encoding='utf-8')) != receipt(pin):
        raise ValueError('Sparkle preparation receipt differs from the disabled exact pin')


def receipt(pin):
    return {'schema': 1, 'sparkle_version': pin['version'], 'source_commit': pin['source_commit'],
            'archive_sha256': pin['archive_sha256'], 'pin_sha256': digest(PIN),
            'updater_started': False, 'update_installation_enabled': False,
            'release_ready': False, 'native_probe': 'not-performed',
            'signing_and_notarization': 'not-performed', 'feed': 'unconfigured', 'public_key': 'unconfigured'}


def prepare(archive, output, root=ROOT):
    pin = load_pin()
    # All bytes and archive paths are checked before creating an output.
    archive = inspect(archive, pin)
    if sys.platform != 'darwin' or int(platform.mac_ver()[0].split('.')[0] or 0) < 12:
        raise ValueError('Native Sparkle preparation requires macOS 12 or later; the disabled core app still supports macOS 11')
    output = external_output(output, root)
    output.mkdir(parents=True, mode=0o700)
    os.chmod(output, 0o700)
    framework = output / 'Sparkle.framework'
    # Write directories and regular files before creating only the exact pinned
    # framework links. No archive extraction API follows attacker-controlled links.
    with zipfile.ZipFile(archive) as source:
        for name, entry in pin['framework_entries'].items():
            if entry['kind'] == 'symlink':
                continue
            path = framework if name == '.' else framework / relative_path(name)
            if entry['kind'] == 'directory':
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open('xb') as stream:
                    stream.write(source.read(pin['framework_prefix'] + name))
            path.chmod(entry['mode'])
        for name, entry in pin['framework_entries'].items():
            if entry['kind'] == 'symlink':
                path = framework / relative_path(name)
                path.symlink_to(entry['target'])
                # Darwin applies umask to symlink modes; restore only the owned link.
                if stat.S_IMODE(path.lstat().st_mode) != 0o777:
                    path.chmod(0o777, follow_symlinks=False)
                if stat.S_IMODE(path.lstat().st_mode) != 0o777:
                    raise ValueError('Staged Sparkle link mode differs from exact pin')
        with (output / 'LICENSE').open('xb') as stream:
            stream.write(source.read('LICENSE'))
    with (output / 'preparation.json').open('x', encoding='utf-8') as stream:
        json.dump(receipt(pin), stream, indent=2)
        stream.write('\n')
    verify_stage(output, pin)
    if digest(archive) != pin['archive_sha256']:
        raise ValueError('Sparkle input changed during preparation; retain failed external work for inspection')
    return output


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', required=True, type=pathlib.Path)
    parser.add_argument('--output', type=pathlib.Path)
    parser.add_argument('--native-audit-output', type=pathlib.Path, help='Fresh private directory for bounded native policy facts; no signing, loading or activation')
    parser.add_argument('--verify-only', action='store_true', help='Verify the exact official archive without staging or native calls')
    args = parser.parse_args()
    try:
        if args.native_audit_output is not None:
            if args.verify_only or args.output is not None:
                raise ValueError('Select exactly one Sparkle preparation mode')
            from sparkle_bundle import audit_native
            from sign_macos import run
            output = external_output(args.native_audit_output, ROOT)
            facts = audit_native(args.archive, output, run)
            with (output / 'native-policy-candidate.json').open('x', encoding='utf-8') as stream:
                json.dump(facts, stream, indent=2, sort_keys=True)
                stream.write('\n')
            print('Native policy facts prepared for independent review; no authority, signing, loading or updater activation')
        elif args.verify_only:
            inspect(args.archive, load_pin())
            print('Verified the pinned Sparkle archive; no code loaded or updater started')
        else:
            if args.output is None:
                raise ValueError('Select a fresh external --output directory for native preparation')
            prepare(args.archive, args.output)
            print('Prepared immutable disabled Sparkle framework outside the checkout; native probe still required')
    except (ValueError, OSError, KeyError, zipfile.BadZipFile) as error:
        parser.exit(1, (str(error) if isinstance(error, ValueError) else 'Sparkle preparation input/output validation failed') + '\n')
