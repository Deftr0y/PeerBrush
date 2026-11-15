"""Verify reviewed catalog definitions and their downloads from public GitHub."""
import argparse
import hashlib
import http.client
import json
from pathlib import Path, PurePosixPath
import re
import tempfile
import time
from urllib.parse import quote

from final_release import read
from package_macos_dmg import bounded_file
from prepare_site_release import (API as RELEASE_API, DOWNLOAD, MAX_DMG_QUALIFICATION_BYTES, RELEASE, STABLE,
                                  public_get as public_release_get, sha)
from production_readiness import unique_fields
from test_homebrew_upgrade import select as select_homebrew
from test_nix_profile_upgrade import definition_inventory, source_inputs
from test_scoop_upgrade import select as select_scoop


API = 'api.github.com'
RAW = 'raw.githubusercontent.com'
COMMIT = r'[0-9a-f]{40}'
DIGEST = r'[0-9a-f]{64}'
REPOSITORY = r'[a-zA-Z0-9][a-zA-Z0-9-]{0,38}/[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,99}'
BRANCH = r'[a-zA-Z0-9_-][a-zA-Z0-9_.-]*(?:/[a-zA-Z0-9_-][a-zA-Z0-9_.-]*)*'
JSON_LIMIT = 2 * 1024 * 1024
FILE_LIMIT = 32 * 1024 * 1024
TOTAL_LIMIT = 128 * 1024 * 1024
ASSET_LIMIT = 512 * 1024 * 1024


def public_get(host, path, limit):
    deadline = time.monotonic() + 120
    connection = http.client.HTTPSConnection(host, timeout=15)
    try:
        connection.request('GET', path, headers={
            'User-Agent': 'PeerBrush-package-catalog-readback', 'Accept-Encoding': 'identity',
            'Cache-Control': 'no-cache', 'Pragma': 'no-cache',
            'Accept': 'application/vnd.github+json' if host == API else 'application/octet-stream',
        })
        with connection.getresponse() as response:
            if response.status != 200:
                raise ValueError('Public catalog GET requires a direct HTTP 200 response.')
            lengths = response.headers.get_all('Content-Length', [])
            encodings = response.headers.get_all('Content-Encoding', [])
            framing = response.headers.get_all('Transfer-Encoding', [])
            if (encodings not in ([], ['identity']) or framing not in ([], ['chunked'])
                    or (framing and lengths) or response.headers.get_all('Content-Range', [])
                    or (lengths and (len(lengths) != 1 or not lengths[0].isdecimal() or int(lengths[0]) > limit))):
                raise ValueError('Public catalog response has invalid framing or exceeds its size budget.')
            data = bytearray()
            while time.monotonic() < deadline:
                chunk = response.read1(min(64 * 1024, limit + 1 - len(data)))
                if time.monotonic() >= deadline:
                    break
                if not chunk:
                    if lengths and len(data) != int(lengths[0]):
                        raise ValueError('Public catalog response was truncated.')
                    return bytes(data)
                data.extend(chunk)
                if len(data) > limit:
                    raise ValueError('Public catalog response exceeds its size budget.')
            raise ValueError('Public catalog GET exceeded its transfer deadline.')
    except (OSError, http.client.HTTPException):
        raise ValueError('Public catalog GET failed; no readback evidence was produced.') from None
    finally:
        connection.close()


def selected(channel, directory, digest, tag, source_commit, team):
    receipt = 'nix-package.json' if channel == 'nix' else 'package-managers.json'
    report = json.loads(read(directory / receipt, 512 * 1024, digest), object_pairs_hook=unique_fields)
    version = tag[1:] if channel == 'nix' else tag
    if not isinstance(report, dict) or report.get('version') != version or report.get('commit') != source_commit:
        raise ValueError('Definition receipt differs from the selected release version or source commit.')
    if channel == 'nix':
        files, executable = definition_inventory(report)
        return report, files, executable
    _, _, definition = (select_scoop((directory, digest)) if channel == 'scoop'
                        else select_homebrew((directory, digest), team))
    return report, {('peerbrush.json' if channel == 'scoop' else 'peerbrush.rb'): sha(definition)}, []


