"""Acquire two pinned final Apple files from one exact official Actions artifact.

No signing, attestation, installation, publication or arbitrary download URLs.
"""
import argparse
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import re
import stat
import struct
import sys
import time
import tomllib
import urllib.parse
import zipfile

from package import git
from package_macos_dmg import bounded_file, private_inspection_directory
from production_readiness import unique_fields

REPOSITORY = 'Deftr0y/PeerBrush'
REPOSITORY_ID = 1407648785
OWNER_ID = 21202494
WORKFLOW = REPOSITORY + '/.github/workflows/macos-qualification.yml@refs/heads/main'
SIGNING_WORKFLOW = '.github/workflows/macos-release-signing.yml'
API = '/repos/' + REPOSITORY + '/actions/'
MAX_FILE = 512 * 1024 * 1024
MAX_ARTIFACT = 2 * MAX_FILE + 1024 * 1024
NUMERIC = r'(?:0|[1-9][0-9]*)'
STABLE = NUMERIC + r'\.' + NUMERIC + r'\.' + NUMERIC


def hexadecimal(value, length):
    return type(value) is str and re.fullmatch('[0-9a-f]{' + str(length) + '}', value)


def identifier(value):
    return type(value) is str and re.fullmatch('[1-9][0-9]{0,19}', value)


def selected(root, commit, run_id, artifact_id, artifact_sha256, archive_name,
             archive_sha256, image_name, image_sha256, team_id, environment):
    required = dict(GITHUB_REPOSITORY=REPOSITORY, GITHUB_REF='refs/heads/main',
        GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_WORKFLOW_REF=WORKFLOW,
        GITHUB_SHA=commit, GITHUB_WORKFLOW_SHA=commit, GITHUB_RUN_ATTEMPT='1',
        RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='macOS', RUNNER_ARCH='ARM64',
        PEERBRUSH_NATIVE_QUALIFICATION_ENABLED='true')
    if (not hexadecimal(commit, 40) or not identifier(run_id) or not identifier(artifact_id)
            or any(not hexadecimal(value, 64) for value in [artifact_sha256, archive_sha256, image_sha256])
            or type(team_id) is not str or not re.fullmatch('[A-Z0-9]{10}', team_id)
            or any(environment.get(key) != value for key, value in required.items())):
        raise ValueError('Native qualification requires explicitly enabled exact official main workflow selections.')
    if (git(root, 'rev-parse', 'HEAD').decode().strip() != commit
            or git(root, 'status', '--porcelain', '--untracked-files=no').strip()):
        raise ValueError('Native qualification requires a clean exact source checkout.')
    version = tomllib.loads(git(root, 'show', 'HEAD:Cargo.toml').decode('utf-8'))['package']['version']
    if type(version) is not str or len(version) > 96 or not re.fullmatch(STABLE, version):
        raise ValueError('Native qualification accepts stable Cargo versions only.')
    if (type(archive_name) is not str or len(archive_name) > 192
            or not re.fullmatch('PeerBrush-' + re.escape(version) + r'-macOS-arm64-apple-signed-r[1-9][0-9]*\.zip', archive_name)
            or type(image_name) is not str or len(image_name) > 224
            or not re.fullmatch(re.escape(archive_name[:-4]) + r'-dmg-signed-r[1-9][0-9]*\.dmg', image_name)):
        raise ValueError('Native qualification requires exact revisioned signed ZIP and DMG filenames.')
    return version


def storage_url(url):
    try:
        p = urllib.parse.urlsplit(url)
        return (type(url) is str and len(url) <= 8192 and url.isascii()
            and not any(ord(c) < 33 for c in url) and p.scheme == 'https'
            and not p.username and not p.password and p.port is None and not p.fragment
            and bool(p.path) and p.path.startswith('/') and not p.path.startswith('//')
            and (p.hostname == 'results-receiver.actions.githubusercontent.com'
                 or re.fullmatch('[a-z0-9]{3,24}\\.blob\\.core\\.windows\\.net', p.hostname or '')))
    except (ValueError, TypeError):
        return False


