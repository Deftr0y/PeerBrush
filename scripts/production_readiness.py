"""Verify candidate bytes and report unmet production gates; never publish anything."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat

from verify_release import inspect_archive
from package import regular_file, relative_path
from package_deb import inspect_final
from package_macos_dmg import bounded_file, private_inspection_directory


DEB_INSTALLATION_CHECKS = {'installation': 'passed', 'installed-build-identity': 'passed',
                           'same-binary-package-upgrade': 'passed', 'remove-purge-reinstall-retention': 'passed',
                           'mime-and-default-consent': 'passed'}
DEB_INSTALLATION_LIMITS = {'native-gui': 'not-performed', 'real-application-version-migration': 'not-performed',
                           'unsaved-work-and-update-interruption': 'not-performed',
                           'publisher-signature': 'not-performed', 'update-repository': 'not-configured',
                           'public-availability': 'not-assessed'}
MAX_INSTALLATION_RECEIPT_BYTES = 64 * 1024
MAX_DMG_QUALIFICATION_BYTES = 8192


def _publication_phases(blockers):
    # Informational partition only. Unknown checks remain before publication;
    # neither list grants approval or changes any existing readiness gate.
    post = 'published-assets-and-website-readback-not-configured'
    return dict(prepublication_blockers=[value for value in blockers if value != post],
                postpublication_checks=[value for value in blockers if value == post])


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
           deb_sha256=None, linux_portable_sha256=None, maintainer=None, _inventory_bytes=None,
           dmg_sha256=None, macos_portable_sha256=None, dmg_qualification=None,
           dmg_qualification_sha256=None, macos_team_id=None, dmg_trust=None):
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
    if not isinstance(assets, list) or len(assets) not in (4, 5, 6) or any(not isinstance(a, dict) for a in assets):
        raise ValueError('Candidates require three portable platforms, source, optional DEB and final DMG.')
    deb_inputs = [deb_sha256, linux_portable_sha256, maintainer]
    if (len(assets) >= 5 and not all(deb_inputs)) or (len(assets) == 4 and any(value is not None for value in deb_inputs)):
        raise ValueError('Five/six-record candidates require explicit DEB/portable hashes and public maintainer; four-record candidates do not accept DEB inputs.')
    dmg_inputs = [dmg_sha256, macos_portable_sha256, dmg_qualification, dmg_qualification_sha256, macos_team_id]
    if (len(assets) == 6 and not all(dmg_inputs)) or (len(assets) != 6 and any(value is not None for value in dmg_inputs)):
        raise ValueError('Six-record candidates require explicit DMG/signed ZIP/qualification hashes, qualification bytes and publisher Team; other candidates cannot accept DMG inputs.')
    if dmg_trust is not None and (len(assets) != 6 or not isinstance(dmg_trust, dict)
            or set(dmg_trust) - {'readback_job'} != {'bundle', 'bundle_sha256', 'trusted_root', 'trusted_root_sha256', 'cli',
                                  'cli_sha256', 'cli_version', 'workflow_commit', 'run_id', 'run_attempt', 'work'}):
        raise ValueError('Native provenance verification requires all independently reviewed inputs with six-record DMG inspection.')
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
    if len(actual_checksums) > 64 * 1024 or actual_checksums.replace(b'\r\n', b'\n') != checksums.encode('utf-8'):
        raise ValueError('Final checksum list differs from the inspected inventory.')
    platforms = {}
    with regular_file(directory, source_name).open('rb') as stream:
        source = stream.read(source_asset['bytes'] + 1)
    if len(source) != source_asset['bytes'] or hashlib.sha256(source).hexdigest() != source_asset['sha256']:
        raise ValueError('Corresponding source changed during inspection.')
    deb = None
    linux = None
    dmg = None
    macos = None
    for asset in assets:
        if 'platform' not in asset:
            if set(asset) != {'file', 'bytes', 'sha256'} or asset['file'] != source_name:
                raise ValueError('Unexpected corresponding-source filename.')
            continue
        if asset['file'].endswith('.deb'):
            if len(assets) < 5 or deb is not None:
                raise ValueError('Expected exactly one final DEB beside the portable candidates.')
            deb = asset
            continue
        if asset['file'].endswith('.dmg'):
            if len(assets) != 6 or dmg is not None:
                raise ValueError('Expected exactly one selected final DMG in six-record candidates.')
            dmg = asset
            continue
        inspected = inspect_archive(directory / asset['file'], commit, version, source_root)
        if inspected['record'] != asset or inspected['source'] != source:
            raise ValueError('Final archive differs from its source or inspected record.')
        platform = asset['platform']
        if platform == 'Linux':
            linux = asset
        if platform == 'macOS':
            macos = asset
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
    if len(assets) >= 5:
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
    if len(assets) == 6:
        if dmg is None or macos['sha256'] != macos_portable_sha256:
            raise ValueError('Final DMG selection differs from the selected signed Mac portable record.')
        bound = bind_dmg(source_root or Path(__file__).resolve().parents[1], directory / macos['file'], directory / dmg['file'],
                         commit=commit, version=version, archive_sha256=macos_portable_sha256, image_sha256=dmg_sha256,
                         qualification=dmg_qualification, qualification_sha256=dmg_qualification_sha256, team_id=macos_team_id)
        if bound['record'] != dmg:
            raise ValueError('Final DMG inventory differs from its selected bytes, source or strict public record.')
        if dmg_trust is not None:
            from verify_macos_qualification import authenticate
            bound['native_origin'] = authenticate(dmg_qualification, qualification_sha256=dmg_qualification_sha256,
                commit=commit, version=version, source_root=source_root or Path(__file__).resolve().parents[1], **dmg_trust)
            bound['native_producer_authentication'] = bound['native_origin']['native_producer_authentication']
            bound['native_job_conclusion'] = bound['native_origin']['native_job_conclusion']
        installed['macOS'] = bound
    if previous is not None:
        immutable(previous, inventory)
        if (dmg is not None and previous['version'] == version
                and {asset['file']: asset['sha256'] for asset in previous['assets']}
                != {asset['file']: asset['sha256'] for asset in inventory['assets']}):
            raise ValueError('A published version cannot gain or replace final inventory assets; select a new version.')
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
    if dmg is not None:
        if dmg_trust is None:
            blockers.append('macos-native-job-provenance-unverified')
        elif installed['macOS']['native_job_conclusion'] != 'service-reconciled-success':
            blockers.append('macos-native-job-conclusion-not-service-reconciled')
    result = dict(schema=1, version=version, commit=commit,
                candidate_verified=dmg is None, production_ready=False,
                immutable_version='checked-against-supplied-inventory' if previous else 'unverified',
                platforms=platforms, blockers=blockers)
    if installed is not None:
        result['installed_candidates'] = installed
    if dmg is not None:
        result['asset_binding_verified'] = True
    result['publication_phases'] = _publication_phases(blockers)
    return result


def bind_dmg(root, archive, image, *, commit, version, archive_sha256, image_sha256,
             qualification, qualification_sha256, team_id):
    """Bind independently selected native claims to bytes; JSON cannot authenticate execution."""
    if (not isinstance(version, str) or len(version) > 32
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version)
            or not isinstance(commit, str) or not re.fullmatch('[0-9a-f]{40}', commit)
            or not isinstance(team_id, str) or not re.fullmatch('[A-Z0-9]{10}', team_id)
            or any(not isinstance(value, str) or not re.fullmatch('[0-9a-f]{64}', value)
                   for value in [archive_sha256, image_sha256, qualification_sha256])
            or not isinstance(qualification, bytes) or not 0 < len(qualification) <= MAX_DMG_QUALIFICATION_BYTES
            or hashlib.sha256(qualification).hexdigest() != qualification_sha256):
        raise ValueError('Final DMG binding requires explicit stable source, independent hashes and bounded qualification bytes.')
    archive, image = handoff_path(archive), handoff_path(image)
    if (not re.fullmatch('PeerBrush-' + re.escape(version) + r'-macOS-arm64-apple-signed-r[1-9][0-9]*\.zip', archive.name)
            or not re.fullmatch(re.escape(archive.stem) + r'-dmg-signed-r[1-9][0-9]*\.dmg', image.name)
            or bounded_file(archive, 512 * 1024 * 1024)[0] != archive_sha256):
        raise ValueError('Final DMG requires the explicitly selected signed ZIP, not an unsigned replacement.')
    image_hash, image_bytes = bounded_file(image, 512 * 1024 * 1024)
    if image_hash != image_sha256 or image_bytes == 0:
        raise ValueError('Final DMG bytes differ from the independently selected SHA256.')
    checked = inspect_archive(archive, commit, version, root)
    manifest = checked['manifest']
    if (checked['packaging_correction'] or checked['record']['binary_source_commit'] != commit
            or checked['record']['sha256'] != archive_sha256 or manifest['platform'] != 'macOS'
            or manifest['architecture'] != 'arm64' or not checked['signing_suffix']
            or manifest['native_publisher_signing'] != 'developer-id-notarized'
            or manifest['macos_signing']['team_id'] != team_id):
        raise ValueError('Signed ZIP source or publisher metadata differs from the selected candidate.')
    record = dict(file=image.name, bytes=image_bytes, sha256=image_sha256, platform='macOS', architecture='arm64',
                  binary_source_commit=commit, source_sha256=manifest['source_sha256'])
    expected = dict(schema=1, format='PeerBrushDmgQualificationV1', version=version, commit=commit,
                    record=record, signed_zip=dict(file=archive.name, sha256=archive_sha256),
                    executable_sha256=manifest['executable_sha256'], publisher_team_id=team_id,
                    candidate_verified=True, ready=False, production_ready=False, public_availability='unverified',
                    content_checks=['exact-source-signed-zip-and-corresponding-source', 'exact-mounted-files-modes-and-Applications-link'],
                    native_checks=['Developer-ID-publisher-and-secure-timestamps', 'app-hardened-runtime-and-empty-entitlements',
                                   'image-and-app-stapled-tickets-and-Gatekeeper', 'hdiutil-integrity-and-read-only-mount'],
                    limits={'drag-to-Applications-and-launch': 'not-performed', 'fresh-quarantined-download': 'not-performed',
                            'upgrade-settings-and-unsaved-work': 'not-performed', 'public-availability': 'not-assessed'},
                    native_job_provenance='requires-trusted-native-job-at-consumption')
    try:
        actual = json.loads(qualification.decode('utf-8'), object_pairs_hook=unique_fields)
        # Canonical serialization distinguishes bool/int/float, unlike Python
        # equality. Only this exact bounded producer schema is supported.
        if json.dumps(actual, sort_keys=True, allow_nan=False) != json.dumps(expected, sort_keys=True):
            raise ValueError('mismatch')
    except (ValueError, UnicodeError, RecursionError):
        raise ValueError('DMG qualification differs from the exact selected bytes, source, schema or limits.') from None
    if bounded_file(archive, 512 * 1024 * 1024)[0] != archive_sha256 or bounded_file(image, 512 * 1024 * 1024)[0] != image_sha256:
        raise ValueError('DMG or signed ZIP changed during qualification binding.')
    return dict(record=record, bytes_verified=True, candidate_verified=False, ready=False, public_availability='unverified',
                signed_zip=dict(file=archive.name, sha256=archive_sha256), qualification_sha256=qualification_sha256,
                qualification_binding='exact-schema-and-independent-digest-matched',
                native_verification='not-performed-by-aggregate',
                native_producer_authentication='unverified-requires-trusted-native-job',
                installed_acceptance='not-established-by-aggregate')


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


def read_previous_inventory(path):
    path = handoff_path(path)
    with regular_file(path.parent, path.name).open('rb') as stream:
        data = stream.read(64 * 1024 + 1)
    if len(data) > 64 * 1024:
        raise ValueError('Previous inventory exceeds its byte budget.')
    return json.loads(data, object_pairs_hook=unique_fields)


def read_dmg_qualification(path):
    path = handoff_path(path)
    with regular_file(path.parent, path.name).open('rb') as stream:
        data = stream.read(MAX_DMG_QUALIFICATION_BYTES + 1)
    if not 0 < len(data) <= MAX_DMG_QUALIFICATION_BYTES:
        raise ValueError('DMG qualification exceeds its bounded report contract.')
    return data


def handoff_dmg(directory, image, qualification_path, output, commit, version, *, image_sha256,
                archive_sha256, qualification_sha256, team_id, deb_sha256, linux_portable_sha256,
                maintainer, previous=None, source_root=None, dmg_trust=None):
    """Append the selected DMG; never swap an existing Mac ZIP or trust native claims."""
    root = source_root or Path(__file__).resolve().parents[1]
    directory, image, qualification_path = handoff_path(directory), handoff_path(image), handoff_path(qualification_path)
    output = private_inspection_directory(output, root, fresh=True)
    if (output.is_relative_to(directory) or directory.is_relative_to(output)
            or any(output.is_relative_to(path.parent) for path in [image, qualification_path])):
        raise ValueError('DMG handoff requires fresh output separate from candidate and native evidence directories.')
    inventory_bytes = read_inventory(directory)
    assess(directory, commit, version, source_root=root, deb_sha256=deb_sha256,
           linux_portable_sha256=linux_portable_sha256, maintainer=maintainer, _inventory_bytes=inventory_bytes)
    inventory = json.loads(inventory_bytes, object_pairs_hook=unique_fields)
    if len(inventory['assets']) != 5:
        raise ValueError('DMG handoff requires a five-record candidate already selecting its signed Mac ZIP and DEB.')
    macos = next(asset for asset in inventory['assets'] if asset.get('platform') == 'macOS')
    if macos['sha256'] != archive_sha256:
        raise ValueError('Selected signed ZIP hash differs from the existing immutable Mac inventory record.')
    qualification = read_dmg_qualification(qualification_path)
    bound = bind_dmg(root, directory / macos['file'], image, commit=commit, version=version,
                     archive_sha256=archive_sha256, image_sha256=image_sha256,
                     qualification=qualification, qualification_sha256=qualification_sha256, team_id=team_id)
    if image.name.casefold() in {asset['file'].casefold() for asset in inventory['assets']}:
        raise ValueError('Final DMG collides with an existing immutable candidate file.')
    final = dict(schema=1, version=version, commit=commit, binary_source_commit=commit,
                 packaging_correction=None, assets=[*inventory['assets'], bound['record']])
    if previous is not None:
        immutable(previous, final)
        if (previous['version'] == version
                and {asset['file']: asset['sha256'] for asset in previous['assets']}
                != {asset['file']: asset['sha256'] for asset in final['assets']}):
            raise ValueError('A published version cannot gain or replace final inventory assets; select a new version.')
    if read_inventory(directory) != inventory_bytes or read_dmg_qualification(qualification_path) != qualification:
        raise ValueError('Candidate inventory or qualification changed before handoff.')
    private_inspection_directory(output, root, fresh=True)
    output.mkdir(mode=0o700)
    os.chmod(output, 0o700)
    # No producer report, signing journal, private receipt or unlisted sidecar is
    # copied. Every selected byte is bounded and checked during exclusive copy.
    for asset in final['assets']:
        private_inspection_directory(output, root, fresh=False)
        source = image if asset['file'] == image.name else regular_file(directory, asset['file'])
        checksum, size = bounded_file(source, 512 * 1024 * 1024, output / asset['file'])
        if size != asset['bytes'] or checksum != asset['sha256']:
            raise ValueError('Selected input changed during DMG handoff; preserve partial output.')
    if read_inventory(directory) != inventory_bytes or read_dmg_qualification(qualification_path) != qualification:
        raise ValueError('Candidate inventory or qualification changed during DMG handoff; preserve partial output.')
    for asset in final['assets']:
        source = image if asset['file'] == image.name else regular_file(directory, asset['file'])
        if bounded_file(source, 512 * 1024 * 1024) != (asset['sha256'], asset['bytes']):
            raise ValueError('Selected input changed after DMG handoff; preserve partial output.')
    private_inspection_directory(output, root, fresh=False)
    with (output / 'release-inventory.json').open('x', encoding='utf-8', newline='\n') as stream:
        stream.write(json.dumps(final, indent=2) + '\n')
    with (output / 'SHA256SUMS').open('x', encoding='utf-8', newline='\n') as stream:
        stream.write(''.join(f"{asset['sha256']}  {asset['file']}\n" for asset in final['assets']))
    result = assess(output, commit, version, previous, root, deb_sha256=deb_sha256,
                    linux_portable_sha256=linux_portable_sha256, maintainer=maintainer,
                    dmg_sha256=image_sha256, macos_portable_sha256=archive_sha256,
                    dmg_qualification=qualification, dmg_qualification_sha256=qualification_sha256, macos_team_id=team_id,
                    dmg_trust=dmg_trust)
    private_inspection_directory(output, root, fresh=False)
    with (output / 'production-readiness.json').open('x', encoding='utf-8', newline='\n') as stream:
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
    result = dict(schema=1, format='PeerBrushDebQualificationV1', version=version, commit=commit,
                record=record, candidate_verified=True, installation_receipt_binding='verified',
                installation_receipt_provenance='requires-trusted-same-job-producer',
                installation_checks=dict(DEB_INSTALLATION_CHECKS), content_checks=inspected['content_checks'],
                public_availability='unverified', ready=False, production_ready=False,
                limits=dict(DEB_INSTALLATION_LIMITS), native_dependencies='syntax-and-required-runtime-only',
                blockers=['protected-publication-approval-not-configured', 'native-gui-not-verified',
                          'real-version-migration-and-interrupted-update-not-verified',
                          'native-shlibdeps-requires-trusted-native-build', 'publisher-signature-not-verified',
                          'update-repository-not-configured', 'published-assets-and-website-readback-not-configured'])
    result['publication_phases'] = _publication_phases(result['blockers'])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--directory', type=Path)
    mode.add_argument('--deb', type=Path)
    parser.add_argument('--include-deb', type=Path, help='With --directory, assemble a fresh five-record candidate; --output then names a new directory')
    parser.add_argument('--include-dmg', type=Path, help='Append a selected final DMG to a five-record signed-ZIP/DEB candidate; output is a fresh external directory')
    parser.add_argument('--dmg-sha256')
    parser.add_argument('--macos-portable-sha256')
    parser.add_argument('--dmg-qualification', type=Path, help='External standalone native qualification; its JSON never authenticates native execution')
    parser.add_argument('--dmg-qualification-sha256')
    parser.add_argument('--macos-team-id')
    for name in ['native-attestation-bundle', 'native-trusted-root', 'native-gh', 'native-verification-work']:
        parser.add_argument('--' + name, type=Path)
    for name in ['native-attestation-sha256', 'native-trusted-root-sha256', 'native-gh-sha256',
                 'native-gh-version', 'native-workflow-commit', 'native-run-id', 'native-run-attempt']:
        parser.add_argument('--' + name)
    parser.add_argument('--native-job-readback', action='store_true',
                        help='After native attestation authentication, read only its exact public run attempt and jobs; default is offline')
    parser.add_argument('--source-root', type=Path, help='Exact selected release source checkout; defaults to this script checkout')
    parser.add_argument('--deb-sha256')
    parser.add_argument('--portable', type=Path)
    parser.add_argument('--portable-sha256')
    parser.add_argument('--maintainer', help='Approved public package-maintainer name <email>')
    parser.add_argument('--installation-receipt', type=Path, help='Private receipt produced immediately beforehand in the same trusted native job')
    parser.add_argument('--commit', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--previous-inventory', type=Path)
    parser.add_argument('--output', type=Path, required=True, help='Readiness JSON file, or a fresh candidate directory with --include-deb/--include-dmg')
    parser.add_argument('--require-ready', action='store_true')
    args = parser.parse_args()
    root = args.source_root or Path(__file__).resolve().parents[1]
    dmg_inputs = [args.dmg_sha256, args.macos_portable_sha256, args.dmg_qualification,
                  args.dmg_qualification_sha256, args.macos_team_id]
    native_values = [args.native_attestation_bundle, args.native_attestation_sha256,
                     args.native_trusted_root, args.native_trusted_root_sha256,
                     args.native_gh, args.native_gh_sha256, args.native_gh_version,
                     args.native_workflow_commit, args.native_run_id, args.native_run_attempt,
                     args.native_verification_work]
    if args.native_job_readback and not all(native_values):
        parser.error('--native-job-readback requires the complete independently reviewed native attestation selection')
    if any(value is not None for value in native_values) and (not all(native_values) or not all(dmg_inputs)):
        parser.error('Native origin verification requires every reviewed verifier/root/bundle/source/run input and full DMG selection')
    dmg_trust = dict(bundle=args.native_attestation_bundle, bundle_sha256=args.native_attestation_sha256,
                     trusted_root=args.native_trusted_root, trusted_root_sha256=args.native_trusted_root_sha256,
                     cli=args.native_gh, cli_sha256=args.native_gh_sha256, cli_version=args.native_gh_version,
                     workflow_commit=args.native_workflow_commit, run_id=args.native_run_id,
                     run_attempt=args.native_run_attempt, work=args.native_verification_work) if all(native_values) else None
    if dmg_trust is not None and args.native_job_readback:
        dmg_trust['readback_job'] = True
    if (any(value is not None for value in dmg_inputs) and not all(dmg_inputs)):
        parser.error('DMG inputs require independent image/ZIP/qualification hashes, qualification path and publisher Team together')
    if (args.include_deb or args.deb) and (args.include_dmg is not None or any(value is not None for value in dmg_inputs)):
        parser.error('DMG handoff/inspection cannot be mixed with DEB construction or qualification mode')
    deb_inputs = [args.deb_sha256, args.portable, args.portable_sha256, args.maintainer, args.installation_receipt]
    if all(dmg_inputs) and not all([args.deb_sha256, args.portable_sha256, args.maintainer]):
        parser.error('Six-record inspection/handoff requires explicit DEB/portable hashes and maintainer')
    if args.include_dmg:
        if (not all(dmg_inputs) or not all([args.deb_sha256, args.portable_sha256, args.maintainer])
                or args.portable is not None or args.installation_receipt is not None):
            parser.error('--include-dmg requires --directory, all DMG inputs and DEB/portable hashes/maintainer; no portable path or private installation receipt')
        try:
            previous = read_previous_inventory(args.previous_inventory) if args.previous_inventory else None
            result = handoff_dmg(args.directory, args.include_dmg, args.dmg_qualification, args.output, args.commit, args.version,
                                 image_sha256=args.dmg_sha256, archive_sha256=args.macos_portable_sha256,
                                 qualification_sha256=args.dmg_qualification_sha256, team_id=args.macos_team_id,
                                 deb_sha256=args.deb_sha256, linux_portable_sha256=args.portable_sha256,
                                 maintainer=args.maintainer, previous=previous, source_root=root, dmg_trust=dmg_trust)
        except Exception:
            parser.exit(1, 'Final DMG candidate handoff failed; preserve partial output for local inspection.\n')
        print('Six-record candidate byte binding verified; native job authentication and production publication remain blocked.')
        return 2 if args.require_ready and not result['production_ready'] else 0
    elif args.deb:
        if not all(deb_inputs) or args.previous_inventory or args.include_deb:
            parser.error('--deb requires --deb-sha256, --portable, --portable-sha256, --maintainer and --installation-receipt; no previous inventory')
        receipt = regular_file(args.installation_receipt.absolute().parent, args.installation_receipt.name)
        with receipt.open('rb') as stream:
            evidence = stream.read(MAX_INSTALLATION_RECEIPT_BYTES + 1)
        result = assess_deb(root, args.portable, args.deb,
                            commit=args.commit, version=args.version, portable_sha256=args.portable_sha256,
                            package_sha256=args.deb_sha256, maintainer=args.maintainer, installation_receipt=evidence)
    elif args.include_deb:
        if (not all([args.deb_sha256, args.portable_sha256, args.maintainer])
                or args.portable is not None or args.installation_receipt is not None):
            parser.error('--include-deb requires --directory, explicit DEB/portable hashes and maintainer; no portable path or private receipt')
        previous = read_previous_inventory(args.previous_inventory) if args.previous_inventory else None
        result = handoff_deb(args.directory, args.include_deb, args.output, args.commit, args.version,
                             package_sha256=args.deb_sha256, portable_sha256=args.portable_sha256,
                             maintainer=args.maintainer, previous=previous, source_root=root)
        print('Five-record candidate content verified; native authority and production publication remain blocked.')
        return 2 if args.require_ready and not result['production_ready'] else 0
    else:
        if args.portable is not None or args.installation_receipt is not None:
            parser.error('Portable paths and private receipts require --deb qualification mode')
        selected = [args.deb_sha256, args.portable_sha256, args.maintainer]
        if any(value is not None for value in selected) and not all(selected):
            parser.error('Five-record inspection requires explicit DEB/portable hashes and maintainer together')
        if all(dmg_inputs):
            try:
                previous = read_previous_inventory(args.previous_inventory) if args.previous_inventory else None
                evidence = read_dmg_qualification(args.dmg_qualification)
                result = assess(args.directory, args.commit, args.version, previous, root,
                                deb_sha256=args.deb_sha256, linux_portable_sha256=args.portable_sha256, maintainer=args.maintainer,
                                dmg_sha256=args.dmg_sha256, macos_portable_sha256=args.macos_portable_sha256,
                                dmg_qualification=evidence, dmg_qualification_sha256=args.dmg_qualification_sha256,
                                macos_team_id=args.macos_team_id, dmg_trust=dmg_trust)
            except Exception:
                parser.exit(1, 'Final DMG candidate inspection failed; no native authority or publication established.\n')
        else:
            previous = read_previous_inventory(args.previous_inventory) if args.previous_inventory else None
            result = assess(args.directory, args.commit, args.version, previous, root, deb_sha256=args.deb_sha256,
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