def release_downloads(channel, definition, tag, source_commit, get):
    source_name = f'PeerBrush-{tag[1:]}-source.zip'
    if channel == 'nix':
        inputs = definition['inputs']
        expected = {source_name: (inputs.get('source_url'), inputs['source_sha256'], FILE_LIMIT * 2)}
    else:
        asset = definition['channels'][channel]['asset']
        expected = {
            source_name: (definition.get('source_url'), asset['source_sha256'], FILE_LIMIT * 2),
            asset['file']: (asset['url'], asset['sha256'], ASSET_LIMIT),
        }
        if channel == 'homebrew' and asset['file'].endswith('.dmg'):
            companion = asset['signed_zip']
            qualification = asset['file'] + '.qualification.json'
            expected[companion['file']] = (
                DOWNLOAD + tag + '/' + quote(companion['file'], safe=''),
                companion['sha256'], ASSET_LIMIT,
            )
            expected[qualification] = (
                DOWNLOAD + tag + '/' + quote(qualification, safe=''),
                asset['qualification_sha256'], MAX_DMG_QUALIFICATION_BYTES,
            )
    for name, (url, checksum, _) in expected.items():
        if url != DOWNLOAD + tag + '/' + quote(name, safe='') or not re.fullmatch(DIGEST, checksum):
            raise ValueError('Package definition does not select exact versioned public downloads.')

    def observed_assets():
        release = json.loads(get(RELEASE_API + 'releases/tags/' + tag, JSON_LIMIT, api=True), object_pairs_hook=unique_fields)
        commit = json.loads(get(RELEASE_API + 'commits/' + tag, JSON_LIMIT, api=True), object_pairs_hook=unique_fields)
        if (not isinstance(release, dict) or release.get('tag_name') != tag or release.get('html_url') != RELEASE + tag
                or release.get('draft') is not False or release.get('prerelease') is not False
                or not isinstance(commit, dict) or commit.get('sha') != source_commit):
            raise ValueError('Public package downloads are not bound to the selected stable release.')
        assets = release.get('assets')
        if (not isinstance(assets, list) or len(assets) > 100
                or any(not isinstance(row, dict) or not isinstance(row.get('name'), str) for row in assets)
                or len({row['name'] for row in assets}) != len(assets)):
            raise ValueError('Public package asset inventory is ambiguous or oversized.')
        assets = {row['name']: row for row in assets}
        selected = {}
        for name, (url, checksum, limit) in expected.items():
            row = assets.get(name, {})
            size = row.get('size')
            if (row.get('browser_download_url') != url or row.get('state') != 'uploaded'
                    or type(size) is not int or not 0 < size <= limit
                    or row.get('digest') not in (None, 'sha256:' + checksum)):
                raise ValueError('Public package asset metadata differs from the selected download.')
            selected[name] = size, row.get('digest')
        return selected

    first = observed_assets()
    verified = {}
    parent = Path(tempfile.gettempdir()).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='peerbrush-catalog-readback-', dir=parent) as temporary:
        for name, (url, checksum, _) in expected.items():
            size, _ = first[name]
            path = Path(temporary) / name
            get(url, size, destination=path)
            if bounded_file(path, size) != (checksum, size):
                raise ValueError('Public package download differs from its authenticated bytes.')
            verified[name] = dict(bytes=size, sha256=checksum)
    if observed_assets() != first:
        raise ValueError('Public package asset metadata changed during download readback.')
    return verified