def request(host, target, limit, *, token=None, destination=None, redirect=False):
    """Fixed HTTPS GET only. Never return/log tokens or signed storage locations."""
    if (type(target) is not str or not target.startswith('/') or target.startswith('//')
            or not target.isascii() or any(ord(c) < 33 for c in target)
            or type(limit) is not int or not 0 < limit <= MAX_ARTIFACT
            or (host == 'api.github.com' and not re.fullmatch(re.escape(API) + r'(?:runs/[1-9][0-9]{0,19}|artifacts/[1-9][0-9]{0,19}(?:/zip)?)', target))
            or (token is not None and host != 'api.github.com')
            or (host != 'api.github.com' and not storage_url('https://' + host + target))
            or (destination is None and limit > 1024 * 1024)):
        raise ValueError('Unexpected native artifact GET selection.')
    headers = {'User-Agent': 'PeerBrush-native-artifact-verifier',
               'Accept': 'application/vnd.github+json' if host == 'api.github.com' else 'application/octet-stream',
               'Cache-Control': 'no-cache', 'Pragma': 'no-cache'}
    if token is not None:
        if type(token) is not str or not 1 <= len(token) <= 8192 or any(ord(c) < 33 or ord(c) > 126 for c in token):
            raise ValueError('Missing bounded job read token.')
        headers.update(Authorization='Bearer ' + token)
    if host == 'api.github.com':
        headers['X-GitHub-Api-Version'] = '2022-11-28'
    connection = http.client.HTTPSConnection(host, timeout=15)
    deadline = time.monotonic() + 120
    try:
        connection.request('GET', target, headers=headers)
        with connection.getresponse() as response:
            if redirect:
                location = response.getheader('Location')
                if host != 'api.github.com' or response.status != 302 or not storage_url(location):
                    raise ValueError('Native artifact download did not return one allowed storage redirect.')
                return location
            if response.status != 200:
                raise ValueError('Selected native artifact GET was unavailable.')
            lengths = [value for key, value in response.getheaders() if key.lower() == 'content-length']
            if (len(lengths) > 1 or (lengths and (not lengths[0].isdecimal() or int(lengths[0]) > limit))
                    or response.getheader('Content-Encoding') not in [None, 'identity']):
                raise ValueError('Selected native artifact response exceeds its bounded contract.')
            total = 0
            sink = destination.open('xb') if destination is not None else io.BytesIO()
            with sink:
                while True:
                    if time.monotonic() >= deadline:
                        raise ValueError('Native artifact body transfer exceeded its deadline.')
                    chunk = response.read1(min(64 * 1024, limit + 1 - total))
                    if time.monotonic() >= deadline:
                        raise ValueError('Native artifact body transfer exceeded its deadline.')
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > limit:
                        raise ValueError('Native artifact transfer exceeds its byte budget.')
                    sink.write(chunk)
                result = sink.getvalue() if destination is None else destination
            if lengths and total != int(lengths[0]):
                raise ValueError('Native artifact response was truncated.')
            return result
    except (OSError, http.client.HTTPException):
        raise ValueError('Native artifact GET failed; no qualification was generated.') from None
    finally:
        connection.close()


