"""Acquire exact official Store SDK inputs; transport metadata grants no SDK authority."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
from urllib.parse import urlsplit

import acquire_windows_qualification_inputs as windows
import acquire_macos_qualification_inputs as transport
import assemble_final_inputs as upstream
from package_macos_dmg import bounded_file, private_inspection_directory
import qualify_store_sdk as qualifier

WORKFLOW = transport.REPOSITORY + '/.github/workflows/store-qualification.yml@refs/heads/main'
TRANSPORT_FIELDS = {'run_id', 'portable_artifact_id', 'portable_artifact_sha256', 'source_sha256',
                    'store_artifact_id', 'store_artifact_sha256', 'checksum_sha256'}


def store_artifact(root, output, *, commit, run_id, artifact_id, artifact_sha256, files, token):
    record = dict(run_id=run_id, artifact_id=artifact_id, artifact_sha256=artifact_sha256, files=files)
    if (not transport.hexadecimal(commit, 40) or not transport.identifier(run_id)
            or not transport.identifier(artifact_id) or not transport.hexadecimal(artifact_sha256, 64)
            or not isinstance(files, dict) or len(files) != 3
            or any(not upstream.inputs.safe_name(name) or not transport.hexadecimal(value, 64) for name, value in files.items())):
        raise ValueError('Exact Store artifact and inner byte selections required.')
    output = private_inspection_directory(output, root, fresh=True)
    run_route, artifact_route = transport.API + 'runs/' + run_id, transport.API + 'artifacts/' + artifact_id
    def read(route):
        return json.loads(transport.request('api.github.com', route, 1024 * 1024, token=token),
                          object_pairs_hook=qualifier.readiness.unique_fields)
    run, artifact = read(run_route), read(artifact_route)
    size = upstream.metadata(run, artifact, 'store_package', record, commit)
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    location = transport.request('api.github.com', artifact_route + '/zip', 8192, token=token, redirect=True)
    parsed = urlsplit(location)
    archive = output / 'selected-artifact.zip'
    transport.request(parsed.hostname, parsed.path + ('?' + parsed.query if parsed.query else ''), size, destination=archive)
    if bounded_file(archive, transport.MAX_ARTIFACT) != (artifact_sha256, size):
        raise ValueError('Store outer artifact differs from independently selected bytes.')
    upstream.unpack(archive, output, files, transport.MAX_ARTIFACT, 'store_package')
    if read(run_route) != run or read(artifact_route) != artifact:
        raise ValueError('Store predecessor metadata changed during acquisition.')
    if bounded_file(archive, transport.MAX_ARTIFACT) != (artifact_sha256, size):
        raise ValueError('Store outer original changed during acquisition.')
    for name, checksum in files.items():
        if bounded_file(output / name, transport.MAX_FILE)[0] != checksum:
            raise ValueError('Store selected inner bytes changed during acquisition.')
    return output, (run, artifact)


def acquire(root, output, selection, *, token, environment):
    if not isinstance(selection, dict) or set(selection) != {'commit', 'transport', 'qualification'}:
        raise ValueError('Strict Store SDK native input selection required.')
    commit = selection['commit']
    required = dict(GITHUB_REPOSITORY=transport.REPOSITORY, GITHUB_REF='refs/heads/main',
        GITHUB_REPOSITORY_ID=str(transport.REPOSITORY_ID), GITHUB_REPOSITORY_OWNER_ID=str(transport.OWNER_ID),
        GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=WORKFLOW,
        GITHUB_SHA=commit, GITHUB_WORKFLOW_SHA=commit, GITHUB_RUN_ATTEMPT='1',
        RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Windows', RUNNER_ARCH='X64',
        PEERBRUSH_STORE_SDK_QUALIFICATION_ENABLED='true')
    if any(environment.get(key) != value for key, value in required.items()):
        raise ValueError('Store SDK qualification requires the approved exact hosted main invocation.')
    version = windows.stable_source(root, commit)
    q, t = selection['qualification'], selection['transport']
    if (not isinstance(q, dict) or set(q) != qualifier.INPUT_FIELDS or q['commit'] != commit or q['version'] != version
            or not isinstance(t, dict) or set(t) != TRANSPORT_FIELDS
            or any(not transport.hexadecimal(t[key], 64) for key in TRANSPORT_FIELDS - {'run_id', 'portable_artifact_id', 'store_artifact_id'})
            or any(not transport.identifier(t[key]) for key in ['run_id', 'portable_artifact_id', 'store_artifact_id'])
            or t['portable_artifact_id'] == t['store_artifact_id']
            or not qualifier.sdk_version(q['makeappx_version']) or not qualifier.tools._sha(q['makeappx_sha256'])):
        raise ValueError('Store SDK inputs require complete fixed transport, source and native tool selections.')
    package = f'PeerBrush-{qualifier.content.store.msix.package_version(version)}-Windows-x64.msix'
    source_name = f'PeerBrush-{version}-source.zip'
    if (q['portable'] != f'PeerBrush-{version}-Windows-x64.zip' or q['package'] != package
            or q['report'] != package + '.json'
            or any(not transport.hexadecimal(q[key], 64) for key in ['portable_sha256', 'package_sha256', 'report_sha256'])):
        raise ValueError('Store SDK artifact names and input digests differ from the canonical selected version.')
    qualifier.content.store.release_version(version)
    qualifier.content.store.version(q['previous_version'])
    qualifier.content.store.msix.package_version(version, previous=q['previous_version'])
    sums = (q['package_sha256'] + '  ' + package + '\n').encode('ascii')
    if t['checksum_sha256'] not in {hashlib.sha256(data).hexdigest() for data in [sums, sums.replace(b'\n', b'\r\n')]}:
        raise ValueError('Store checksum selection differs from exact selected package bytes/name.')
    output = private_inspection_directory(output, root, fresh=True)
    output.mkdir(mode=0o700)
    original = windows.acquire_artifact(root, output / 'portable', commit=commit, run_id=t['run_id'],
        artifact_id=t['portable_artifact_id'], artifact_sha256=t['portable_artifact_sha256'],
        files={q['portable']: q['portable_sha256'], source_name: t['source_sha256']}, token=token, portable=True)
    files = {package: q['package_sha256'], package + '.json': q['report_sha256'], package + '.sha256': t['checksum_sha256']}
    package_area, snapshot = store_artifact(root, output / 'store', commit=commit, run_id=t['run_id'],
        artifact_id=t['store_artifact_id'], artifact_sha256=t['store_artifact_sha256'], files=files, token=token)
    _, _, release = qualifier.content.store.msix.verified_portable(root, original / q['portable'], require_provenance=True)
    if release['source_sha256'] != t['source_sha256']:
        raise ValueError('Separate Store predecessor source differs from the portable corresponding source.')
    owned = dict(q, portable=str(original / q['portable']), package=str(package_area / package),
                 report=str(package_area / (package + '.json')))
    qualifier.content.qualify(root, owned['portable'], owned['package'], owned['report'],
        **{key: owned[key] for key in ['commit', 'version', 'previous_version', 'portable_sha256', 'package_sha256', 'report_sha256']})
    raw = qualifier.encode(owned)
    for area, selected in [(original, {q['portable']: q['portable_sha256'], source_name: t['source_sha256']}), (package_area, files)]:
        for name, checksum in selected.items():
            if bounded_file(area / name, transport.MAX_FILE)[0] != checksum:
                raise ValueError('Selected Store SDK original changed before handoff.')
    def read(route):
        return json.loads(transport.request('api.github.com', route, 1024 * 1024, token=token),
                          object_pairs_hook=qualifier.readiness.unique_fields)
    run = read(transport.API + 'runs/' + t['run_id'])
    artifact = read(transport.API + 'artifacts/' + t['store_artifact_id'])
    if (run, artifact) != snapshot:
        raise ValueError('Selected Store predecessor metadata changed before native handoff.')
    transport.metadata(run, read(transport.API + 'artifacts/' + t['portable_artifact_id']),
        commit=commit, run_id=t['run_id'], artifact_id=t['portable_artifact_id'],
        artifact_sha256=t['portable_artifact_sha256'], _portable=True)
    windows.stable_source(root, commit)
    with (output / 'qualification-selection.json').open('xb') as stream:
        stream.write(raw)
    if (output / 'qualification-selection.json').read_bytes() != raw:
        raise ValueError('Owned Store SDK selection changed.')
    return hashlib.sha256(raw).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'output', 'selection']:
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--selection-sha256', required=True)
    args = parser.parse_args()
    try:
        raw = qualifier.content.selected(args.selection, args.selection_sha256, 32768)[1]
        selection = json.loads(raw, object_pairs_hook=qualifier.readiness.unique_fields)
        checksum = acquire(args.source_root, args.output, selection,
            token=os.environ.pop('GITHUB_TOKEN', ''), environment=os.environ)
        if qualifier.content.selected(args.selection, args.selection_sha256, 32768)[1] != raw:
            raise ValueError('Original Store SDK selection changed.')
    except Exception:
        print('Store SDK input acquisition failed; no qualification, authentication or publication occurred.', file=sys.stderr)
        return 1
    print(checksum)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