def check(channel, directory, digest, *, tag, source_commit, repository, catalog_commit, team=None,
          get=public_get, get_release=public_release_get):
    if (channel not in ('scoop', 'homebrew', 'nix') or not re.fullmatch(DIGEST, digest)
            or not re.fullmatch('v' + STABLE, tag) or not re.fullmatch(COMMIT, source_commit)
            or not re.fullmatch(REPOSITORY, repository) or not re.fullmatch(COMMIT, catalog_commit)
            or (channel == 'homebrew' and (team is None or not re.fullmatch('[A-Z0-9]{10}', team)))
            or (channel != 'homebrew' and team is not None)):
        raise ValueError('Select authenticated release and public catalog pins for the requested channel.')
    definition, files, executable = selected(channel, Path(directory), digest, tag, source_commit, team)
    api = '/repos/' + repository

    def request(path):
        value = json.loads(get(API, api + path, JSON_LIMIT), object_pairs_hook=unique_fields)
        if not isinstance(value, dict):
            raise ValueError('Public GitHub catalog returned an unexpected object.')
        return value

    def head():
        info = request('')
        branch, full_name = info.get('default_branch'), info.get('full_name')
        if (info.get('private') is not False or not isinstance(full_name, str)
                or full_name.casefold() != repository.casefold() or not isinstance(branch, str)
                or len(branch) > 200 or not re.fullmatch(BRANCH, branch) or '..' in branch):
            raise ValueError('Public GitHub catalog identity or default branch differs from the approved repository.')
        ref = 'refs/heads/' + branch
        target = request('/git/ref/heads/' + branch)
        obj = target.get('object')
        if (target.get('ref') != ref or not isinstance(obj, dict) or obj.get('type') != 'commit'
                or obj.get('sha') != catalog_commit):
            raise ValueError('Public catalog default branch differs from the selected catalog commit.')
        return branch

    def tree(object_id, recursive=False):
        if not isinstance(object_id, str) or not re.fullmatch(COMMIT, object_id):
            raise ValueError('Public catalog tree does not identify a Git object.')
        result = request('/git/trees/' + object_id + ('?recursive=1' if recursive else ''))
        rows = result.get('tree')
        if result.get('sha') != object_id or result.get('truncated') is not False or not isinstance(rows, list):
            raise ValueError('Public catalog tree is incomplete or identifies a different Git object.')
        entries = {}
        for row in rows:
            path = row.get('path') if isinstance(row, dict) else None
            if not isinstance(path, str) or path in entries:
                raise ValueError('Public catalog tree contains a missing or duplicate path.')
            entries[path] = row
        return entries

    def directory_tree(entries, name):
        row = entries.get(name, {})
        if row.get('type') != 'tree' or row.get('mode') != '040000':
            raise ValueError('Public catalog directory is missing or is not a Git tree.')
        return tree(row.get('sha'))

    branch = head()
    commit = request('/git/commits/' + catalog_commit)
    if commit.get('sha') != catalog_commit or not isinstance(commit.get('tree'), dict):
        raise ValueError('Public catalog commit does not match the selected commit.')
    entries = tree(commit['tree'].get('sha'), recursive=channel == 'nix')
    prefix = ''
    if channel == 'scoop':
        entries, prefix = directory_tree(entries, 'bucket'), 'bucket/'
    elif channel == 'homebrew':
        entries, prefix = directory_tree(entries, 'Casks'), 'Casks/'
        if 'p' in entries:
            grouped = directory_tree(entries, 'p')
            if 'peerbrush.rb' in grouped:
                if 'peerbrush.rb' in entries:
                    raise ValueError('Public tap contains more than one PeerBrush cask path.')
                entries, prefix = grouped, 'Casks/p/'
    else:
        directories = {parent.as_posix() for name in files for parent in PurePosixPath(name).parents
                       if parent.parts}
        if (entries.keys() != files.keys() | directories
                or any(entries[name].get('type') != 'tree' or entries[name].get('mode') != '040000'
                       for name in directories)):
            raise ValueError('Public Nix catalog has a different support tree or flake root from the reviewed definition.')

    total = 0
    for name, checksum in files.items():
        row = entries.get(name, {})
        size, object_id = row.get('size'), row.get('sha')
        if (row.get('type') != 'blob' or row.get('mode') != ('100755' if name in executable else '100644')
                or type(size) is not int or not 0 <= size <= FILE_LIMIT
                or not isinstance(object_id, str) or not re.fullmatch(COMMIT, object_id)):
            raise ValueError('Public catalog definition is missing or has a different Git file mode.')
        total += size
        if total > TOTAL_LIMIT:
            raise ValueError('Public catalog files exceed the verification budget.')
        data = get(RAW, '/' + repository + '/' + catalog_commit + '/' + quote(prefix + name), max(1, size))
        blob = hashlib.sha1(f'blob {size}\0'.encode(), usedforsecurity=False)
        blob.update(data)
        if len(data) != size or blob.hexdigest() != object_id or sha(data) != checksum:
            raise ValueError('Public catalog definition differs from the authenticated bytes or Git blob.')
        if channel == 'nix' and name == 'source-inputs.json':
            source_inputs(definition, files, data)
    downloads = release_downloads(channel, definition, tag, source_commit, get_release)
    if head() != branch:
        raise ValueError('Public catalog default branch changed during readback.')
    report = dict(schema=1, format='PeerBrushPackageCatalogReadbackV1', channel=channel, version=tag[1:],
                source_commit=source_commit, catalog_repository=repository, catalog_branch=branch,
                catalog_commit=catalog_commit, definition_receipt_sha256=digest, verified_files=len(files),
                catalog_publication='exact-bytes-and-modes-verified',
                release_downloads=downloads, release_downloads_readback='exact-bytes-verified')
    if channel == 'homebrew':
        report['publisher_team_id'] = team
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--channel', required=True, choices=['scoop', 'homebrew', 'nix'])
    parser.add_argument('--definition', type=Path, required=True)
    for name in ('definition-sha256', 'tag', 'source-commit', 'repository', 'catalog-commit'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--team', help='Independently approved Apple team ID for Homebrew.')
    args = parser.parse_args()
    report = check(args.channel, args.definition, args.definition_sha256, tag=args.tag, source_commit=args.source_commit,
                   repository=args.repository, catalog_commit=args.catalog_commit, team=args.team)
    print(json.dumps(report, sort_keys=True))


if __name__ == '__main__':
    main()