def metadata(run, artifact, *, commit, run_id, artifact_id, artifact_sha256, _windows=False, _portable=False, _linux=False):
    if (type(_windows) is not bool or type(_portable) is not bool or type(_linux) is not bool
            or (_windows and (_portable or _linux))):
        raise ValueError('Native predecessor uses a fixed platform adapter.')
    workflow = '.github/workflows/release.yml' if (_portable or _linux) else '.github/workflows/windows-release-signing.yml' if _windows else SIGNING_WORKFLOW
    artifact_name = ('candidate-Linux' if _portable else 'DEB-installation-candidate') if _linux else (
        'candidate-Windows' if _portable else ('windows-signed-inputs-' if _windows else 'macos-signed-pair-') + run_id + '-1')
    repo = dict(id=REPOSITORY_ID, full_name=REPOSITORY)
    if not isinstance(run, dict) or not isinstance(artifact, dict):
        raise ValueError('Native predecessor metadata must be objects.')
    for key in ['repository', 'head_repository']:
        value = run.get(key)
        if (not isinstance(value, dict) or any(value.get(k) != v or type(value.get(k)) is not type(v) for k, v in repo.items())
                or value.get('fork') is not False or value.get('private') is not False
                or not isinstance(value.get('owner'), dict) or type(value['owner'].get('id')) is not int
                or value['owner']['id'] != OWNER_ID):
            raise ValueError('Native predecessor repository or fork differs from the official public source.')
    if (type(run.get('id')) is not int or run['id'] != int(run_id)
            or type(run.get('run_attempt')) is not int or run['run_attempt'] != 1
            or run.get('status') != 'completed' or run.get('conclusion') != 'success'
            or run.get('event') != 'workflow_dispatch' or run.get('head_branch') != 'main'
            or run.get('path') not in {workflow, workflow + '@main'}
            or run.get('head_sha') != commit or run.get('pull_requests') != []):
        raise ValueError('Native predecessor is not the exact successful first main attempt.')
    expected = dict(id=int(artifact_id), name=artifact_name,
        expired=False, digest='sha256:' + artifact_sha256,
        url='https://api.github.com' + API + 'artifacts/' + artifact_id,
        archive_download_url='https://api.github.com' + API + 'artifacts/' + artifact_id + '/zip')
    if (any(artifact.get(key) != value or type(artifact.get(key)) is not type(value) for key, value in expected.items())
            or type(artifact.get('size_in_bytes')) is not int or not 0 < artifact['size_in_bytes'] <= MAX_ARTIFACT):
        raise ValueError('Native artifact differs from its independently selected ID, digest or size.')
    origin = artifact.get('workflow_run')
    expected_origin = dict(id=int(run_id), repository_id=REPOSITORY_ID, head_repository_id=REPOSITORY_ID,
                           head_branch='main', head_sha=commit)
    if not isinstance(origin, dict) or any(origin.get(key) != value or type(origin.get(key)) is not type(value) for key, value in expected_origin.items()):
        raise ValueError('Native artifact does not belong to the selected official main source run.')
    return artifact['size_in_bytes']


def extract(archive, output, selected_files):
    if (not isinstance(selected_files, dict) or not 1 <= len(selected_files) <= 8
            or any(type(name) is not str or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,223}', name)
                   or not hexadecimal(checksum, 64) for name, checksum in selected_files.items())
            or len({name.casefold() for name in selected_files}) != len(selected_files)):
        raise ValueError('Native input files require a bounded unique basename/hash selection.')
    # Preflight the ordinary small central directory before ZipFile allocates
    # entries. The fixed bounded file selections require no ZIP64 or comments.
    if archive.stat().st_size < 22:
        raise ValueError('Missing bounded native artifact ZIP directory.')
    with archive.open('rb') as stream:
        stream.seek(-22, os.SEEK_END)
        end = stream.read(22)
    if len(end) != 22:
        raise ValueError('Missing bounded native artifact ZIP directory.')
    signature, disk, directory_disk, count_disk, count, size, offset, comment = struct.unpack('<4s4H2IH', end)
    if (signature != b'PK\x05\x06' or disk != 0 or directory_disk != 0
            or count_disk != len(selected_files) or count != len(selected_files) or not 0 < size <= 4096 or comment != 0
            or offset + size != archive.stat().st_size - 22):
        raise ValueError('Native input artifact must match its bounded ordinary ZIP entry selection.')
    with zipfile.ZipFile(archive) as source:
        members = source.infolist()
        if (len(members) != len(selected_files) or {member.filename for member in members} != set(selected_files)
                or sum(member.file_size for member in members) > MAX_ARTIFACT):
            raise ValueError('Native artifact inventory differs from the selected final files.')
        for member in members:
            kind = stat.S_IFMT(member.external_attr >> 16)
            if (member.is_dir() or member.external_attr & 0x10 or kind not in [0, stat.S_IFREG]
                    or member.flag_bits & 1 or member.compress_type not in [zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED]
                    or not 0 < member.file_size <= MAX_FILE or not 0 < member.compress_size <= MAX_FILE):
                raise ValueError('Native artifact entries must be bounded regular unencrypted files.')
            target = output / member.filename
            checksum, total = hashlib.sha256(), 0
            with source.open(member) as stream, target.open('xb') as sink:
                while True:
                    chunk = stream.read(min(64 * 1024, member.file_size + 1 - total))
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > member.file_size:
                        raise ValueError('Native input ZIP entry exceeds its declared size.')
                    checksum.update(chunk)
                    sink.write(chunk)
            target.chmod(0o400)
            if total != member.file_size or checksum.hexdigest() != selected_files[member.filename]:
                raise ValueError('Native final file bytes differ from the independent inner hash selections.')
            if bounded_file(target, MAX_FILE) != (selected_files[member.filename], total):
                raise ValueError('Native extracted file changed during readback.')


