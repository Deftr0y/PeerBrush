"""Verify candidate bytes and report unmet production gates; never publish anything."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import stat

from verify_release import inspect_archive
from package import regular_file, relative_path
from package_deb import inspect_final


DEB_INSTALLATION_CHECKS = {'installation': 'passed', 'installed-build-identity': 'passed',
                           'same-binary-package-upgrade': 'passed', 'remove-purge-reinstall-retention': 'passed',
                           'mime-and-default-consent': 'passed'}
DEB_INSTALLATION_LIMITS = {'native-gui': 'not-performed', 'real-application-version-migration': 'not-performed',
                           'unsaved-work-and-update-interruption': 'not-performed',
                           'publisher-signature': 'not-performed', 'update-repository': 'not-configured',
                           'public-availability': 'not-assessed'}
MAX_INSTALLATION_RECEIPT_BYTES = 64 * 1024


def unique_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Candidate JSON contains duplicate fields')
        result[key] = value
    return result


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def read_inventory(directory):
    with regular_file(directory, 'release-inventory.json').open('rb') as stream:
        data = stream.read(64 * 1024 + 1)
    if len(data) > 64 * 1024:
        raise ValueError('Candidate inventory exceeds its inspection budget.')
    return data


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


def assess(directory, commit, version, previous=None, source_root=None, *,
           deb_sha256=None, linux_portable_sha256=None, maintainer=None, _inventory_bytes=None):
    directory = Path(directory)
    if not re.fullmatch('[0-9a-f]{40}', commit) or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?', version):
        raise ValueError('Expected an exact source SHA and Cargo release version.')
    if _inventory_bytes is None:
        _inventory_bytes = read_inventory(directory)
    if not isinstance(_inventory_bytes, bytes) or len(_inventory_bytes) > 64 * 1024:
        raise ValueError('Candidate inventory exceeds its inspection budget.')
    inventory = json.loads(_inventory_bytes, object_pairs_hook=unique_fields)
    if (not isinstance(inventory, dict)
            or set(inventory) != {'schema', 'version', 'commit', 'binary_source_commit', 'packaging_correction', 'assets'}
            or type(inventory.get('schema')) is not int or inventory['schema'] != 1 or inventory.get('version') != version
            or inventory.get('commit') != commit or inventory.get('binary_source_commit') != commit
            or inventory.get('packaging_correction') is not None):
        raise ValueError('Production requires one exact source commit, without relabelled older binaries.')
    assets = inventory.get('assets')
    if not isinstance(assets, list) or len(assets) not in (4, 5) or any(not isinstance(a, dict) for a in assets):
        raise ValueError('Candidates require three portable platforms, matching source and at most one final DEB.')
    deb_inputs = [deb_sha256, linux_portable_sha256, maintainer]
    if (len(assets) == 5 and not all(deb_inputs)) or (len(assets) == 4 and any(value is not None for value in deb_inputs)):
        raise ValueError('Five-record candidates require explicit DEB/portable hashes and public maintainer; four-record candidates do not accept DEB inputs.')
    names = [a.get('file') for a in assets]
    if (any(not isinstance(name, str) or len(relative_path(name).parts) != 1 for name in names)
            or len({name.casefold() for name in names}) != len(assets)):
        raise ValueError('Invalid or duplicate candidate filenames.')
    source_name = f'PeerBrush-{version}-source.zip'
    source_assets = [asset for asset in assets if asset['file'] == source_name]
    if len(source_assets) != 1 or set(source_assets[0]) != {'file', 'bytes', 'sha256'}:
        raise ValueError('Expected exactly one strict corresponding-source record before reading source bytes.')
    source_asset = source_assets[0]
    for asset in assets:
        path = regular_file(directory, asset['file'])
        if (type(asset.get('bytes')) is not int or not 0 < asset['bytes'] <= 512 * 1024 * 1024
                or path.is_symlink() or not path.is_file() or path.stat().st_size != asset['bytes']
                or getattr(path.stat(), 'st_file_attributes', 0) & 0x400 or digest(path) != asset.get('sha256')):
            raise ValueError('Candidate content differs from its final inventory.')
    checksums = ''.join(f"{a['sha256']}  {a['file']}\n" for a in assets)
    with regular_file(directory, 'SHA256SUMS').open('rb') as stream:
        actual_checksums = stream.read(64 * 1024 + 1)
    if len(actual_checksums) > 64 * 1024 or actual_checksums.decode('utf-8') != checksums:
        raise ValueError('Final checksum list differs from the inspected inventory.')
    platforms = {}
    with regular_file(directory, source_name).open('rb') as stream:
        source = stream.read(source_asset['bytes'] + 1)
    if len(source) != source_asset['bytes'] or hashlib.sha256(source).hexdigest() != source_asset['sha256']:
        raise ValueError('Corresponding source changed during inspection.')
    deb = None
    linux = None
    for asset in assets:
        if 'platform' not in asset:
            if set(asset) != {'file', 'bytes', 'sha256'} or asset['file'] != source_name:
                raise ValueError('Unexpected corresponding-source filename.')
            continue
        if asset['file'].endswith('.deb'):
            if len(assets) != 5 or deb is not None:
                raise ValueError('Expected exactly one final DEB beside the portable candidates.')
            deb = asset
            continue
        inspected = inspect_archive(directory / asset['file'], commit, version, source_root)
        if inspected['record'] != asset or inspected['source'] != source:
            raise ValueError('Final archive differs from its source or inspected record.')
        platform = asset['platform']
        if platform == 'Linux':
            linux = asset
        if platform in platforms:
            raise ValueError('Duplicate platform candidate.')
        platforms[platform] = dict(file=asset['file'], sha256=asset['sha256'],
                                   candidate_verified=True, public_availability='unverified',
                                   native_signing=asset['native_publisher_signing'],
                                   signing_verification=asset['signing_verification'],
                                   ready=False)
    if set(platforms) != {'Windows', 'macOS', 'Linux'}:
        raise ValueError('All three exact-source platform candidates are required.')
    installed = None
    if len(assets) == 5:
        if deb is None or linux['sha256'] != linux_portable_sha256:
            raise ValueError('Final DEB selection differs from the exact Linux portable inventory record.')
        inspected = inspect_final(source_root or Path(__file__).resolve().parents[1], directory / linux['file'],
                                  directory / deb['file'], commit=commit, version=version,
                                  portable_sha256=linux_portable_sha256, package_sha256=deb_sha256, maintainer=maintainer)
        if inspected['record'] != deb or inspected['record']['source_sha256'] != hashlib.sha256(source).hexdigest():
            raise ValueError('Final DEB differs from its independently inspected record or corresponding source.')
        installed = dict(Linux=dict(record=deb, candidate_verified=True, ready=False, public_availability='unverified',
                                    content_checks=inspected['content_checks'], native_signatures='not-verified',
                                    installed_acceptance='not-established-by-aggregate',
                                    producer_provenance='requires-successful-same-workflow-native-job'))
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
    result = dict(schema=1, version=version, commit=commit,
                candidate_verified=True, production_ready=False,
                immutable_version='checked-against-supplied-inventory' if previous else 'unverified',
                platforms=platforms, blockers=blockers)
    if installed is not None:
        result['installed_candidates'] = installed
    return result


def handoff_path(path, new=False):
    path = Path(path).absolute()
    for candidate in [path, *path.parents]:
        try:
            metadata = candidate.lstat()
        except FileNotFoundError:
            if new and candidate == path:
                continue
            raise ValueError('Candidate handoff requires existing regular inputs and output parent.') from None
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
            raise ValueError('Candidate handoff paths cannot use links or reparse points.')
    return path.parent.resolve(strict=True) / path.name if new else path.resolve(strict=True)


def handoff_deb(directory, package, output, commit, version, *, package_sha256,
                portable_sha256, maintainer, previous=None, source_root=None):
    """Add inspected DEB bytes to a fresh candidate; native/private evidence stays separate."""
    directory, package, output = handoff_path(directory), handoff_path(package), handoff_path(output, new=True)
    if (output.exists() or output.is_relative_to(directory) or directory.is_relative_to(output)
            or package.is_relative_to(output) or not output.parent.is_dir()):
        raise ValueError('Candidate handoff requires fresh, nonoverlapping output; existing files are immutable.')
    inventory_bytes = read_inventory(directory)
    assess(directory, commit, version, source_root=source_root, _inventory_bytes=inventory_bytes)
    inventory = json.loads(inventory_bytes, object_pairs_hook=unique_fields)
    if len(inventory['assets']) != 4 or read_inventory(directory) != inventory_bytes:
        raise ValueError('Handoff requires the unchanged four-record portable candidate.')
    linux = next(asset for asset in inventory['assets'] if asset.get('platform') == 'Linux')
    if linux['sha256'] != portable_sha256:
        raise ValueError('Selected portable hash differs from the verified Linux inventory record.')
    package = regular_file(package.parent, package.name)
    checked = inspect_final(source_root or Path(__file__).resolve().parents[1], directory / linux['file'], package,
                            commit=commit, version=version, portable_sha256=portable_sha256,
                            package_sha256=package_sha256, maintainer=maintainer)
    if package.name.casefold() in {asset['file'].casefold() for asset in inventory['assets']}:
        raise ValueError('Final DEB collides with an existing immutable candidate file.')
    inventory = dict(schema=1, version=version, commit=commit, binary_source_commit=commit,
                     packaging_correction=None, assets=[*inventory['assets'], checked['record']])
    if previous is not None:
        immutable(previous, inventory)
    handoff_path(output, new=True)
    output.mkdir(mode=0o700)
    # Copy this exact list only. Input directories may contain reports, private
    # receipts or unrelated files which have no authority and must stay behind.
    for asset in inventory['assets']:
        handoff_path(output)
        handoff_path(directory)
        handoff_path(package)
        source = package if asset['file'] == package.name else regular_file(directory, asset['file'])
        total = 0
        checksum = hashlib.sha256()
        with source.open('rb') as stream, (output / asset['file']).open('xb') as destination:
            while chunk := stream.read(min(64 * 1024, asset['bytes'] + 1 - total)):
                total += len(chunk)
                if total > asset['bytes']:
                    raise ValueError('Candidate input grew during handoff; preserve partial output and choose a fresh directory.')
                checksum.update(chunk)
                destination.write(chunk)
        if total != asset['bytes'] or checksum.hexdigest() != asset['sha256']:
            raise ValueError('Candidate input changed during handoff; preserve partial output and choose a fresh directory.')
    if (read_inventory(directory) != inventory_bytes or digest(package) != package_sha256
            or digest(directory / linux['file']) != portable_sha256):
        raise ValueError('Selected candidate inputs changed during handoff; preserve partial output.')
    handoff_path(output)
    with (output / 'release-inventory.json').open('x', encoding='utf-8') as stream:
        stream.write(json.dumps(inventory, indent=2) + '\n')
    with (output / 'SHA256SUMS').open('x', encoding='utf-8', newline='\n') as stream:
        stream.write(''.join(f"{asset['sha256']}  {asset['file']}\n" for asset in inventory['assets']))
    result = assess(output, commit, version, previous, source_root, deb_sha256=package_sha256,
                    linux_portable_sha256=portable_sha256, maintainer=maintainer)
    handoff_path(output)
    with (output / 'production-readiness.json').open('x', encoding='utf-8') as stream:
        stream.write(json.dumps(result, indent=2, sort_keys=True) + '\n')
    return result


def assess_deb(root, portable, package, *, commit, version, portable_sha256, package_sha256,
               maintainer, installation_receipt):
    """Bind private same-job evidence to inspected bytes, without authenticating its producer.

    The workflow must run the disposable installation check immediately before
    this consumer. Matching JSON alone cannot prove that native checks occurred.
    """
    if not isinstance(installation_receipt, bytes) or not 0 < len(installation_receipt) <= MAX_INSTALLATION_RECEIPT_BYTES:
        raise ValueError('Expected a bounded private DEB installation receipt')
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
    parser.add_argument('--include-deb', type=Path, help='With --directory, assemble a fresh five-record candidate; --output then names a new directory')
    parser.add_argument('--deb-sha256')
    parser.add_argument('--portable', type=Path)
    parser.add_argument('--portable-sha256')
    parser.add_argument('--maintainer', help='Approved public package-maintainer name <email>')
    parser.add_argument('--installation-receipt', type=Path, help='Private receipt produced immediately beforehand in the same trusted native job')
    parser.add_argument('--commit', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--previous-inventory', type=Path)
    parser.add_argument('--output', type=Path, required=True, help='Readiness JSON file, or a fresh candidate directory only with --include-deb')
    parser.add_argument('--require-ready', action='store_true')
    args = parser.parse_args()
    deb_inputs = [args.deb_sha256, args.portable, args.portable_sha256, args.maintainer, args.installation_receipt]
    if args.deb:
        if not all(deb_inputs) or args.previous_inventory or args.include_deb:
            parser.error('--deb requires --deb-sha256, --portable, --portable-sha256, --maintainer and --installation-receipt; no previous inventory')
        receipt = regular_file(args.installation_receipt.absolute().parent, args.installation_receipt.name)
        with receipt.open('rb') as stream:
            evidence = stream.read(MAX_INSTALLATION_RECEIPT_BYTES + 1)
        result = assess_deb(Path(__file__).resolve().parents[1], args.portable, args.deb,
                            commit=args.commit, version=args.version, portable_sha256=args.portable_sha256,
                            package_sha256=args.deb_sha256, maintainer=args.maintainer, installation_receipt=evidence)
    elif args.include_deb:
        if (not all([args.deb_sha256, args.portable_sha256, args.maintainer])
                or args.portable is not None or args.installation_receipt is not None):
            parser.error('--include-deb requires --directory, explicit DEB/portable hashes and maintainer; no portable path or private receipt')
        previous = json.loads(args.previous_inventory.read_text(encoding='utf-8'), object_pairs_hook=unique_fields) if args.previous_inventory else None
        result = handoff_deb(args.directory, args.include_deb, args.output, args.commit, args.version,
                             package_sha256=args.deb_sha256, portable_sha256=args.portable_sha256,
                             maintainer=args.maintainer, previous=previous)
        print('Five-record candidate content verified; native authority and production publication remain blocked.')
        return 2 if args.require_ready and not result['production_ready'] else 0
    else:
        if args.portable is not None or args.installation_receipt is not None:
            parser.error('Portable paths and private receipts require --deb qualification mode')
        selected = [args.deb_sha256, args.portable_sha256, args.maintainer]
        if any(value is not None for value in selected) and not all(selected):
            parser.error('Five-record inspection requires explicit DEB/portable hashes and maintainer together')
        previous = json.loads(args.previous_inventory.read_text(encoding='utf-8'), object_pairs_hook=unique_fields) if args.previous_inventory else None
        result = assess(args.directory, args.commit, args.version, previous, deb_sha256=args.deb_sha256,
                        linux_portable_sha256=args.portable_sha256, maintainer=args.maintainer)
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
