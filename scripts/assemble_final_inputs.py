"""Acquire fixed upstream data and prepare final inputs; native consumers keep authority.

The offline public verifier/root files require separate approved provisioning.
This helper does not acquire or approve trust, sign, install, publish or submit.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
from urllib.parse import urlsplit
import zipfile

import acquire_macos_qualification_inputs as transport
import final_release as final
from package_deb import deb_version
from package_macos_dmg import bounded_file, private_inspection_directory
import prepare_final_inputs as inputs

POLICIES = {
    'candidate': ('release.yml', 'verified-release-candidate'),
    'macos_signed': ('macos-release-signing.yml', 'macos-signed-pair-'),
    'macos_native': ('macos-qualification.yml', 'macos-native-qualification-'),
    'windows_signed': ('windows-release-signing.yml', 'windows-signed-inputs-'),
    'windows_native': ('windows-qualification.yml', 'windows-native-qualification-'),
    'linux_native': ('linux-qualification.yml', 'linux-native-qualification-'),
}


def selections(value):
    if (not isinstance(value, dict) or set(value) != {'schema', 'final', 'artifacts'}
            or type(value['schema']) is not int or value['schema'] != 1
            or not isinstance(value['artifacts'], dict) or set(value['artifacts']) != set(POLICIES)):
        raise ValueError('Exact fixed upstream artifact selections required.')
    selected, artifacts = value['final'], value['artifacts']
    roles, trusts = inputs.paths(selected)
    if 'linux_native' not in selected or 'native' not in selected['windows']:
        raise ValueError('The upstream producer requires both current Windows and Linux native contracts.')
    for keys, name, _, _ in roles:
        if inputs.get(selected, keys) != name:
            raise ValueError('Upstream file fields must use their fixed portable role locations.')
    for keys, platform in trusts:
        trust = inputs.get(selected, keys)
        if trust['work'] != '@consumer/' + platform or trust['workflow_commit'] != selected['commit']:
            raise ValueError('Upstream trust must retain exact source and fresh-consumer work markers.')
    for role, record in artifacts.items():
        if (not isinstance(record, dict) or set(record) != {'run_id', 'artifact_id', 'artifact_sha256', 'files'}
                or any(not transport.identifier(record[key]) for key in ['run_id', 'artifact_id'])
                or not transport.hexadecimal(record['artifact_sha256'], 64)
                or not isinstance(record['files'], dict) or not 1 <= len(record['files']) <= 8
                or len({name.casefold() for name in record['files'] if type(name) is str}) != len(record['files'])
                or any(not inputs.safe_name(name) or not transport.hexadecimal(checksum, 64)
                       for name, checksum in record['files'].items())):
            raise ValueError('Upstream artifacts require exact bounded ID/outer/inner selections.')
    if len({record['run_id'] for record in artifacts.values()}) != len(POLICIES):
        raise ValueError('Each fixed upstream workflow requires its distinct selected run.')
    version, windows = selected['version'], selected['windows']
    if type(version) is not str or not re.fullmatch(transport.STABLE, version):
        raise ValueError('Upstream producer requires the exact stable version.')
    portable = f'PeerBrush-{version}-Windows-x64.zip'
    candidate = artifacts['candidate']['files']
    expected = {portable, f'PeerBrush-{version}-macOS-arm64.zip', f'PeerBrush-{version}-Linux-x64.zip',
                f'PeerBrush-{version}-source.zip', f'peerbrush_{deb_version(version)}_amd64.deb',
                'release-inventory.json', 'SHA256SUMS', 'production-readiness.json'}
    if set(candidate) != expected:
        raise ValueError('Select the exact ordinary five-record candidate artifact.')
    signed = artifacts['macos_signed']['files']
    archives = [name for name in signed if re.fullmatch('PeerBrush-' + re.escape(version) +
                r'-macOS-arm64-apple-signed-r[1-9][0-9]*\.zip', name)]
    if len(archives) != 1:
        raise ValueError('Select one exact revisioned signed Mac ZIP.')
    archive = archives[0]
    images = [name for name in signed if re.fullmatch(re.escape(archive[:-4]) + r'-dmg-signed-r[1-9][0-9]*\.dmg', name)]
    if len(images) != 1 or set(signed) != {archive, images[0]}:
        raise ValueError('Select the exact two-file signed Mac pair.')
    def exact(role, expected):
        if artifacts[role]['files'] != expected:
            raise ValueError('Upstream role filenames/digests differ from existing final selectors.')
    def native(role, report, selection, extra=None):
        if selection['trust']['run_id'] != artifacts[role]['run_id'] or selection['trust']['run_attempt'] != '1':
            raise ValueError('Native authority and data acquisition must select the same exact first run.')
        exact(role, {report + '.json': selection['qualification_sha256'],
                     report + '.sigstore.json': selection['trust']['bundle_sha256'], **(extra or {})})
    native('macos_native', 'dmg-qualification', selected['macos'])
    native('windows_native', 'windows-installer-qualification', windows['native'],
           {Path(windows['descriptor']).name: windows['descriptor_sha256']})
    native('linux_native', 'linux-deb-qualification', selected['linux_native'],
           {'installation.json': selected['linux_receipt_sha256'], 'portable-to-deb.json': selected['linux_native']['upgrade_sha256']})
    if windows['identity'] != 'windows/direct-identity.json':
        raise ValueError('Existing Windows producer requires its exact public identity filename.')
    exact('windows_signed', {portable: candidate[portable], 'runtime-license.txt': windows['native']['runtime_license_sha256'],
          **{Path(windows[key]).name: windows[key + '_sha256'] for key in ['unsigned', 'package', 'runtime', 'identity']}})
    return selected, roles, trusts, archive, images[0]


def metadata(run, artifact, role, record, commit):
    workflow, name = POLICIES[role]
    workflow = '.github/workflows/' + workflow
    for field in ['repository', 'head_repository']:
        repository = run.get(field) if isinstance(run, dict) else None
        expected = dict(id=transport.REPOSITORY_ID, full_name=transport.REPOSITORY, fork=False, private=False)
        if (not isinstance(repository, dict)
                or any(repository.get(key) != value or type(repository.get(key)) is not type(value) for key, value in expected.items())
                or not isinstance(repository.get('owner'), dict)
                or type(repository['owner'].get('id')) is not int or repository['owner']['id'] != transport.OWNER_ID):
            raise ValueError('Upstream data requires the fixed official public nonfork repository.')
    if (type(run.get('id')) is not int or run['id'] != int(record['run_id'])
            or type(run.get('run_attempt')) is not int or run['run_attempt'] != 1
            or run.get('status') != 'completed' or run.get('conclusion') != 'success'
            or run.get('event') not in ({'push', 'workflow_dispatch'} if role == 'candidate' else {'workflow_dispatch'})
            or run.get('head_branch') != 'main' or run.get('head_sha') != commit or run.get('pull_requests') != []
            or run.get('path') not in {workflow, workflow + '@main'}):
        raise ValueError('Upstream data is not its fixed successful exact first main workflow.')
    if role != 'candidate':
        name += record['run_id'] + '-1'
    expected = dict(id=int(record['artifact_id']), name=name, expired=False,
        digest='sha256:' + record['artifact_sha256'], url='https://api.github.com' + transport.API + 'artifacts/' + record['artifact_id'],
        archive_download_url='https://api.github.com' + transport.API + 'artifacts/' + record['artifact_id'] + '/zip')
    if (not isinstance(artifact, dict)
            or any(artifact.get(key) != value or type(artifact.get(key)) is not type(value) for key, value in expected.items())
            or type(artifact.get('size_in_bytes')) is not int or not 0 < artifact['size_in_bytes'] <= transport.MAX_ARTIFACT):
        raise ValueError('Upstream artifact ID/name/outer digest/size differs.')
    expected = dict(id=int(record['run_id']), repository_id=transport.REPOSITORY_ID,
                    head_repository_id=transport.REPOSITORY_ID, head_branch='main', head_sha=commit)
    origin = artifact.get('workflow_run')
    if not isinstance(origin, dict) or any(origin.get(key) != value or type(origin.get(key)) is not type(value) for key, value in expected.items()):
        raise ValueError('Upstream artifact origin differs from its selected workflow/source.')
    return artifact['size_in_bytes']


def unpack(archive, output, files, remaining, role):
    # Preserve the existing eight-basename extractor; preflight only narrows its
    # total budget before it can create any selected member.
    if archive.stat().st_size < 22:
        raise ValueError('Upstream ZIP lacks its bounded directory.')
    with archive.open('rb') as stream:
        stream.seek(-22, os.SEEK_END)
        end = stream.read(22)
    signature, disk, directory_disk, count_disk, count, size, offset, comment = struct.unpack('<4s4H2IH', end)
    if (signature != b'PK\x05\x06' or disk or directory_disk or count_disk != len(files) or count != len(files)
            or not 0 < size <= 4096 or comment or offset + size != archive.stat().st_size - 22):
        raise ValueError('Upstream ZIP needs its exact small ordinary directory.')
    with zipfile.ZipFile(archive) as source:
        members = source.infolist()
        total = sum(member.file_size for member in members)
        for member in members:
            limit = transport.MAX_FILE
            if role.endswith('_native'):
                limit = inputs.LIMITS['bundle'] if member.filename.endswith('.sigstore.json') else \
                    inputs.MAX_INDEX if member.filename in {'installation.json', 'portable-to-deb.json'} else \
                    inputs.LIMITS['qualification'] if role == 'macos_native' else 16 * 1024
            elif member.filename in {'release-inventory.json', 'SHA256SUMS', 'production-readiness.json'}:
                limit = inputs.MAX_INDEX
            elif member.filename == 'direct-identity.json':
                limit = 16 * 1024
            elif member.filename == 'runtime-license.txt':
                limit = 256 * 1024
            if member.filename not in files or not 0 < member.file_size <= limit:
                raise ValueError('Upstream member exceeds its existing selected role budget.')
    if total > remaining:
        raise ValueError('Upstream extracted selections exceed the remaining aggregate budget.')
    transport.extract(archive, output, files)
    return total


def produce(root, output, work, artifact_output, value, *, authority_directory, token):
    selected, roles, trusts, archive_name, image_name = selections(value)
    final.signed._source(root, selected['commit'])
    authority_directory = private_inspection_directory(authority_directory, root, fresh=False)
    authorities = []
    for keys, platform in trusts:
        trust = inputs.get(selected, keys)
        for key, filename, limit in [('trusted_root', 'trusted_root.jsonl', inputs.LIMITS['trusted-root']), ('cli', 'gh', inputs.LIMITS['gh'])]:
            path = authority_directory / platform / filename
            actual, size = bounded_file(path, limit)
            if actual != trust[key + '_sha256'] or not size:
                raise ValueError('Separately provisioned public authority differs from its independent approved hash.')
            authorities.append(((*keys, key), path, limit, actual, size, platform + '/' + filename))
    work = private_inspection_directory(work, root, fresh=True)
    output = private_inspection_directory(output, root, fresh=True)
    artifact_output = private_inspection_directory(artifact_output, root, fresh=True)
    areas = [work, output, artifact_output, authority_directory]
    if any(a.is_relative_to(b) or b.is_relative_to(a) for n, a in enumerate(areas) for b in areas[n + 1:]):
        raise ValueError('Upstream work, selected output, artifact and provisioned authorities must be separate.')
    def read(route):
        return json.loads(transport.request('api.github.com', route, 1024 * 1024, token=token), object_pairs_hook=final.readiness.unique_fields)
    snapshots = {}
    for role, record in value['artifacts'].items():
        run_route, artifact_route = transport.API + 'runs/' + record['run_id'], transport.API + 'artifacts/' + record['artifact_id']
        run, artifact = read(run_route), read(artifact_route)
        size = metadata(run, artifact, role, record, selected['commit'])
        snapshots[role] = (run_route, artifact_route, run, artifact, size)
    if sum(row[4] for row in snapshots.values()) > inputs.MAX_TOTAL:
        raise ValueError('Selected upstream outer artifacts exceed the existing aggregate budget.')
    work.mkdir(mode=0o700)
    work.chmod(0o700)
    sources, remaining = {}, inputs.MAX_TOTAL - inputs.MAX_INDEX
    for keys, path, limit, checksum, size, name in authorities:
        owned = work / 'authorities' / name
        owned.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if bounded_file(path, min(limit, remaining), owned) != (checksum, size):
            raise ValueError('Public authority changed during exclusive owned copy.')
        remaining -= size
        owned.chmod(0o400)
        sources[keys] = owned
    raw = {}
    for role, record in value['artifacts'].items():
        area = work / role
        area.mkdir(mode=0o700)
        area.chmod(0o700)
        size = snapshots[role][4]
        location = transport.request('api.github.com', snapshots[role][1] + '/zip', 8192, token=token, redirect=True)
        parsed = urlsplit(location)
        wrapper = area / 'selected-artifact.zip'
        transport.request(parsed.hostname, parsed.path + ('?' + parsed.query if parsed.query else ''), size, destination=wrapper)
        wrapper.chmod(0o400)
        if bounded_file(wrapper, transport.MAX_ARTIFACT) != (record['artifact_sha256'], size):
            raise ValueError('Upstream outer bytes differ from their independent digest/size.')
        remaining -= unpack(wrapper, area, record['files'], remaining, role)
        raw[role] = area
    for key in ['unsigned', 'package', 'runtime', 'identity']:
        sources[('windows', key)] = raw['windows_signed'] / Path(selected['windows'][key]).name
    sources[('windows', 'descriptor')] = raw['windows_native'] / Path(selected['windows']['descriptor']).name
    for keys, platform in trusts:
        report = {'macos': 'dmg-qualification', 'windows': 'windows-installer-qualification', 'linux': 'linux-deb-qualification'}[platform]
        sources[(*keys, 'bundle')] = raw[platform + '_native'] / (report + '.sigstore.json')
        qualification_keys = ('macos', 'qualification') if platform == 'macos' else ('windows', 'native', 'qualification') if platform == 'windows' else ('linux_native', 'qualification')
        sources[qualification_keys] = raw[platform + '_native'] / (report + '.json')
    sources[('linux_receipt',)] = raw['linux_native'] / 'installation.json'
    sources[('linux_native', 'upgrade')] = raw['linux_native'] / 'portable-to-deb.json'
    candidate = raw['candidate']
    inventory = inputs.json_input(candidate / 'release-inventory.json', inputs.MAX_INDEX, value['artifacts']['candidate']['files']['release-inventory.json'])
    from prepare_site_release import validate_inventory
    records = validate_inventory(inventory, selected['commit'], selected['version'])
    if len(records) != 5 or set(records) | {'release-inventory.json', 'SHA256SUMS', 'production-readiness.json'} != set(value['artifacts']['candidate']['files']):
        raise ValueError('Upstream candidate must be the exact selected five-record base.')
    windows_portable = f"PeerBrush-{selected['version']}-Windows-x64.zip"
    if bounded_file(candidate / windows_portable, final.LIMIT) != bounded_file(raw['windows_signed'] / windows_portable, final.LIMIT):
        raise ValueError('Windows signed producer substituted the selected candidate portable.')
    identity = inputs.json_input(sources[('windows', 'identity')], 16 * 1024, selected['windows']['identity_sha256'])
    dependency = final.installer.framework_identity(sources[('windows', 'package')], sources[('windows', 'runtime')])
    descriptor = final.installer.descriptor_bytes_for_selection(identity, dependency, final.msix.package_version(selected['version']),
        *[selected['windows'][key] for key in ['descriptor_uri', 'package_uri', 'runtime_uri']])
    if final.read(sources[('windows', 'descriptor')], 16 * 1024, selected['windows']['descriptor_sha256']) != descriptor:
        raise ValueError('Selected actual native descriptor differs from the exact shared serializer.')
    owned = copy.deepcopy(selected)
    for keys, _, _, _ in roles:
        inputs.put(owned, keys, str(sources[keys]))
    for keys, platform in trusts:
        inputs.get(owned, keys)['work'] = str(work / ('final-' + platform))
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    mac = dict(owned['macos']['trust'], work=str(work / 'assemble-macos'))
    deb = next(record for name, record in records.items() if name.endswith('.deb'))
    linux = next(record for name, record in records.items() if name.endswith('-Linux-x64.zip'))
    final.readiness.handoff_dmg(candidate, raw['macos_signed'] / image_name, sources[('macos', 'qualification')],
        output / 'base', selected['commit'], selected['version'], image_sha256=value['artifacts']['macos_signed']['files'][image_name],
        archive_sha256=value['artifacts']['macos_signed']['files'][archive_name], qualification_sha256=selected['macos']['qualification_sha256'],
        team_id=selected['macos']['team_id'], deb_sha256=deb['sha256'], linux_portable_sha256=linux['sha256'],
        maintainer=selected['maintainer'], source_root=root, dmg_trust=mac, signed_archive=raw['macos_signed'] / archive_name)
    selection = output / 'selection.json'
    data = final.encode(owned)
    if not 0 < len(data) <= inputs.MAX_INDEX:
        raise ValueError('Owned final selection exceeds the existing bounded JSON contract.')
    with selection.open('xb') as stream:
        stream.write(data)
    selection.chmod(0o400)
    index = inputs.prepare(root, output / 'base', selection, hashlib.sha256(data).hexdigest(), artifact_output, work / 'bundle')
    for role, record in value['artifacts'].items():
        run_route, artifact_route, run, artifact, size = snapshots[role]
        if read(run_route) != run or read(artifact_route) != artifact or bounded_file(raw[role] / 'selected-artifact.zip', transport.MAX_ARTIFACT) != (record['artifact_sha256'], size):
            raise ValueError('Upstream originals or metadata changed during full preparation.')
        for name, checksum in record['files'].items():
            if bounded_file(raw[role] / name, transport.MAX_FILE)[0] != checksum:
                raise ValueError('Upstream selected inner original changed during preparation.')
    for _, path, limit, checksum, size, _ in authorities:
        if bounded_file(path, limit) != (checksum, size):
            raise ValueError('Provisioned public authority changed during full preparation.')
    for keys, _, limit, checksum in roles:
        if bounded_file(sources[keys], limit)[0] != checksum:
            raise ValueError('Owned upstream final role changed during preparation.')
    if final.read(selection, inputs.MAX_INDEX) != data:
        raise ValueError('Owned final selection changed during preparation.')
    final.signed._source(root, selected['commit'])
    return index


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'output', 'work', 'artifact-output', 'selection']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--selection-sha256', required=True)
    args = parser.parse_args(argv)
    try:
        value = inputs.json_input(args.selection, inputs.MAX_INDEX, args.selection_sha256)
        produce(args.source_root, args.output, args.work, args.artifact_output, value,
            authority_directory=Path(os.environ['RUNNER_TEMP']) / 'peerbrush-final-trust', token=os.environ.pop('GITHUB_TOKEN', ''))
        if inputs.json_input(args.selection, inputs.MAX_INDEX, args.selection_sha256) != value:
            raise ValueError('Reviewed upstream selection changed.')
    except Exception:
        print('Upstream final-input preparation failed; preserve private partial work. No publication occurred.', file=sys.stderr)
        return 1
    print('Final input data retained; installed/public/production/Store readiness remains blocked.')
    return 2


if __name__ == '__main__':
    raise SystemExit(main())