def acquire(root, output, *, commit, run_id, artifact_id, artifact_sha256, archive_name,
            archive_sha256, image_name, image_sha256, team_id, token, environment):
    inputs = dict(commit=commit, run_id=run_id, artifact_id=artifact_id, artifact_sha256=artifact_sha256,
        archive_name=archive_name, archive_sha256=archive_sha256, image_name=image_name,
        image_sha256=image_sha256, team_id=team_id)
    version = selected(root, **inputs, environment=environment)
    output = private_inspection_directory(output, root, fresh=True)
    run_route, artifact_route = API + 'runs/' + run_id, API + 'artifacts/' + artifact_id
    def read(route):
        return json.loads(request('api.github.com', route, 1024 * 1024, token=token), object_pairs_hook=unique_fields)
    first_run, first_artifact = read(run_route), read(artifact_route)
    size = metadata(first_run, first_artifact, commit=commit, run_id=run_id,
                    artifact_id=artifact_id, artifact_sha256=artifact_sha256)
    private_inspection_directory(output, root, fresh=True)
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    private_inspection_directory(output, root, fresh=False)
    location = request('api.github.com', artifact_route + '/zip', 8192, token=token, redirect=True)
    p = urllib.parse.urlsplit(location)
    wrapper = output / 'selected-artifact.zip'
    request(p.hostname, p.path + ('?' + p.query if p.query else ''), MAX_ARTIFACT, destination=wrapper)
    wrapper.chmod(0o400)
    if bounded_file(wrapper, MAX_ARTIFACT) != (artifact_sha256, size):
        raise ValueError('Downloaded native artifact differs from its independent outer digest/size.')
    extract(wrapper, output, {archive_name: archive_sha256, image_name: image_sha256})
    if (read(run_route) != first_run or read(artifact_route) != first_artifact
            or selected(root, **inputs, environment=environment) != version
            or bounded_file(wrapper, MAX_ARTIFACT) != (artifact_sha256, size)):
        raise ValueError('Native input metadata, source or selected artifact changed during acquisition.')
    for name, checksum in [(archive_name, archive_sha256), (image_name, image_sha256)]:
        if bounded_file(output / name, MAX_FILE)[0] != checksum:
            raise ValueError('Extracted native inputs changed before qualification handoff.')
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-root', type=Path, required=True)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--check-selection', action='store_true')
    for name in ['commit', 'run-id', 'artifact-id', 'artifact-sha256', 'archive-name',
                 'archive-sha256', 'image-name', 'image-sha256', 'team-id']:
        parser.add_argument('--' + name, required=True)
    values = vars(parser.parse_args())
    root, output, checking = values.pop('source_root'), values.pop('output'), values.pop('check_selection')
    try:
        if checking:
            version = selected(root, **values, environment=os.environ)
        else:
            if output is None:
                raise ValueError('Choose a new private external output directory.')
            token = os.environ.pop('GITHUB_TOKEN', '')
            version = acquire(root, output, **values, token=token, environment=os.environ)
        print('Selected stable source ' + version + '; acquisition alone grants no native or release readiness.')
        return 0
    except Exception:
        print('Selected native inputs could not be verified; no qualification was generated.', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
