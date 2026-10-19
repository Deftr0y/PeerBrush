"""Verify candidate bytes and report unmet production gates; never publish anything."""
import argparse
import hashlib
import json
from pathlib import Path
import re

from verify_release import inspect_archive


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def immutable(previous, current):
    if (not isinstance(previous, dict) or type(previous.get('schema')) is not int or previous['schema'] != 1
            or type(previous.get('commit')) is not str or not re.fullmatch('[0-9a-f]{40}', previous['commit'])
            or type(previous.get('version')) is not str
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', previous['version'])):
        raise ValueError('The retained published inventory must have a stable release version.')
    assets = previous.get('assets')
    if (not isinstance(assets, list) or not 1 <= len(assets) <= 64
            or any(not isinstance(a, dict) or not isinstance(a.get('file'), str)
                   or Path(a['file']).name != a['file'] or '\\' in a['file']
                   or type(a.get('sha256')) is not str or not re.fullmatch('[0-9a-f]{64}', a['sha256']) for a in assets)
            or len({a['file'] for a in assets}) != len(assets)):
        raise ValueError('The retained inventory has invalid or duplicate immutable assets.')
    candidate = tuple(map(int, current['version'].split('-')[0].split('.')))
    published = tuple(map(int, previous['version'].split('.')))
    if candidate < published or (candidate == published and '-' in current['version']):
        raise ValueError('A production candidate cannot downgrade the published version.')
    if previous['version'] != current['version']:
        return
    if previous['commit'] != current['commit']:
        raise ValueError('An existing release version is pinned to different source.')
    old = {a['file']: a['sha256'] for a in previous['assets']}
    new = {a['file']: a['sha256'] for a in current['assets']}
    if any(new.get(name) != checksum for name, checksum in old.items()):
        raise ValueError('Existing versioned release files are immutable; no replacement or removal.')


def assess(directory, commit, version, previous=None, source_root=None):
    directory = Path(directory)
    if not re.fullmatch('[0-9a-f]{40}', commit) or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?', version):
        raise ValueError('Expected an exact source SHA and Cargo release version.')
    inventory = json.loads((directory / 'release-inventory.json').read_text(encoding='utf-8'))
    if (type(inventory.get('schema')) is not int or inventory['schema'] != 1 or inventory.get('version') != version
            or inventory.get('commit') != commit or inventory.get('binary_source_commit') != commit
            or inventory.get('packaging_correction') is not None):
        raise ValueError('Production requires one exact source commit, without relabelled older binaries.')
    assets = inventory.get('assets')
    if not isinstance(assets, list) or len(assets) != 4:
        raise ValueError('Production candidates require all three platforms and matching source.')
    names = [a.get('file') for a in assets]
    if any(not isinstance(name, str) or Path(name).name != name or '\\' in name for name in names) or len(set(names)) != 4:
        raise ValueError('Invalid or duplicate candidate filenames.')
    for asset in assets:
        path = directory / asset['file']
        if (type(asset.get('bytes')) is not int or not 0 < asset['bytes'] <= 512 * 1024 * 1024
                or path.is_symlink() or not path.is_file() or path.stat().st_size != asset['bytes']
                or getattr(path.stat(), 'st_file_attributes', 0) & 0x400 or digest(path) != asset.get('sha256')):
            raise ValueError('Candidate content differs from its final inventory.')
    checksums = ''.join(f"{a['sha256']}  {a['file']}\n" for a in assets)
    if (directory / 'SHA256SUMS').read_text(encoding='utf-8') != checksums:
        raise ValueError('Final checksum list differs from the inspected inventory.')
    platforms = {}
    source = (directory / f'PeerBrush-{version}-source.zip').read_bytes()
    for asset in assets:
        if 'platform' not in asset:
            if asset['file'] != f'PeerBrush-{version}-source.zip':
                raise ValueError('Unexpected corresponding-source filename.')
            continue
        inspected = inspect_archive(directory / asset['file'], commit, version, source_root)
        if inspected['record'] != asset or inspected['source'] != source:
            raise ValueError('Final archive differs from its source or inspected record.')
        platform = asset['platform']
        if platform in platforms:
            raise ValueError('Duplicate platform candidate.')
        platforms[platform] = dict(file=asset['file'], sha256=asset['sha256'],
                                   candidate_verified=True, public_availability='unverified',
                                   native_signing=asset['native_publisher_signing'],
                                   signing_verification=asset['signing_verification'],
                                   ready=False)
    if set(platforms) != {'Windows', 'macOS', 'Linux'}:
        raise ValueError('All three exact-source platform candidates are required.')
    if previous is not None:
        immutable(previous, inventory)
    # Existing ZIP inspection is real evidence, but cannot establish installed-
    # package acceptance, trusted native signatures, or protected approval.
    blockers = ['protected-publication-approval-not-configured',
                'windows-installed-distribution-and-signing-not-verified',
                'macos-dmg-native-signing-and-installation-not-verified',
                'linux-installed-distribution-and-update-owner-not-verified',
                'published-assets-and-website-readback-not-configured']
    if previous is None:
        blockers.append('published-version-immutability-not-reconciled')
    if '-' in version:
        blockers.append('prerelease-version')
    return dict(schema=1, version=version, commit=commit,
                candidate_verified=True, production_ready=False,
                immutable_version='checked-against-supplied-inventory' if previous else 'unverified',
                platforms=platforms, blockers=blockers)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--previous-inventory', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--require-ready', action='store_true')
    args = parser.parse_args()
    previous = json.loads(args.previous_inventory.read_text(encoding='utf-8')) if args.previous_inventory else None
    result = assess(args.directory, args.commit, args.version, previous)
    # Candidate reports are immutable too; an interrupted rerun can reuse only
    # byte-identical output, never replace a prior readiness decision silently.
    data = (json.dumps(result, indent=2, sort_keys=True) + '\n').encode()
    try:
        with args.output.open('xb') as stream:
            stream.write(data)
    except FileExistsError:
        if args.output.read_bytes() != data:
            raise ValueError('Refusing to replace an existing readiness report.')
    print('Exact-source candidates verified; production publication remains blocked.')
    return 2 if args.require_ready and not result['production_ready'] else 0


if __name__ == '__main__':
    raise SystemExit(main())
