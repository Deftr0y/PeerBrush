"""Acquire one independently pinned final-input data producer; never native authority."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import struct
import sys
from urllib.parse import urlsplit
import zipfile

import acquire_macos_qualification_inputs as transport
import final_release as final
import prepare_final_inputs as inputs
from package_macos_dmg import bounded_file, private_inspection_directory
from production_readiness import unique_fields


def official_run(value, *, commit, run_id, final_inputs):
    workflow = inputs.WORKFLOW if final_inputs else '.github/workflows/release.yml'
    for field in ['repository', 'head_repository']:
        repository = value.get(field) if isinstance(value, dict) else None
        expected = dict(id=transport.REPOSITORY_ID, full_name=transport.REPOSITORY, fork=False, private=False)
        if (not isinstance(repository, dict)
                or any(repository.get(k) != v or type(repository.get(k)) is not type(v) for k, v in expected.items())
                or not isinstance(repository.get('owner'), dict)
                or type(repository['owner'].get('id')) is not int
                or repository['owner']['id'] != transport.OWNER_ID):
            raise ValueError('Data producer is not the official public nonfork repository.')
    if (type(value.get('id')) is not int or value['id'] != int(run_id)
            or type(value.get('run_attempt')) is not int or value['run_attempt'] != 1
            or value.get('status') != 'completed' or value.get('conclusion') != 'success'
            or value.get('event') not in ({'workflow_dispatch'} if final_inputs else {'push', 'workflow_dispatch'})
            or value.get('head_branch') != 'main' or value.get('head_sha') != commit
            or value.get('path') not in {workflow, workflow + '@main'} or value.get('pull_requests') != []):
        raise ValueError('Selected data producer/predecessor is not its exact successful first main attempt.')


def metadata(run, artifact, *, commit, run_id, artifact_id, artifact_sha256):
    official_run(run, commit=commit, run_id=run_id, final_inputs=True)
    expected = dict(id=int(artifact_id), name='final-inputs-' + run_id + '-1', expired=False,
        digest='sha256:' + artifact_sha256,
        url='https://api.github.com' + transport.API + 'artifacts/' + artifact_id,
        archive_download_url='https://api.github.com' + transport.API + 'artifacts/' + artifact_id + '/zip')
    if (not isinstance(artifact, dict)
            or any(artifact.get(k) != v or type(artifact.get(k)) is not type(v) for k, v in expected.items())
            or type(artifact.get('size_in_bytes')) is not int
            or not 0 < artifact['size_in_bytes'] <= transport.MAX_ARTIFACT):
        raise ValueError('Final-input artifact ID, name, outer digest or bounded size differs.')
    origin = artifact.get('workflow_run')
    expected = dict(id=int(run_id), repository_id=transport.REPOSITORY_ID,
                    head_repository_id=transport.REPOSITORY_ID, head_branch='main', head_sha=commit)
    if (not isinstance(origin, dict)
            or any(origin.get(k) != v or type(origin.get(k)) is not type(v) for k, v in expected.items())):
        raise ValueError('Final-input artifact belongs to another run or source.')
    return artifact['size_in_bytes']


def extract(root, archive, output, index, selection_sha256):
    records = inputs.checked_index(index, index['commit'], selection_sha256)
    archive = final.readiness.handoff_path(archive)
    output = private_inspection_directory(output, root, fresh=True)
    before = bounded_file(archive, transport.MAX_FILE)
    if before[1] < 22:
        raise ValueError('Missing ordinary final-input ZIP directory.')
    with archive.open('rb') as stream:
        if stream.read(4) != b'PK\x03\x04':
            raise ValueError('Final-input ZIP must start at its first regular member.')
        stream.seek(-22, os.SEEK_END)
        end = stream.read(22)
    signature, disk, directory_disk, count_disk, count, size, offset, comment = struct.unpack('<4s4H2IH', end)
    if (signature != b'PK\x05\x06' or disk != 0 or directory_disk != 0 or count_disk != len(records)
            or count != len(records) or not 0 < size <= inputs.MAX_FILES * 512 or comment != 0
            or offset + size != before[1] - 22):
        raise ValueError('Final-input ZIP must have its exact small ordinary central directory.')
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    with zipfile.ZipFile(archive) as source:
        members = source.infolist()
        if len(members) != len(records) or {member.filename for member in members} != set(records):
            raise ValueError('Final-input ZIP inventory differs from its independently selected index.')
        for member in members:
            private_inspection_directory(output, root, fresh=False)
            record = records[member.filename]
            if (member.is_dir() or member.external_attr & 0x10
                    or stat.S_IFMT(member.external_attr >> 16) not in [0, stat.S_IFREG]
                    or member.flag_bits & 1 or member.compress_type not in [zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED]
                    or member.file_size != record['bytes'] or not 0 < member.compress_size <= transport.MAX_FILE):
                raise ValueError('Final-input member is not the exact bounded regular data entry.')
            target = output / member.filename
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            final.readiness.handoff_path(target, new=True)
            checksum, total = hashlib.sha256(), 0
            with source.open(member) as stream, target.open('xb') as sink:
                while True:
                    chunk = stream.read(min(64 * 1024, record['bytes'] + 1 - total))
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > record['bytes']:
                        raise ValueError('Final-input member grew beyond its declared budget.')
                    checksum.update(chunk)
                    sink.write(chunk)
            target.chmod(0o400)
            if (checksum.hexdigest(), total) != (record['sha256'], record['bytes']):
                raise ValueError('Extracted final-input member differs from independent inner hashes.')
            if bounded_file(target, record['bytes']) != (record['sha256'], record['bytes']):
                raise ValueError('Owned final-input member changed during extraction readback.')
    if bounded_file(archive, transport.MAX_FILE) != before:
        raise ValueError('Final-input archive changed during extraction.')


def acquire(root, output, *, commit, candidate_run_id, run_id, artifact_id, artifact_sha256,
            bundle_sha256, index_sha256, selection_sha256, token):
    if (not transport.hexadecimal(commit, 40)
            or any(not transport.identifier(value) for value in [candidate_run_id, run_id, artifact_id])
            or candidate_run_id == run_id
            or any(not transport.hexadecimal(value, 64)
                   for value in [artifact_sha256, bundle_sha256, index_sha256, selection_sha256])):
        raise ValueError('Independent exact predecessor, producer, artifact and byte selections required.')
    final.signed._source(root, commit)
    output = private_inspection_directory(output, root, fresh=True)
    def read(route):
        return json.loads(transport.request('api.github.com', route, 1024 * 1024, token=token), object_pairs_hook=unique_fields)
    predecessor_route = transport.API + 'runs/' + candidate_run_id
    run_route, artifact_route = transport.API + 'runs/' + run_id, transport.API + 'artifacts/' + artifact_id
    predecessor, run, artifact = read(predecessor_route), read(run_route), read(artifact_route)
    official_run(predecessor, commit=commit, run_id=candidate_run_id, final_inputs=False)
    size = metadata(run, artifact, commit=commit, run_id=run_id, artifact_id=artifact_id, artifact_sha256=artifact_sha256)
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    location = transport.request('api.github.com', artifact_route + '/zip', 8192, token=token, redirect=True)
    parsed = urlsplit(location)
    wrapper = output / 'selected-artifact.zip'
    transport.request(parsed.hostname, parsed.path + ('?' + parsed.query if parsed.query else ''),
                      transport.MAX_ARTIFACT, destination=wrapper)
    wrapper.chmod(0o400)
    if bounded_file(wrapper, transport.MAX_ARTIFACT) != (artifact_sha256, size):
        raise ValueError('Downloaded final-input wrapper differs from its independent outer digest/size.')
    transport.extract(wrapper, output, {inputs.ARCHIVE: bundle_sha256, inputs.INDEX: index_sha256})
    index = inputs.json_input(output / inputs.INDEX, inputs.MAX_INDEX, index_sha256)
    inputs.checked_index(index, commit, selection_sha256)
    extract(root, output / inputs.ARCHIVE, output / 'data', index, selection_sha256)
    inputs.verify_data(root, output / 'data', index, selection_sha256)
    if (read(predecessor_route) != predecessor or read(run_route) != run or read(artifact_route) != artifact
            or bounded_file(wrapper, transport.MAX_ARTIFACT) != (artifact_sha256, size)):
        raise ValueError('Selected predecessor, producer or wrapper changed during acquisition.')
    for name, checksum, limit in [(inputs.ARCHIVE, bundle_sha256, transport.MAX_FILE),
                                  (inputs.INDEX, index_sha256, inputs.MAX_INDEX)]:
        if bounded_file(output / name, limit)[0] != checksum:
            raise ValueError('Original selected final-input data changed during acquisition.')
    final.signed._source(root, commit)
    return index


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ['commit', 'candidate-run-id', 'run-id', 'artifact-id', 'artifact-sha256',
                 'bundle-sha256', 'index-sha256', 'selection-sha256']:
        parser.add_argument('--' + name, required=True)
    args = vars(parser.parse_args(argv))
    root = args.pop('source_root')
    try:
        acquire(root, **args, token=os.environ.pop('GITHUB_TOKEN', ''))
        print('Selected final-input data acquired; all actual native consumers must run before composition.')
        return 0
    except Exception:
        print('Final-input acquisition failed; preserve private partial data. No native authority was granted.', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
