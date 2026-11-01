"""Fixed Store SDK-origin adapter; installed and public Store authority stay separate."""
import json
from pathlib import Path

from package_macos_dmg import bounded_file, private_inspection_directory
import production_readiness as readiness
import qualify_store_sdk as qualifier
import verify_macos_qualification as native

FIELDS = {'qualification', 'qualification_sha256', 'makeappx_sha256', 'makeappx_version', 'trust'}


def authenticate(qualification, **inputs):
    if any(key in inputs for key in ['_windows', '_linux', '_store', '_setup_host']):
        raise ValueError('Store SDK origin uses its fixed reviewed workflow and host policy.')
    return native.authenticate(qualification, **inputs, _store=True)


def bind(root, selection, checked):
    if (not isinstance(selection, dict) or set(selection) != FIELDS
            or not qualifier.tools._sha(selection['qualification_sha256'])
            or not qualifier.tools._sha(selection['makeappx_sha256'])
            or not qualifier.sdk_version(selection['makeappx_version'])
            or not isinstance(selection['trust'], dict)
            or set(selection['trust']) != readiness.NATIVE_TRUST_FIELDS | {'readback_job'}
            or selection['trust']['readback_job'] is not True):
        raise ValueError('Store SDK origin requires complete independent pins and exact job readback.')
    _, raw = qualifier.content.selected(selection['qualification'], selection['qualification_sha256'], qualifier.LIMIT)
    expected = qualifier.report_for(checked, makeappx_sha256=selection['makeappx_sha256'],
                                    makeappx_version=selection['makeappx_version'])
    if qualifier.encode(json.loads(raw, object_pairs_hook=readiness.unique_fields)) != qualifier.encode(expected):
        raise ValueError('Store SDK report differs from selected content, tool pins or authority holds.')
    trust = selection['trust']
    origin = authenticate(raw, qualification_sha256=selection['qualification_sha256'],
        commit=checked['commit'], version=checked['version'], source_root=root, **trust)
    if (origin['native_job_conclusion'] != 'service-reconciled-success'
            or origin['native_producer_authentication'] != 'verified-pinned-workflow-oidc-attestation'):
        raise ValueError('Store SDK qualification has not authenticated exact successful native completion.')
    work = private_inspection_directory(Path(trust['work']), root, fresh=False)
    records = [(Path(selection['qualification']), qualifier.LIMIT, selection['qualification_sha256']),
               (work / qualifier.REPORT, qualifier.LIMIT, selection['qualification_sha256'])]
    for key, filename, limit in [('bundle', 'bundle.jsonl', native.LIMITS['bundle']),
            ('trusted_root', 'trusted_root.jsonl', native.LIMITS['trusted-root']), ('cli', 'gh', native.LIMITS['gh'])]:
        records += [(Path(path), limit, trust[key + '_sha256']) for path in [trust[key], work / filename]]
    for path, limit, checksum in records:
        if bounded_file(path, limit)[0] != checksum:
            raise ValueError('Store SDK original or owned evidence changed during authentication.')
    return origin, records
