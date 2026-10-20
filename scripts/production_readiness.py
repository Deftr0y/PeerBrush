"""Verify candidate bytes and report unmet production gates; never publish anything."""
import argparse
import hashlib
import json
from pathlib import Path
import re

from verify_release import inspect_archive
from package import regular_file
from package_deb import inspect_final


DEB_INSTALLATION_CHECKS = {'installation': 'passed', 'installed-build-identity': 'passed',
                           'same-binary-package-upgrade': 'passed', 'remove-purge-reinstall-retention': 'passed',
                           'mime-and-default-consent': 'passed'}
DEB_INSTALLATION_LIMITS = {'native-gui': 'not-performed', 'real-application-version-migration': 'not-performed',
                           'unsaved-work-and-update-interruption': 'not-performed',
                           'publisher-signature': 'not-performed', 'update-repository': 'not-configured',
                           'public-availability': 'not-assessed'}
MAX_INSTALLATION_RECEIPT_BYTES = 64 * 1024


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


def assess_deb(root, portable, package, *, commit, version, portable_sha256, package_sha256,
               maintainer, installation_receipt):
    """Bind private same-job evidence to inspected bytes, without authenticating its producer.

    The workflow must run the disposable installation check immediately before
    this consumer. Matching JSON alone cannot prove that native checks occurred.
    """
    if not isinstance(installation_receipt, bytes) or not 0 < len(installation_receipt) <= MAX_INSTALLATION_RECEIPT_BYTES:
        raise ValueError('Expected a bounded private DEB installation receipt')
    def unique_fields(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('DEB installation receipt contains duplicate fields')
            result[key] = value
        return result
    try:
        receipt = json.loads(installation_receipt.decode('utf-8'), object_pairs_hook=unique_fields)
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise ValueError('DEB installation receipt is not bounded valid JSON') from error
    inspected = inspect_final(root, portable, package, commit=commit, version=version,
                              portable_sha256=portable_sha256, package_sha256=package_sha256, maintainer=maintainer)
    record = inspected['record']
    expected = dict(schema=1, format='PeerBrushDebInstallationV1', product='PeerBrush', version=version, commit=commit,
                    package=dict(file=record['file'], sha256=record['sha256']),
                    executable_sha256=inspected['executable_sha256'], source_sha256=record['source_sha256'],
                    target='Ubuntu 24.04 amd64', architecture='amd64', checks=DEB_INSTALLATION_CHECKS,
                    limits=DEB_INSTALLATION_LIMITS, release_ready=False, production_readiness='not-assessed')
    if (not isinstance(receipt, dict) or type(receipt.get('schema')) is not int
            or receipt.get('release_ready') is not False or receipt != expected):
        raise ValueError('DEB installation receipt differs from the selected package, source, checks or explicit limits')
    return dict(schema=1, format='PeerBrushDebQualificationV1', version=version, commit=commit,
                record=record, candidate_verified=True, installation_receipt_binding='verified',
                installation_receipt_provenance='requires-trusted-same-job-producer',
                installation_checks=dict(DEB_INSTALLATION_CHECKS), content_checks=inspected['content_checks'],
                public_availability='unverified', ready=False, production_ready=False,
                limits=dict(DEB_INSTALLATION_LIMITS), native_dependencies='syntax-and-required-runtime-only',
                blockers=['protected-publication-approval-not-configured', 'native-gui-not-verified',
                          'real-version-migration-and-interrupted-update-not-verified',
                          'native-shlibdeps-requires-trusted-native-build', 'publisher-signature-not-verified',
                          'update-repository-not-configured', 'published-assets-and-website-readback-not-configured'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--directory', type=Path)
    mode.add_argument('--deb', type=Path)
    parser.add_argument('--deb-sha256')
    parser.add_argument('--portable', type=Path)
    parser.add_argument('--portable-sha256')
    parser.add_argument('--maintainer', help='Approved public package-maintainer name <email>')
    parser.add_argument('--installation-receipt', type=Path, help='Private receipt produced immediately beforehand in the same trusted native job')
    parser.add_argument('--commit', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--previous-inventory', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--require-ready', action='store_true')
    args = parser.parse_args()
    deb_inputs = [args.deb_sha256, args.portable, args.portable_sha256, args.maintainer, args.installation_receipt]
    if args.deb:
        if not all(deb_inputs) or args.previous_inventory:
            parser.error('--deb requires --deb-sha256, --portable, --portable-sha256, --maintainer and --installation-receipt; no previous inventory')
        receipt = regular_file(args.installation_receipt.absolute().parent, args.installation_receipt.name)
        with receipt.open('rb') as stream:
            evidence = stream.read(MAX_INSTALLATION_RECEIPT_BYTES + 1)
        result = assess_deb(Path(__file__).resolve().parents[1], args.portable, args.deb,
                            commit=args.commit, version=args.version, portable_sha256=args.portable_sha256,
                            package_sha256=args.deb_sha256, maintainer=args.maintainer, installation_receipt=evidence)
    else:
        if any(value is not None for value in deb_inputs):
            parser.error('DEB qualification inputs require --deb')
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
