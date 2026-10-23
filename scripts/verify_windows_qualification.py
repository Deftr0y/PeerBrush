"""Bind actual Windows bytes to an authenticated native qualification, never ready JSON."""
import hashlib
import json
from pathlib import Path

from package_macos_dmg import bounded_file
from production_readiness import unique_fields, NATIVE_TRUST_FIELDS
import qualify_signed_msix as signed
import verify_macos_qualification as native

LIMITS = dict(native.LIMITS, qualification=signed.OUTPUT_LIMIT)
FIELDS = {'qualification', 'qualification_sha256', 'certificate_sha256', 'timestamp_certificate_sha256',
          'executable_timestamp_certificate_sha256', 'runtime_certificate_sha256', 'runtime_timestamp_certificate_sha256',
          'runtime_license_sha256', 'signtool_sha256', 'powershell_sha256', 'trust'}


def authenticate(qualification, **inputs):
    if '_windows' in inputs:
        raise ValueError('Windows native origin uses its fixed reviewed workflow policy.')
    return native.authenticate(qualification, **inputs, _windows=True)


def bind(root, selection, qualification, *, commit, version, identity, family, portable_sha256,
         unsigned_sha256, package, package_sha256, original_executable, executable,
         identity_sha256, runtime, runtime_sha256, dependency, descriptor, descriptor_sha256,
         descriptor_uri, package_uri, runtime_uri):
    if (not isinstance(selection, dict) or set(selection) != FIELDS
            or any(not signed._sha(selection[name]) for name in FIELDS - {'qualification', 'trust'})
            or not isinstance(selection['trust'], dict)
            or set(selection['trust']) != NATIVE_TRUST_FIELDS | {'readback_job'}
            or selection['trust']['readback_job'] is not True):
        raise ValueError('Windows native handoff requires complete independent public pins and actual job readback.')
    if (not isinstance(qualification, bytes) or not 0 < len(qualification) <= signed.OUTPUT_LIMIT
            or hashlib.sha256(qualification).hexdigest() != selection['qualification_sha256']):
        raise ValueError('Windows qualification differs from its independently selected report digest.')
    child = dict(schema=1, format='PeerBrushSignedMsixQualificationV1', commit=commit, version=version,
        package_version=signed.msix.package_version(version), package=dict(file=package.name, sha256=package_sha256),
        unsigned_sha256=unsigned_sha256, portable_sha256=portable_sha256, identity=identity, package_family_name=family,
        certificate_sha256=selection['certificate_sha256'], timestamp_certificate_sha256=selection['timestamp_certificate_sha256'],
        verifier=dict(signtool_sha256=selection['signtool_sha256'], powershell_sha256=selection['powershell_sha256'],
            powershell_version=signed.POWERSHELL_VERSION), content_verified=True,
        native_signature='verified-embedded-default-authentication-policy', timestamp='required-by-native-verifier',
        native_producer_authentication='unverified-requires-trusted-native-job',
        executable_signing='verified-embedded-default-authentication-policy',
        executable=dict(unsigned_sha256=hashlib.sha256(original_executable).hexdigest(), sha256=hashlib.sha256(executable).hexdigest(),
            certificate_sha256=selection['certificate_sha256'], timestamp_certificate_sha256=selection['executable_timestamp_certificate_sha256']),
        installed_acceptance='not-performed', appinstaller='not-configured', public_availability='unverified', ready=False, production_ready=False)
    expected = dict(schema=1, format='PeerBrushWindowsInstallerQualificationV1', commit=commit, version=version,
        signed_package=child, identity_sha256=identity_sha256,
        runtime=dict(file=runtime.name, sha256=runtime_sha256, identity=dependency,
            certificate_sha256=selection['runtime_certificate_sha256'], timestamp_certificate_sha256=selection['runtime_timestamp_certificate_sha256'],
            license_sha256=selection['runtime_license_sha256']),
        descriptor=dict(file=descriptor.name, sha256=descriptor_sha256, uri=descriptor_uri, package_uri=package_uri, runtime_uri=runtime_uri),
        native_producer_authentication='unverified-requires-trusted-native-job', installed_acceptance='not-performed',
        runtime_redistribution_approval='not-authenticated', public_availability='unverified', ready=False, production_ready=False)
    report = json.loads(qualification, object_pairs_hook=unique_fields)
    # Canonical byte equality also rejects Python's bool/int equality aliasing.
    encode = lambda value: json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode('ascii')
    if encode(report) != encode(expected):
        raise ValueError('Windows native report differs from actual selected bytes, source, leaves, tools or safety gates.')
    origin = authenticate(qualification, qualification_sha256=selection['qualification_sha256'],
        commit=commit, version=version, source_root=root, **selection['trust'])
    if origin['native_job_conclusion'] != 'service-reconciled-success':
        raise ValueError('Windows native qualification job has not completed successfully.')
    return origin


def retained_inputs(selection):
    trust = selection['trust']
    records = [(Path(selection['qualification']), LIMITS['qualification'], selection['qualification_sha256']),
               (Path(trust['work']) / 'windows-installer-qualification.json', LIMITS['qualification'], selection['qualification_sha256'])]
    for key, owned, limit in [('bundle', 'bundle.jsonl', LIMITS['bundle']),
                              ('trusted_root', 'trusted_root.jsonl', LIMITS['trusted-root']), ('cli', 'gh', LIMITS['gh'])]:
        records += [(Path(path), limit, trust[key + '_sha256']) for path in [trust[key], Path(trust['work']) / owned]]
    for path, limit, checksum in records:
        if bounded_file(path, limit)[0] != checksum:
            raise ValueError('Windows original or owned native evidence changed.')
    return records
