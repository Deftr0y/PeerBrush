"""Fixed Linux adapter for actual receipt binding and the shared native origin verifier."""
import hashlib
import json
from pathlib import Path

from package_macos_dmg import bounded_file, private_inspection_directory
import production_readiness as readiness
import qualify_linux_deb as qualifier
import verify_macos_qualification as native

FIELDS = {'qualification', 'qualification_sha256', 'upgrade', 'upgrade_sha256', 'trust'}


def authenticate(qualification, **inputs):
    if '_windows' in inputs or '_linux' in inputs:
        raise ValueError('Linux native origin uses its fixed reviewed workflow policy.')
    return native.authenticate(qualification, **inputs, _linux=True)


def bind(root, selection, *, installation, assessment, commit, version, portable_sha256, maintainer):
    if (not isinstance(selection, dict) or set(selection) != FIELDS
            or not qualifier.sha(selection['qualification_sha256']) or not qualifier.sha(selection['upgrade_sha256'])
            or not isinstance(selection['trust'], dict)
            or set(selection['trust']) != readiness.NATIVE_TRUST_FIELDS | {'readback_job'}
            or selection['trust']['readback_job'] is not True):
        raise ValueError('Linux native handoff requires complete independent public pins and actual job readback.')
    qualification = qualifier.read(selection['qualification'], selection['qualification_sha256'], qualifier.LIMIT)
    upgrade = qualifier.read(selection['upgrade'], selection['upgrade_sha256'], readiness.MAX_INSTALLATION_RECEIPT_BYTES)
    expected = qualifier.report(assessment, installation, upgrade, commit=commit, version=version,
        portable_sha256=portable_sha256, maintainer=maintainer)
    actual = json.loads(qualification, object_pairs_hook=readiness.unique_fields)
    if qualifier.encode(actual) != qualifier.encode(expected):
        raise ValueError('Linux native report differs from actual selected receipt/package/source bytes or safety gates.')
    origin = authenticate(qualification, qualification_sha256=selection['qualification_sha256'],
        commit=commit, version=version, source_root=root, **selection['trust'])
    if (origin['native_job_conclusion'] != 'service-reconciled-success'
            or origin['native_producer_authentication'] != 'verified-pinned-workflow-oidc-attestation'):
        raise ValueError('Linux native qualification job has not authenticated successful completion.')
    trust = selection['trust']
    work = private_inspection_directory(Path(trust['work']), root, fresh=False)
    records = [(Path(selection['qualification']), qualifier.LIMIT, selection['qualification_sha256']),
               (Path(trust['work']) / qualifier.REPORT, qualifier.LIMIT, selection['qualification_sha256']),
               (Path(selection['upgrade']), readiness.MAX_INSTALLATION_RECEIPT_BYTES, selection['upgrade_sha256'])]
    for name, data in [('installation.json', installation), ('portable-to-deb.json', upgrade)]:
        private_inspection_directory(work, root, fresh=False)
        with (work / name).open('xb') as stream:
            stream.write(data)
        (work / name).chmod(0o400)
        records.append((work / name, readiness.MAX_INSTALLATION_RECEIPT_BYTES, hashlib.sha256(data).hexdigest()))
    for key, name, limit in [('bundle', 'bundle.jsonl', native.LIMITS['bundle']),
                            ('trusted_root', 'trusted_root.jsonl', native.LIMITS['trusted-root']), ('cli', 'gh', native.LIMITS['gh'])]:
        records += [(Path(path), limit, trust[key + '_sha256']) for path in [trust[key], Path(trust['work']) / name]]
    for path, limit, checksum in records:
        if bounded_file(path, limit)[0] != checksum:
            raise ValueError('Linux original or owned native evidence changed.')
    return origin, records
