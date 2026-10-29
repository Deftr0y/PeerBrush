"""Compose exact final release bytes; native installed authority remains a separate gate.

Selection JSON supplies independently reviewed paths/hashes, never a readiness grant.
No signing, installation, publication, Store submission or account changes occur here.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import zipfile

from package import regular_file
from package_macos_dmg import bounded_file, private_inspection_directory
import package_msix as msix
import prepare_appinstaller as installer
import production_readiness as readiness
import qualify_signed_msix as signed

FORMAT = 'PeerBrushFinalReleaseV2'
LEGACY_FORMAT = 'PeerBrushFinalReleaseV1'
MANIFEST = 'final-release-inventory.json'
LIMIT = 512 * 1024 * 1024
_CONSTRUCTED = object()


def encode(value):
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode('utf-8')


def read(path, limit, expected=None):
    path = readiness.handoff_path(path)
    actual, size = bounded_file(path, limit)
    if not size or (expected is not None and actual != expected):
        raise ValueError('Selected release input differs from its independent digest.')
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) != size or hashlib.sha256(data).hexdigest() != actual:
        raise ValueError('Selected release input changed during its bounded read.')
    return data


def store_channel(observation=None):
    """Public summary of the existing read-only Store reconciler, never availability.

    Callers may pass its in-process result; serialized observations grant no authority.
    No Store client, credential, durable backend or write operation is configured here.
    """
    state = 'disabled'
    if observation is not None:
        from store_release_state import STATUSES
        allowed = set(STATUSES.values()) | {'not-started', 'create-outcome-uncertain',
            'another-submission-pending', 'version-already-present-or-superseded'}
        state = observation.get('service_state') if isinstance(observation, dict) else None
        if state not in allowed:
            raise ValueError('Unknown Store service state; public availability remains unverified.')
    return dict(submission_enabled=False, service_state=state, public_availability='unverified',
                ready=False, reason='durable-submission-backend-and-public-availability-not-configured')


def observe_store(store, journal):
    """Reuse existing durable reconciliation for an explicitly configured caller.

    The disabled composition/CLI never constructs a client or invokes this path.
    Certification/publishing status alone never establishes public availability.
    """
    from store_release_state import reconcile
    return store_channel(reconcile(store, journal))


def _windows_files(value):
    entry = value['channels']['windows-direct']
    if value['format'] == LEGACY_FORMAT:
        return dict(package=entry['dependencies'][0], runtime=entry['dependencies'][1],
                    descriptor=entry['file'], stable_descriptor=installer.STABLE_DESCRIPTOR)
    return dict(package=entry['file'], runtime=entry['dependencies'][0],
                descriptor=entry['discovery']['versioned'], stable_descriptor=entry['discovery']['stable'])


def windows_files(value):
    """Validated file roles only; these grant no installation or native authority."""
    validate_manifest(value)
    return _windows_files(value)


def validate_manifest(value):
    fields = {'schema', 'format', 'version', 'commit', 'base_inventory', 'channels', 'files'}
    if (not isinstance(value, dict) or set(value) != fields or type(value['schema']) is not int
            or (value['schema'], value['format']) not in [(1, LEGACY_FORMAT), (2, FORMAT)]
            or type(value['commit']) is not str or not re.fullmatch('[0-9a-f]{40}', value['commit'])
            or type(value['version']) is not str
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', value['version'])):
        raise ValueError('Final release requires a strict stable exact-source manifest.')
    from prepare_site_release import validate_inventory
    base = validate_inventory(value['base_inventory'], value['commit'], value['version'])
    if len(base) != 6:
        raise ValueError('Final composition requires all portable platforms, source, DEB and DMG.')
    files = value['files']
    if (not isinstance(files, list) or not 9 <= len(files) <= 32
            or any(not isinstance(r, dict) or set(r) != {'file', 'bytes', 'sha256'}
                   or type(r['file']) is not str or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,199}', r['file'])
                   or type(r['bytes']) is not int or not 0 < r['bytes'] <= LIMIT
                   or type(r['sha256']) is not str or not re.fullmatch('[0-9a-f]{64}', r['sha256']) for r in files)
            or [r['file'] for r in files] != sorted({r['file'] for r in files})
            or len({r['file'].casefold() for r in files}) != len(files)
            or any(r['file'] in {MANIFEST, 'SHA256SUMS'} for r in files)):
        raise ValueError('Final release has invalid, duplicate or unbounded public files.')
    records = {r['file']: r for r in files}
    if any(records.get(name) != {key: record[key] for key in ['file', 'bytes', 'sha256']} for name, record in base.items()):
        raise ValueError('Final files differ from the inspected base inventory.')
    channels = value['channels']
    if not isinstance(channels, dict) or set(channels) != {'windows-direct', 'macos-direct', 'linux-direct', 'microsoft-store'}:
        raise ValueError('Each distribution needs its explicit reviewed channel adapter.')
    legacy = value['format'] == LEGACY_FORMAT
    expected_platform = {'windows-direct': ('Windows', '.appinstaller' if legacy else '.msix'), 'macos-direct': ('macOS', '.dmg'),
                         'linux-direct': ('Linux', '.deb')}
    for channel, (platform, suffix) in expected_platform.items():
        entry = channels[channel]
        fields = {'platform', 'file', 'dependencies', 'installed_acceptance', 'ready'}
        if channel == 'windows-direct' and not legacy:
            fields |= {'discovery', 'update_route'}
        if (not isinstance(entry, dict) or set(entry) != fields
                or entry['platform'] != platform or type(entry['file']) is not str
                or entry['file'] not in records or not entry['file'].endswith(suffix)
                or entry['installed_acceptance'] != 'unverified' or entry['ready'] is not False
                or not isinstance(entry['dependencies'], list)
                or len(entry['dependencies']) != ((2 if legacy else 1) if channel == 'windows-direct' else 0)
                or any(type(name) is not str or name not in records for name in entry['dependencies'])
                or len(set(entry['dependencies'])) != len(entry['dependencies'])):
            raise ValueError('Final channel binding or explicit unverified installed state differs.')
    if channels['microsoft-store'] != store_channel():
        raise ValueError('Store submission remains disabled; a manifest cannot assert public availability.')
    windows = channels['windows-direct']
    if not legacy:
        discovery = windows['discovery']
        if (windows['update_route'] != 'verified-local' or not isinstance(discovery, dict)
                or set(discovery) != {'versioned', 'stable'}
                or any(type(name) is not str for name in discovery.values())
                or discovery['stable'] != installer.STABLE_DESCRIPTOR):
            raise ValueError('Windows discovery is metadata only for the verified-local route.')
    selected = _windows_files(value)
    target = msix.package_version(value['version'])
    if (selected['descriptor'] != f'PeerBrush-{target}-Windows-x64.appinstaller'
            or not re.fullmatch(f'PeerBrush-{re.escape(target)}-Windows-x64-signed-r[1-9][0-9]*\\.msix', selected['package'])
            or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,126}\.(?:appx|msix)', selected['runtime'])
            or len(set(selected.values())) != 4):
        raise ValueError('Final Windows channel must bind its exact signed package and framework.')
    stable = records.get(installer.STABLE_DESCRIPTOR)
    descriptor = records.get(selected['descriptor'])
    if stable is None or descriptor is None or any(stable[key] != descriptor[key] for key in ['bytes', 'sha256']):
        raise ValueError('Stable discovery must contain the exact immutable Windows descriptor bytes.')
    dmg = channels['macos-direct']['file']
    deb = channels['linux-direct']['file']
    if dmg not in base or deb not in base:
        raise ValueError('Selected Mac/Linux channels differ from the inspected base assets.')
    expected = set(base) | {*selected.values(), dmg + '.qualification.json',
                            dmg + '.sha256', deb + '.json', deb + '.sha256'}
    if set(records) != expected:
        raise ValueError('Final release permits only exact selected assets and reviewed public sidecars.')
    return records


def checksum_bytes(manifest):
    records = dict(validate_manifest(manifest))
    raw = encode(manifest)
    records[MANIFEST] = dict(file=MANIFEST, bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
    return ''.join(records[name]['sha256'] + '  ' + name + '\n' for name in sorted(records)).encode('ascii')


def immutable(previous, current):
    validate_manifest(previous)
    validate_manifest(current)
    readiness.immutable(dict(schema=1, version=previous['version'], commit=previous['commit'], assets=previous['files']),
                        dict(version=current['version'], commit=current['commit'], assets=current['files']))
    if previous['version'] == current['version'] and previous != current:
        raise ValueError('Published final manifests and channel bindings are immutable for this version.')


class Candidate:
    """An in-process result of real consumers; loading a report cannot construct it."""
    def __init__(self, root, directory, manifest, assessment, token):
        if token is not _CONSTRUCTED:
            raise ValueError('Only actual final composition may construct a candidate.')
        self.root, self.directory = root, directory
        self.manifest, self.assessment = manifest, assessment
        self.blockers = tuple(assessment['publication_phases']['prepublication_blockers'])

    def verify(self):
        signed._source(self.root, self.manifest['commit'])
        private_inspection_directory(self.directory, self.root, fresh=False)
        records = validate_manifest(self.manifest)
        for name, record in records.items():
            if bounded_file(regular_file(self.directory, name), LIMIT) != (record['sha256'], record['bytes']):
                raise ValueError('Final release files changed after composition.')
        if (read(self.directory / MANIFEST, 128 * 1024) != encode(self.manifest)
                or read(self.directory / 'SHA256SUMS', 8192) != checksum_bytes(self.manifest)
                or {p.name for p in self.directory.iterdir()} != set(records) | {MANIFEST, 'SHA256SUMS'}):
            raise ValueError('Final release inventory, checksums or exclusive output changed.')


def windows_binding(root, base, directory, selection):
    required = {'unsigned', 'unsigned_sha256', 'package', 'package_sha256', 'runtime', 'runtime_sha256',
                'identity', 'identity_sha256', 'package_family', 'descriptor', 'descriptor_sha256',
                'descriptor_uri', 'package_uri', 'runtime_uri'}
    if not isinstance(selection, dict) or set(selection) not in [required, required | {'native'}]:
        raise ValueError('Select all exact Windows direct files, identities, hashes and immutable locations.')
    package, unsigned, runtime, descriptor, identity_path = [readiness.handoff_path(selection[name])
        for name in ['package', 'unsigned', 'runtime', 'descriptor', 'identity']]
    for key, path, limit in [('unsigned', unsigned, LIMIT), ('package', package, LIMIT), ('runtime', runtime, LIMIT),
                             ('descriptor', descriptor, 16 * 1024), ('identity', identity_path, 16 * 1024)]:
        if not signed._sha(selection[key + '_sha256']) or bounded_file(path, limit)[0] != selection[key + '_sha256']:
            raise ValueError('Windows direct input differs from its independent final digest.')
    identity = json.loads(read(identity_path, 16 * 1024), object_pairs_hook=readiness.unique_fields)
    msix.identity_values(identity)
    msix.validate_package_family(identity, selection['package_family'])
    version = msix.package_version(base['version'])
    if (unsigned.name != f'PeerBrush-{version}-Windows-x64.msix'
            or not re.fullmatch(re.escape(unsigned.stem) + r'-signed-r[1-9][0-9]*\.msix', package.name)
            or descriptor.name != f'PeerBrush-{version}-Windows-x64.appinstaller'):
        raise ValueError('Select the canonical unsigned, signed revision and versioned App Installer files.')
    portable = next(record for record in base['assets'] if record.get('platform') == 'Windows')
    expected, release = msix.expected_payload(root, directory / portable['file'], identity, require_provenance=True)
    if (release['version'], release['commit']) != (base['version'], base['commit']):
        raise ValueError('Windows direct content has a different embedded source/version.')
    with zipfile.ZipFile(unsigned) as archive:
        entry = archive.getinfo('peerbrush.exe')
        if not 0 < entry.file_size <= msix.PACKAGE_LIMIT:
            raise ValueError('Windows direct executable exceeds the existing byte budget.')
        with archive.open(entry) as stream:
            executable = stream.read(msix.PACKAGE_LIMIT + 1)
        if len(executable) != entry.file_size:
            raise ValueError('Windows direct executable differs from its declared size.')
    original_executable = expected['peerbrush.exe']
    expected = msix.signed_executable_payload(expected, executable)
    msix.verify_payload(unsigned, expected)
    msix.verify_payload(package, expected, signed=True)
    signed._unsigned_binding(unsigned, package)
    dependency = installer.framework_identity(package, runtime)
    for key, name in [('descriptor_uri', descriptor.name), ('package_uri', package.name), ('runtime_uri', runtime.name)]:
        installer.canonical_uri(selection[key], name)
    expected_descriptor = installer.descriptor_bytes_for_selection(identity, dependency, version,
        selection['descriptor_uri'], selection['package_uri'], selection['runtime_uri'])
    if read(descriptor, 16 * 1024) != expected_descriptor:
        raise ValueError('Final App Installer differs from the exact shared descriptor serializer.')
    channel = dict(platform='Windows', file=package.name, dependencies=[runtime.name],
        discovery=dict(versioned=descriptor.name, stable=installer.STABLE_DESCRIPTOR),
        update_route='verified-local', installed_acceptance='unverified', ready=False)
    originals = [
        (path, limit, selection[key + '_sha256']) for key, path, limit in [
            ('unsigned', unsigned, LIMIT), ('package', package, LIMIT), ('runtime', runtime, LIMIT),
            ('descriptor', descriptor, 16 * 1024), ('identity', identity_path, 16 * 1024)]]
    origin = None
    if 'native' in selection:
        import verify_windows_qualification as native
        if not isinstance(selection['native'], dict) or set(selection['native']) != native.FIELDS:
            raise ValueError('Complete strict Windows native qualification selection required.')
        qualification = read(selection['native']['qualification'], native.LIMITS['qualification'], selection['native']['qualification_sha256'])
        origin = native.bind(root, selection['native'], qualification, commit=base['commit'], version=base['version'],
            identity=identity, family=selection['package_family'], portable_sha256=portable['sha256'],
            unsigned_sha256=selection['unsigned_sha256'], package=package, package_sha256=selection['package_sha256'],
            original_executable=original_executable, executable=executable, identity_sha256=selection['identity_sha256'],
            runtime=runtime, runtime_sha256=selection['runtime_sha256'], dependency=dependency,
            descriptor=descriptor, descriptor_sha256=selection['descriptor_sha256'],
            descriptor_uri=selection['descriptor_uri'], package_uri=selection['package_uri'], runtime_uri=selection['runtime_uri'])
        originals += native.retained_inputs(selection['native'])
    return [package, runtime, descriptor], channel, originals, origin


def compose(root, directory, output, *, commit, version, macos, windows, maintainer,
            linux_receipt, linux_receipt_sha256, previous=None, linux_native=None):
    root, directory = readiness.handoff_path(root), readiness.handoff_path(directory)
    signed._source(root, commit)
    inventory_bytes = readiness.read_inventory(directory)
    inventory = json.loads(inventory_bytes, object_pairs_hook=readiness.unique_fields)
    if len(inventory.get('assets', [])) != 6:
        raise ValueError('Final composition needs the complete six-record exact-source base.')
    by_suffix = lambda suffix: next(r for r in inventory['assets'] if r['file'].endswith(suffix))
    dmg, deb = by_suffix('.dmg'), by_suffix('.deb')
    portable = next(r for r in inventory['assets'] if r.get('platform') == 'Linux' and r['file'].endswith('.zip'))
    mac_archive = next(r for r in inventory['assets'] if r.get('platform') == 'macOS' and r['file'].endswith('.zip'))
    if not isinstance(macos, dict) or set(macos) != {'qualification', 'qualification_sha256', 'team_id', 'trust'}:
        raise ValueError('Select the exact native Mac qualification and reviewed authentication inputs.')
    qualification = read(macos['qualification'], readiness.MAX_DMG_QUALIFICATION_BYTES, macos['qualification_sha256'])
    if not isinstance(macos['trust'], dict) or macos['trust'].get('readback_job') is not True:
        raise ValueError('Native Mac origin requires actual cryptographic verification and exact-job service readback.')
    report = readiness.assess(directory, commit, version, source_root=root,
        deb_sha256=deb['sha256'], linux_portable_sha256=portable['sha256'], maintainer=maintainer,
        dmg_sha256=dmg['sha256'], macos_portable_sha256=mac_archive['sha256'],
        dmg_qualification=qualification, dmg_qualification_sha256=macos['qualification_sha256'],
        macos_team_id=macos['team_id'], dmg_trust=macos['trust'])
    native = report['installed_candidates']['macOS']
    if (native['native_producer_authentication'] != 'verified-pinned-workflow-oidc-attestation'
            or native['native_job_conclusion'] != 'service-reconciled-success'):
        raise ValueError('Final Mac bytes lack authenticated successful native qualification.')
    receipt = read(linux_receipt, readiness.MAX_INSTALLATION_RECEIPT_BYTES, linux_receipt_sha256)
    linux = readiness.assess_deb(root, directory / portable['file'], directory / deb['file'], commit=commit,
        version=version, portable_sha256=portable['sha256'], package_sha256=deb['sha256'], maintainer=maintainer,
        installation_receipt=receipt)
    linux_origin, linux_originals = None, []
    if linux_native is not None:
        import verify_linux_qualification as linux_verifier
        linux_origin, linux_originals = linux_verifier.bind(root, linux_native, installation=receipt,
            assessment=linux, commit=commit, version=version, portable_sha256=portable['sha256'], maintainer=maintainer)
    direct, windows_channel, windows_originals, windows_origin = windows_binding(root, inventory, directory, windows)
    outputs = {record['file']: directory / record['file'] for record in inventory['assets']}
    outputs.update({path.name: path for path in direct})
    if len(outputs) != 9:
        raise ValueError('Final platform filenames collide; no output was prepared.')
    # These are strict public candidate metadata, not private acceptance evidence.
    deb_report = dict(schema=1, file=deb['file'], version=version, commit=commit, architecture='amd64',
        binary_source_commit=commit, sha256=deb['sha256'], source_sha256=deb['source_sha256'],
        target='Ubuntu 24.04 amd64', update_owner='apt-dpkg', update_repository='not-configured',
        signing='not-performed', installation_test='not-performed', default_app='user-choice-only', release_ready=False)
    sidecars = {dmg['file'] + '.qualification.json': qualification, deb['file'] + '.json': encode(deb_report),
                installer.STABLE_DESCRIPTOR: read(windows['descriptor'], 16 * 1024, windows['descriptor_sha256'])}
    for record in [dmg, deb]:
        sidecars[record['file'] + '.sha256'] = (record['sha256'] + '  ' + record['file'] + '\n').encode('ascii')
    if set(sidecars) & set(outputs):
        raise ValueError('Final public sidecar names collide.')
    records = [dict(file=name, bytes=size, sha256=checksum) for name, path in outputs.items()
               for checksum, size in [bounded_file(path, LIMIT)]]
    expected_hashes = {record['file']: record['sha256'] for record in inventory['assets']}
    expected_hashes.update({Path(windows[key]).name: windows[key + '_sha256'] for key in ['package', 'runtime', 'descriptor']})
    if any(record['sha256'] != expected_hashes[record['file']] for record in records):
        raise ValueError('Final input changed between validation and composition.')
    records += [dict(file=name, bytes=len(data), sha256=hashlib.sha256(data).hexdigest()) for name, data in sidecars.items()]
    manifest = dict(schema=2, format=FORMAT, version=version, commit=commit, base_inventory=inventory,
        files=sorted(records, key=lambda r: r['file']), channels={
            'windows-direct': windows_channel,
            'macos-direct': dict(platform='macOS', file=dmg['file'], dependencies=[], installed_acceptance='unverified', ready=False),
            'linux-direct': dict(platform='Linux', file=deb['file'], dependencies=[], installed_acceptance='unverified', ready=False),
            'microsoft-store': store_channel()})
    validate_manifest(manifest)
    if previous is not None:
        immutable(previous, manifest)
    output = private_inspection_directory(output, root, fresh=True)
    if any(output.is_relative_to(path.parent) for path in [directory / 'release-inventory.json', *outputs.values()]):
        raise ValueError('Final output must be separate from every selected input directory.')
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    for record in records:
        path = output / record['file']
        if record['file'] in outputs:
            if bounded_file(outputs[record['file']], LIMIT, path) != (record['sha256'], record['bytes']):
                raise ValueError('Selected final bytes changed during exclusive composition.')
        else:
            with path.open('xb') as stream:
                stream.write(sidecars[record['file']])
        path.chmod(0o400)
    for name, data in [(MANIFEST, encode(manifest)), ('SHA256SUMS', checksum_bytes(manifest))]:
        with (output / name).open('xb') as stream:
            stream.write(data)
        (output / name).chmod(0o400)
    for record in records:
        if record['file'] in outputs and bounded_file(outputs[record['file']], LIMIT) != (record['sha256'], record['bytes']):
            raise ValueError('Original final input changed after composition; preserve partial output.')
    if readiness.read_inventory(directory) != inventory_bytes:
        raise ValueError('Original base inventory changed during composition.')
    for path, limit, expected in [*windows_originals, *linux_originals]:
        if bounded_file(path, limit)[0] != expected:
            raise ValueError('Original or owned native binding input changed after composition.')
    if (read(macos['qualification'], readiness.MAX_DMG_QUALIFICATION_BYTES, macos['qualification_sha256']) != qualification
            or read(linux_receipt, readiness.MAX_INSTALLATION_RECEIPT_BYTES, linux_receipt_sha256) != receipt):
        raise ValueError('Original native evidence changed after composition.')
    from verify_macos_qualification import LIMITS
    trust = macos['trust']
    for key, owned, limit in [('bundle', 'bundle.jsonl', LIMITS['bundle']),
                              ('trusted_root', 'trusted_root.jsonl', LIMITS['trusted-root']), ('cli', 'gh', LIMITS['gh'])]:
        for path in [Path(trust[key]), Path(trust['work']) / owned]:
            if bounded_file(path, limit)[0] != trust[key + '_sha256']:
                raise ValueError('Native Mac original or owned authentication input changed after composition.')
    if bounded_file(Path(trust['work']) / 'dmg-qualification.json', readiness.MAX_DMG_QUALIFICATION_BYTES)[0] != macos['qualification_sha256']:
        raise ValueError('Owned native Mac qualification changed after composition.')
    if windows_origin is None:
        report['blockers'] += ['windows-native-final-producer-not-authenticated',
            'windows-executable-signing-and-installed-acceptance-not-established']
    else:
        report['blockers'].remove('windows-installed-distribution-and-signing-not-verified')
        report['blockers'].append('windows-installed-acceptance-not-established')
    report['blockers'] += [
        'windows-runtime-redistribution-approval-not-authenticated',
        'linux-installed-evidence-producer-not-authenticated',
        'installed-upgrade-interruption-and-visible-native-acceptance-not-established']
    if linux_origin is not None:
        report['blockers'].remove('linux-installed-evidence-producer-not-authenticated')
        linux['installation_receipt_provenance'] = 'verified-pinned-workflow-oidc-attestation-and-successful-job'
        report['installed_candidates']['Linux']['producer_provenance'] = linux['installation_receipt_provenance']
        report['linux_bound_installation'] = linux
        report['linux_native_origin'] = linux_origin
    report['publication_phases'] = readiness._publication_phases(report['blockers'])
    report.update(format='PeerBrushFinalCompositionAssessmentV1', production_ready=False,
        final_files_bound=True, windows_native_signature=('verified-by-authenticated-native-qualification' if windows_origin else 'not-authenticated-by-aggregate'),
        linux_installation_binding=linux['installation_receipt_binding'],
        linux_installation_provenance=linux['installation_receipt_provenance'], store=store_channel())
    if windows_origin is not None:
        report['windows_native_origin'] = windows_origin
    candidate = Candidate(root, output, manifest, report, _CONSTRUCTED)
    candidate.verify()
    return candidate


def from_selection(root, directory, output, selection, selection_sha256):
    selected = json.loads(read(selection, 64 * 1024, selection_sha256), object_pairs_hook=readiness.unique_fields)
    required = {'commit', 'version', 'macos', 'windows', 'maintainer', 'linux_receipt', 'linux_receipt_sha256'}
    if (not isinstance(selected, dict) or set(selected) not in [required, required | {'linux_native'}]
            or ('linux_native' in selected and not isinstance(selected['linux_native'], dict))):
        raise ValueError('Strict reviewed final selection required; reports cannot grant authority.')
    return compose(root, directory, output, **selected)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'directory', 'output', 'selection']:
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--selection-sha256', required=True)
    args = parser.parse_args(argv)
    try:
        candidate = from_selection(args.source_root, args.directory, args.output, args.selection, args.selection_sha256)
    except Exception:
        parser.exit(1, 'Final composition failed; preserve private partial output. No publication or submission occurred.\n')
    print(json.dumps(candidate.assessment, sort_keys=True))
    return 2  # Native installed producers/publication authority are still absent.


if __name__ == '__main__':
    raise SystemExit(main())
