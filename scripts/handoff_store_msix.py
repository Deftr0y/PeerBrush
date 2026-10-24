"""Bind a Store MSIX to exact candidate bytes without publishing or granting authority."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import tempfile

from package import git, regular_file
from package_macos_dmg import bounded_file, private_inspection_directory
from prepare_site_release import recognize, validate_inventory
from production_readiness import assess, handoff_path, unique_fields
import qualify_store_msix as preflight

FORMAT = 'PeerBrushStoreMsixHandoffV1'
MANIFEST = 'store-candidate-inventory.json'
FLAGS = dict(channel='microsoft-store', package_family_name=preflight.store.FAMILY,
    content_verified=True, common_inventory_binding='bytes-only-not-production-readiness',
    native_sdk_validation='not-performed-by-consumer',
    native_producer_authentication='unverified-requires-trusted-native-job',
    installed_acceptance='not-performed', store_submission='not-performed',
    public_availability='unverified', signing='unsigned-store-submission-input', previous_version_authority='operator-selected-not-service-reconciled',
    published_immutability='not-service-reconciled', ready=False, production_ready=False)


def selected_hash(value):
    if type(value) is not str or not re.fullmatch('[0-9a-f]{64}', value):
        raise ValueError('Select an independent SHA-256 for every selected input.')
    return value


def capture(path, limit):
    path = handoff_path(path)
    with tempfile.TemporaryDirectory(prefix='peerbrush-store-handoff-') as temporary:
        copied = Path(temporary).resolve() / 'input'
        checksum, size = bounded_file(path, limit, copied)
        data = copied.read_bytes()
    return path, data, checksum


def common(directory, root, commit, version, inventory_sha256, windows_sha256, *,
           maintainer, dmg_qualification, dmg_qualification_sha256, macos_team_id):
    directory = handoff_path(directory)
    path, data, checksum = capture(directory / 'release-inventory.json', 64 * 1024)
    if checksum != selected_hash(inventory_sha256):
        raise ValueError('Common inventory differs from its independent SHA-256.')
    inventory = json.loads(data, object_pairs_hook=unique_fields)
    records = validate_inventory(inventory, commit, version)
    qualification_inputs = []
    qualification_bytes = None
    if dmg_qualification is not None:
        qualification_path, qualification_bytes, qualification_hash = capture(dmg_qualification, 64 * 1024)
        if qualification_hash != selected_hash(dmg_qualification_sha256):
            raise ValueError('DMG qualification differs from its independent SHA-256.')
        qualification_inputs.append((qualification_path, qualification_hash, len(qualification_bytes), 64 * 1024))
    deb = next((record for name, record in records.items() if recognize(name, version) == ('deb', 'Linux')), None)
    dmg = next((record for name, record in records.items() if recognize(name, version) == ('dmg', 'macOS')), None)
    linux = next((record for name, record in records.items() if recognize(name, version) == ('zip', 'Linux')), None)
    macos = next((record for name, record in records.items() if recognize(name, version) == ('zip', 'macOS')), None)
    # The independent inventory hash pins every derived package/portable hash.
    assess(directory, commit, version, source_root=root, _inventory_bytes=data,
        deb_sha256=deb['sha256'] if deb else None,
        linux_portable_sha256=linux['sha256'] if deb and linux else None, maintainer=maintainer,
        dmg_sha256=dmg['sha256'] if dmg else None,
        macos_portable_sha256=macos['sha256'] if dmg and macos else None,
        dmg_qualification=qualification_bytes, dmg_qualification_sha256=dmg_qualification_sha256,
        macos_team_id=macos_team_id)
    kinds = {recognize(name, version) for name in records}
    required = {('source', None), ('zip', 'Windows'), ('zip', 'macOS'), ('zip', 'Linux')}
    if len(records) >= 5:
        required.add(('deb', 'Linux'))
    if len(records) == 6:
        required.add(('dmg', 'macOS'))
    if len(records) not in (4, 5, 6) or kinds != required:
        raise ValueError('Expected the strict four/five/six-record common candidate shape.')
    inputs = [(path, checksum, len(data), 64 * 1024), *qualification_inputs]
    for record in records.values():
        asset = handoff_path(regular_file(directory, record['file']))
        actual, size = bounded_file(asset, 512 * 1024 * 1024)
        if (actual, size) != (record['sha256'], record['bytes']):
            raise ValueError('Common candidate bytes differ from the selected inventory.')
        inputs.append((asset, actual, size, 512 * 1024 * 1024))
    sums, sums_data, sums_sha = capture(directory / 'SHA256SUMS', 64 * 1024)
    expected = ''.join(f"{record['sha256']}  {record['file']}\n" for record in inventory['assets']).encode()
    if sums_data.replace(b'\r\n', b'\n') != expected:
        raise ValueError('Common candidate checksums differ from the selected inventory.')
    inputs.append((sums, sums_sha, len(sums_data), 64 * 1024))
    windows = next(record for record in records.values() if recognize(record['file'], version) == ('zip', 'Windows'))
    if windows['sha256'] != selected_hash(windows_sha256):
        raise ValueError('Selected Windows portable differs from the common inventory.')
    portable = directory / windows['file']
    source = records[f'PeerBrush-{version}-source.zip']
    return directory, portable, source['sha256'], inputs


def previous_selection(path, checksum, manifest):
    if (path is None) != (checksum is None):
        raise ValueError('Select previous Store inventory bytes and independent SHA-256 together.')
    if path is None:
        return []
    path, data, actual = capture(path, 64 * 1024)
    if actual != selected_hash(checksum):
        raise ValueError('Previous Store inventory differs from its independent SHA-256.')
    previous = json.loads(data, object_pairs_hook=unique_fields)
    if (not isinstance(previous, dict) or set(previous) != set(manifest)
            or type(previous.get('schema')) is not int or previous['schema'] != 1
            or previous.get('format') != FORMAT
            or any(type(previous.get(key)) is not type(value) or previous[key] != value for key, value in FLAGS.items())
            or type(previous.get('commit')) is not str or not re.fullmatch('[0-9a-f]{40}', previous['commit'])
            or type(previous.get('version')) is not str or type(previous.get('previous_version')) is not str
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', previous['version'])):
        raise ValueError('Unsupported previous Store bundle inventory.')
    for key in ['common_inventory_sha256', 'source_sha256', 'windows_portable_sha256']:
        selected_hash(previous[key])
    package_version = preflight.store.msix.package_version(previous['version'])
    if previous['package_version'] != package_version:
        raise ValueError('Previous Store version is not bound to its canonical application mapping.')
    preflight.store.version(previous['previous_version'])
    preflight.store.msix.package_version(previous['version'], previous=previous['previous_version'])
    names = [f'PeerBrush-{package_version}-Windows-x64.msix' + suffix for suffix in ['', '.json', '.sha256']]
    if (not isinstance(previous['assets'], list) or len(previous['assets']) != 3
            or [asset.get('file') if isinstance(asset, dict) else None for asset in previous['assets']] != names):
        raise ValueError('Previous Store bundle must retain its exact three publication assets.')
    for asset, limit in zip(previous['assets'], [512 * 1024 * 1024, 64 * 1024, 1024]):
        if (set(asset) != {'file', 'bytes', 'sha256'} or type(asset['bytes']) is not int
                or not 0 < asset['bytes'] <= limit):
            raise ValueError('Invalid previous Store asset record.')
        selected_hash(asset['sha256'])
    current_version = tuple(map(int, manifest['version'].split('.')))
    prior_version = tuple(map(int, previous['version'].split('.')))
    if current_version < prior_version:
        raise ValueError('Store bundle cannot downgrade an existing application version.')
    if current_version == prior_version:
        if previous != manifest:
            raise ValueError('Existing Store version assets, source and bindings are immutable.')
    elif preflight.store.version(manifest['previous_version']) < preflight.store.version(package_version):
        raise ValueError('Selected previous Store baseline is older than the retained bundle.')
    return [(path, actual, len(data), 64 * 1024)]


def handoff(root, directory, package, report, output, *, commit, version, previous_version,
            inventory_sha256, portable_sha256, package_sha256, report_sha256,
            previous_inventory=None, previous_inventory_sha256=None, maintainer=None,
            dmg_qualification=None, dmg_qualification_sha256=None, macos_team_id=None):
    root = handoff_path(root)
    output = private_inspection_directory(Path(output).absolute(), root, fresh=True)
    directory = handoff_path(directory)
    if output.is_relative_to(directory) or directory.is_relative_to(output):
        raise ValueError('Store handoff output must not overlap candidate inputs.')
    if (type(commit) is not str or not re.fullmatch('[0-9a-f]{40}', commit)
            or type(version) is not str or type(previous_version) is not str
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version)):
        raise ValueError('Store handoff requires one stable exact source version.')
    for checksum in [inventory_sha256, portable_sha256, package_sha256, report_sha256]:
        selected_hash(checksum)
    directory, portable, source_sha256, inputs = common(directory, root, commit, version,
        inventory_sha256, portable_sha256, maintainer=maintainer, dmg_qualification=dmg_qualification,
        dmg_qualification_sha256=dmg_qualification_sha256, macos_team_id=macos_team_id)
    qualification = preflight.qualify(root, portable, package, report, commit=commit, version=version,
        previous_version=previous_version, portable_sha256=portable_sha256,
        package_sha256=package_sha256, report_sha256=report_sha256)
    package, package_sha256_actual = handoff_path(package), qualification['package']['sha256']
    report, report_bytes, report_actual = capture(report, 64 * 1024)
    if package_sha256_actual != package_sha256 or report_actual != report_sha256:
        raise ValueError('Selected Store inputs changed during qualification.')
    sidecar, sidecar_data, sidecar_sha = capture(package.with_name(package.name + '.sha256'), 1024)
    checksum_bytes = f'{package_sha256}  {package.name}\n'.encode()
    if sidecar_data.replace(b'\r\n', b'\n') != checksum_bytes:
        raise ValueError('Store checksum sidecar differs from the independently selected package.')
    inputs.extend([(package, package_sha256, qualification['package']['bytes'], 512 * 1024 * 1024),
                   (report, report_sha256, len(report_bytes), 64 * 1024),
                   (sidecar, sidecar_sha, len(sidecar_data), 1024)])
    assets = [dict(file=package.name, bytes=qualification['package']['bytes'], sha256=package_sha256),
              dict(file=report.name, bytes=len(report_bytes), sha256=report_sha256),
              dict(file=sidecar.name, bytes=len(checksum_bytes), sha256=hashlib.sha256(checksum_bytes).hexdigest())]
    manifest = dict(schema=1, format=FORMAT, commit=commit, version=version,
        package_version=qualification['package_version'], common_inventory_sha256=inventory_sha256,
        source_sha256=source_sha256, windows_portable_sha256=portable_sha256,
        previous_version=previous_version, assets=assets, **FLAGS)
    inputs.extend(previous_selection(previous_inventory, previous_inventory_sha256, manifest))
    output = private_inspection_directory(output, root, fresh=True)
    output.mkdir(mode=0o700)
    for path, record in [(package, assets[0]), (report, assets[1])]:
        private_inspection_directory(output, root, fresh=False)
        if bounded_file(path, record['bytes'], output / record['file']) != (record['sha256'], record['bytes']):
            raise ValueError('Store input changed during copy; retain partial output and choose a fresh directory.')
    with (output / sidecar.name).open('xb') as stream:
        stream.write(checksum_bytes)
    for path, checksum, size, limit in inputs:
        if bounded_file(path, limit) != (checksum, size):
            raise ValueError('Selected input changed during handoff; retain partial output.')
    if (git(root, 'rev-parse', 'HEAD').decode().strip() != commit
            or git(root, 'status', '--porcelain', '--untracked-files=no').strip()):
        raise ValueError('Selected source changed during handoff.')
    private_inspection_directory(output, root, fresh=False)
    for record in assets:
        if bounded_file(output / record['file'], record['bytes']) != (record['sha256'], record['bytes']):
            raise ValueError('Copied Store bundle bytes changed before final inventory.')
    if {path.name for path in output.iterdir()} != {record['file'] for record in assets}:
        raise ValueError('Store output gained unexpected files; retain partial output.')
    encoded = (json.dumps(manifest, indent=2, sort_keys=True) + '\n').encode()
    if len(encoded) > 16 * 1024:
        raise ValueError('Store bundle inventory exceeds its byte budget.')
    with (output / MANIFEST).open('xb') as stream:
        stream.write(encoded)
    if {path.name for path in output.iterdir()} != {MANIFEST, *[record['file'] for record in assets]}:
        raise ValueError('Store output gained unexpected files; no bundle acceptance established.')
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'directory', 'package', 'report', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ['commit', 'version', 'previous-version', 'inventory-sha256',
                 'portable-sha256', 'package-sha256', 'report-sha256']:
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--maintainer')
    parser.add_argument('--dmg-qualification', type=Path)
    parser.add_argument('--dmg-qualification-sha256')
    parser.add_argument('--macos-team-id')
    parser.add_argument('--previous-inventory', type=Path)
    parser.add_argument('--previous-inventory-sha256')
    args = vars(parser.parse_args())
    root = args.pop('source_root')
    try:
        handoff(root, **args)
    except Exception:
        print('Store auxiliary handoff failed; no publication, authentication or installation performed.', file=sys.stderr)
        return 1
    print('Store content bundle bound; native, installed, Store and public authority remain unverified.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
