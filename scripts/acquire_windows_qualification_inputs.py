"""Acquire fixed official Windows candidates; hashes/metadata never grant native trust."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tomllib
from urllib.parse import urlsplit

import acquire_macos_qualification_inputs as transport
from package_macos_dmg import bounded_file, private_inspection_directory
import package_msix as msix
from production_readiness import unique_fields
from qualify_windows_installer import read_selection

WORKFLOW = transport.REPOSITORY + '/.github/workflows/windows-qualification.yml@refs/heads/main'


def stable_source(root, commit):
    if (not transport.hexadecimal(commit, 40)
            or transport.git(root, 'rev-parse', 'HEAD').decode().strip() != commit
            or transport.git(root, 'status', '--porcelain', '--untracked-files=no').strip()):
        raise ValueError('Windows native qualification requires clean exact committed source.')
    version = tomllib.loads(transport.git(root, 'show', 'HEAD:Cargo.toml').decode())['package']['version']
    if type(version) is not str or not re.fullmatch(transport.STABLE, version):
        raise ValueError('Windows native producer requires a stable exact version.')
    return version


def acquire_artifact(root, output, *, commit, run_id, artifact_id, artifact_sha256, files, token, portable=False):
    if (not transport.hexadecimal(commit, 40) or not transport.identifier(run_id) or not transport.identifier(artifact_id)
            or not transport.hexadecimal(artifact_sha256, 64)):
        raise ValueError('Official exact artifact and independent digest selections required.')
    output = private_inspection_directory(output, root, fresh=True)
    run_route, artifact_route = transport.API + 'runs/' + run_id, transport.API + 'artifacts/' + artifact_id
    def read(route):
        return json.loads(transport.request('api.github.com', route, 1024 * 1024, token=token), object_pairs_hook=unique_fields)
    run, artifact = read(run_route), read(artifact_route)
    expected_size = transport.metadata(run, artifact, commit=commit, run_id=run_id, artifact_id=artifact_id,
        artifact_sha256=artifact_sha256, _portable=portable, _windows=not portable)
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    private_inspection_directory(output, root, fresh=False)
    location = transport.request('api.github.com', artifact_route + '/zip', transport.MAX_ARTIFACT, token=token, redirect=True)
    parsed = urlsplit(location)
    target = parsed.path + ('?' + parsed.query if parsed.query else '')
    archive = output / 'selected-artifact.zip'
    transport.request(parsed.hostname, target, transport.MAX_ARTIFACT, destination=archive)
    if bounded_file(archive, transport.MAX_ARTIFACT) != (artifact_sha256, expected_size):
        raise ValueError('Selected outer Windows artifact differs from its reviewed bytes.')
    transport.extract(archive, output, files)
    if read(run_route) != run or read(artifact_route) != artifact:
        raise ValueError('Windows predecessor metadata changed during acquisition.')
    if bounded_file(archive, transport.MAX_ARTIFACT) != (artifact_sha256, expected_size):
        raise ValueError('Selected Windows artifact changed during readback.')
    for name, checksum in files.items():
        if bounded_file(output / name, transport.MAX_FILE)[0] != checksum:
            raise ValueError('Selected Windows inner bytes changed during readback.')
    return output


def acquire(root, output, selection, *, token, environment):
    if not isinstance(selection, dict) or set(selection) != {'commit', 'transport', 'qualification'}:
        raise ValueError('Strict Windows native input selection required.')
    commit = selection['commit']
    required = dict(GITHUB_REPOSITORY=transport.REPOSITORY, GITHUB_REF='refs/heads/main',
        GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=WORKFLOW,
        GITHUB_SHA=commit, GITHUB_WORKFLOW_SHA=commit, GITHUB_RUN_ATTEMPT='1',
        RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Windows', RUNNER_ARCH='X64',
        PEERBRUSH_WINDOWS_QUALIFICATION_ENABLED='true')
    if any(environment.get(key) != value for key, value in required.items()):
        raise ValueError('Windows qualification requires the approved exact hosted main invocation.')
    version = stable_source(root, commit)
    q, t = selection['qualification'], selection['transport']
    paths = {'portable', 'unsigned', 'package', 'runtime', 'identity', 'runtime_license'}
    public = {'commit', 'version', 'package_family', 'portable_sha256', 'unsigned_sha256', 'package_sha256',
        'certificate_sha256', 'runtime_sha256', 'runtime_certificate_sha256', 'runtime_license_sha256',
        'descriptor_uri', 'package_uri', 'runtime_uri', 'signtool', 'signtool_sha256', 'powershell', 'powershell_sha256',
        'timestamp_certificate_sha256', 'executable_timestamp_certificate_sha256', 'runtime_timestamp_certificate_sha256'}
    if (not isinstance(q, dict) or set(q) != paths | public or q['commit'] != commit or q['version'] != version
            or not isinstance(t, dict) or set(t) != {'portable_run_id', 'portable_artifact_id', 'portable_artifact_sha256',
                'signing_run_id', 'signing_artifact_id', 'signing_artifact_sha256', 'source_sha256', 'files'}):
        raise ValueError('Windows native inputs require complete fixed source, transport and qualification selections.')
    target = msix.package_version(version)
    if (q['portable'] != f'PeerBrush-{version}-Windows-x64.zip' or q['unsigned'] != f'PeerBrush-{target}-Windows-x64.msix'
            or type(q['package']) is not str or not re.fullmatch(re.escape(q['unsigned'][:-5]) + r'-signed-r[1-9][0-9]*\.msix', q['package'])
            or type(q['runtime']) is not str or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,126}\.(?:appx|msix)', q['runtime'])
            or q['identity'] != 'direct-identity.json' or q['runtime_license'] != 'runtime-license.txt'
            or not isinstance(t['files'], dict) or set(t['files']) != {q[name] for name in paths}
            or len(t['files']) != 6 or t['files'][q['portable']] != q['portable_sha256']):
        raise ValueError('Windows native artifact must contain the exact six selected canonical files.')
    for key in ['unsigned', 'package', 'runtime', 'runtime_license']:
        if t['files'][q[key]] != q[key + '_sha256']:
            raise ValueError('Windows inner artifact and qualification digests differ.')
    output = private_inspection_directory(output, root, fresh=True)
    output.mkdir(mode=0o700)
    source_name = f'PeerBrush-{version}-source.zip'
    if not transport.hexadecimal(t['source_sha256'], 64):
        raise ValueError('Trusted portable artifact requires its matching corresponding-source digest.')
    original = acquire_artifact(root, output / 'original', commit=commit, run_id=t['portable_run_id'],
        artifact_id=t['portable_artifact_id'], artifact_sha256=t['portable_artifact_sha256'],
        files={q['portable']: q['portable_sha256'], source_name: t['source_sha256']}, token=token, portable=True)
    media = acquire_artifact(root, output / 'media', commit=commit, run_id=t['signing_run_id'],
        artifact_id=t['signing_artifact_id'], artifact_sha256=t['signing_artifact_sha256'], files=t['files'], token=token)
    if bounded_file(original / q['portable'], transport.MAX_FILE) != bounded_file(media / q['portable'], transport.MAX_FILE):
        raise ValueError('Signing bundle substituted the trusted original portable.')
    _, _, release = msix.verified_portable(root, original / q['portable'], require_provenance=True)
    if release['source_sha256'] != t['source_sha256']:
        raise ValueError('Trusted portable and corresponding-source bytes differ.')
    values = dict(q)
    for name in paths:
        values[name] = str(media / q[name])
    values.update(work=str(output.parent / 'peerbrush-windows-inspection'), output=str(output.parent / 'peerbrush-windows-descriptor'))
    data = (json.dumps(values, sort_keys=True, indent=2) + '\n').encode()
    with (output / 'qualification-selection.json').open('xb') as stream:
        stream.write(data)
    stable_source(root, commit)
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'selection', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--selection-sha256', required=True)
    args = parser.parse_args()
    try:
        selected = read_selection(args.selection, args.selection_sha256)
        checksum = acquire(args.source_root, args.output, selected, token=os.environ.pop('GITHUB_TOKEN', ''), environment=os.environ)
        read_selection(args.selection, args.selection_sha256)
    except Exception:
        print('Windows native input acquisition failed; no qualification or publication occurred.', file=sys.stderr)
        return 1
    print(checksum)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
