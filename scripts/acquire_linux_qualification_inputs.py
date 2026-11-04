"""Acquire fixed official Linux candidates; data acquisition grants no native authority."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
from urllib.parse import urlsplit

import acquire_macos_qualification_inputs as transport
from package_deb import deb_version
from package_macos_dmg import bounded_file, private_inspection_directory
import qualify_linux_deb as qualifier
from production_readiness import unique_fields

WORKFLOW = transport.REPOSITORY + '/.github/workflows/linux-qualification.yml@refs/heads/main'


def acquire_artifact(root, output, *, commit, run_id, artifact_id, artifact_sha256, files, token, portable):
    if (not transport.hexadecimal(commit, 40) or not transport.identifier(run_id)
            or not transport.identifier(artifact_id) or not transport.hexadecimal(artifact_sha256, 64)):
        raise ValueError('Official exact Linux artifact and independent digest selections required.')
    output = private_inspection_directory(output, root, fresh=True)
    run_route, artifact_route = transport.API + 'runs/' + run_id, transport.API + 'artifacts/' + artifact_id
    def read(route):
        return json.loads(transport.request('api.github.com', route, 1024 * 1024, token=token), object_pairs_hook=unique_fields)
    run, artifact = read(run_route), read(artifact_route)
    size = transport.metadata(run, artifact, commit=commit, run_id=run_id, artifact_id=artifact_id,
        artifact_sha256=artifact_sha256, _linux=True, _portable=portable)
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    private_inspection_directory(output, root, fresh=False)
    location = transport.request('api.github.com', artifact_route + '/zip', transport.MAX_ARTIFACT, token=token, redirect=True)
    parsed = urlsplit(location)
    archive = output / 'selected-artifact.zip'
    transport.request(parsed.hostname, parsed.path + ('?' + parsed.query if parsed.query else ''), transport.MAX_ARTIFACT, destination=archive)
    if bounded_file(archive, transport.MAX_ARTIFACT) != (artifact_sha256, size):
        raise ValueError('Selected outer Linux artifact differs from reviewed bytes.')
    transport.extract(archive, output, files)
    if read(run_route) != run or read(artifact_route) != artifact:
        raise ValueError('Linux predecessor metadata changed during acquisition.')
    if bounded_file(archive, transport.MAX_ARTIFACT) != (artifact_sha256, size):
        raise ValueError('Selected Linux artifact changed during readback.')
    for name, checksum in files.items():
        if bounded_file(output / name, transport.MAX_FILE)[0] != checksum:
            raise ValueError('Selected Linux inner bytes changed during readback.')
    return output


def acquire(root, output, selection, *, token, environment):
    if not isinstance(selection, dict) or set(selection) != {'commit', 'transport', 'qualification'}:
        raise ValueError('Strict Linux native input selection required.')
    commit = selection['commit']
    required = dict(GITHUB_REPOSITORY=transport.REPOSITORY, GITHUB_REF='refs/heads/main',
        GITHUB_REPOSITORY_ID=str(transport.REPOSITORY_ID), GITHUB_REPOSITORY_OWNER_ID=str(transport.OWNER_ID),
        GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=WORKFLOW,
        GITHUB_SHA=commit, GITHUB_WORKFLOW_SHA=commit, GITHUB_RUN_ATTEMPT='1',
        RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Linux', RUNNER_ARCH='X64',
        PEERBRUSH_LINUX_QUALIFICATION_ENABLED='true')
    if any(environment.get(key) != value for key, value in required.items()):
        raise ValueError('Linux qualification requires the approved exact hosted main invocation.')
    version = qualifier.stable_source(root, commit)
    q, t = selection['qualification'], selection['transport']
    if (not isinstance(q, dict) or set(q) != qualifier.INPUT_FIELDS or q['commit'] != commit or q['version'] != version
            or not isinstance(t, dict) or set(t) != {'run_id', 'portable_artifact_id', 'portable_artifact_sha256',
                'deb_artifact_id', 'deb_artifact_sha256', 'source_sha256', 'deb_files'}):
        raise ValueError('Linux native inputs require complete fixed transport and qualification selections.')
    package = f'peerbrush_{deb_version(version)}_amd64.deb'
    if (q['portable'] != f'PeerBrush-{version}-Linux-x64.zip' or q['package'] != package
            or not qualifier.sha(q['portable_sha256']) or not qualifier.sha(q['package_sha256'])
            or not qualifier.sha(t['source_sha256']) or not isinstance(t['deb_files'], dict)
            or set(t['deb_files']) != {package, package + '.json', package + '.sha256', package + '.qualification.json'}
            or t['deb_files'][package] != q['package_sha256']
            or any(not qualifier.sha(value) for value in t['deb_files'].values())):
        raise ValueError('Linux native artifact must contain the exact selected canonical files.')
    output = private_inspection_directory(output, root, fresh=True)
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    original = acquire_artifact(root, output / 'original', commit=commit, run_id=t['run_id'],
        artifact_id=t['portable_artifact_id'], artifact_sha256=t['portable_artifact_sha256'], token=token, portable=True,
        files={q['portable']: q['portable_sha256'], f'PeerBrush-{version}-source.zip': t['source_sha256']})
    media = acquire_artifact(root, output / 'media', commit=commit, run_id=t['run_id'],
        artifact_id=t['deb_artifact_id'], artifact_sha256=t['deb_artifact_sha256'], token=token, portable=False, files=t['deb_files'])
    values = dict(q, portable=str(original / q['portable']), package=str(media / package))
    checked = qualifier.inputs(root, values)
    if (bounded_file(original / f'PeerBrush-{version}-source.zip', transport.MAX_FILE)[0] != checked['record']['source_sha256']
            or checked['record']['source_sha256'] != t['source_sha256']):
        raise ValueError('Linux portable and corresponding-source bytes differ.')
    data = qualifier.encode(values) + b'\n'
    with (output / 'qualification-selection.json').open('xb') as stream:
        stream.write(data)
    qualifier.inputs(root, values)
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'selection', 'output']:
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--selection-sha256', required=True)
    args = parser.parse_args()
    try:
        selected = qualifier.read_selection(args.selection, args.selection_sha256)
        checksum = acquire(args.source_root, args.output, selected, token=os.environ.pop('GITHUB_TOKEN', ''), environment=os.environ)
        qualifier.read_selection(args.selection, args.selection_sha256)
    except Exception:
        print('Linux native input acquisition failed; no qualification or publication occurred.', file=sys.stderr)
        return 1
    print(checksum)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
