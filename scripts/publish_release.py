"""Protected immutable GitHub release transaction; current native gates keep it disabled.

No ready-report input, replacement/delete API, Store write or automatic mutation retry.
"""
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import time
from urllib.parse import quote, urlsplit

from final_release import Candidate, LIMIT, MANIFEST, checksum_bytes, encode, read, validate_manifest
from package import regular_file
from package_macos_dmg import bounded_file
from production_readiness import unique_fields

REPOSITORY = 'Deftr0y/PeerBrush'
PREFIX = '/repos/' + REPOSITORY
API_LIMIT = 2 * 1024 * 1024


class AmbiguousMutation(ValueError):
    pass


class _Upload:
    def __init__(self, path):
        self.file = path.open('rb')
        self.expected, self.total = os.fstat(self.file.fileno()).st_size, 0
        if not 0 < self.expected <= LIMIT:
            self.file.close()
            raise ValueError('Release upload input exceeds its byte budget.')
        self.deadline = time.monotonic() + 120

    def read(self, size):
        if time.monotonic() >= self.deadline:
            raise ValueError('Release upload exceeded its deadline.')
        data = self.file.read(min(size, self.expected + 1 - self.total))
        self.total += len(data)
        if self.total > self.expected or (not data and self.total != self.expected):
            raise ValueError('Release upload input changed or was truncated.')
        return data

    def close(self):
        self.file.close()


class GitHub:
    """One fixed official repository; tokens never follow a download redirect."""
    def __init__(self, token):
        if type(token) is not str or not 1 <= len(token) <= 8192 or any(not 33 <= ord(c) <= 126 for c in token):
            raise ValueError('Protected release token unavailable.')
        self._token = token

    def request(self, method, route, payload=None, *, upload=None):
        if (method not in {'GET', 'POST', 'PATCH'} or type(route) is not str or not route.startswith(PREFIX + '/')
                or not route.isascii() or any(ord(c) < 33 for c in route)
                or not re.fullmatch(re.escape(PREFIX) + r'/(?:releases(?:\?per_page=100&page=[1-9][0-9]?)?|releases/tags/v[0-9.]+|releases/[1-9][0-9]{0,19}(?:/assets(?:\?name=[A-Za-z0-9._-]+)?)?|releases/assets/[1-9][0-9]{0,19}|commits/(?:main|v[0-9.]+))', route)):
            raise ValueError('Unsupported protected release route.')
        host = 'uploads.github.com' if upload is not None else 'api.github.com'
        headers = {'User-Agent': 'PeerBrush-immutable-release', 'Authorization': 'Bearer ' + self._token,
                   'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28',
                   'Cache-Control': 'no-cache'}
        body = encode(payload) if payload is not None else None
        if upload is not None:
            if method != 'POST' or payload is not None or not route.endswith('/assets?name=' + quote(upload.name, safe='')):
                raise ValueError('Release uploads require an exact selected filename.')
            body = _Upload(upload)
            headers.update({'Content-Type': 'application/octet-stream', 'Content-Length': str(body.expected)})
        elif body is not None:
            headers['Content-Type'] = 'application/json'
        connection = None
        try:
            connection = http.client.HTTPSConnection(host, timeout=15)
            connection.request(method, route, body=body, headers=headers)
            with connection.getresponse() as response:
                if response.status == 404 and method == 'GET':
                    return None
                if not 200 <= response.status < 300:
                    raise ValueError('Release service did not confirm the selected operation.')
                lengths = response.getheaders()
                if (response.getheader('Content-Encoding') not in [None, 'identity']
                        or len([v for k, v in lengths if k.lower() == 'content-length']) > 1):
                    raise ValueError('Release response has ambiguous encoding/length.')
                data, deadline = bytearray(), time.monotonic() + 60
                while True:
                    if time.monotonic() >= deadline:
                        raise ValueError('Release response exceeded its deadline.')
                    chunk = response.read1(min(64 * 1024, API_LIMIT + 1 - len(data)))
                    if time.monotonic() >= deadline:
                        raise ValueError('Release response exceeded its deadline.')
                    if not chunk:
                        break
                    data.extend(chunk)
                    if len(data) > API_LIMIT:
                        raise ValueError('Release response exceeded its byte budget.')
                length = response.getheader('Content-Length')
                if length is not None and (not length.isdecimal() or int(length) != len(data)):
                    raise ValueError('Release response was truncated.')
                return json.loads(data, object_pairs_hook=unique_fields)
        except Exception:
            if method != 'GET':
                raise AmbiguousMutation('Release mutation outcome uncertain; reconcile the draft before any retry.') from None
            raise ValueError('Release read failed; no mutation was retried.') from None
        finally:
            if upload is not None:
                body.close()
            if connection is not None:
                connection.close()

    def release(self, tag):
        return self.request('GET', PREFIX + '/releases/tags/' + tag)

    def commit(self, ref):
        return self.request('GET', PREFIX + '/commits/' + ref)

    def history(self):
        result = []
        for page in range(1, 11):
            values = self.request('GET', PREFIX + '/releases?per_page=100&page=' + str(page))
            if not isinstance(values, list) or len(values) > 100:
                raise ValueError('Release history response is unsupported.')
            result.extend(values)
            if len(values) < 100:
                return result
        raise ValueError('Release history exceeds the bounded reconciliation window.')

    def create(self, tag, commit, name, notes):
        return self.request('POST', PREFIX + '/releases', dict(tag_name=tag, target_commitish=commit,
            name=name, body=notes, draft=True, prerelease=False, generate_release_notes=False))

    def upload(self, identifier, path):
        return self.request('POST', PREFIX + '/releases/' + str(identifier) + '/assets?name=' + quote(path.name, safe=''), upload=path)

    def publish(self, identifier):
        return self.request('PATCH', PREFIX + '/releases/' + str(identifier), dict(draft=False, make_latest='true'))

    def asset_hash(self, asset):
        # GitHub's digest is an additional check. Read actual selected bytes on
        # resume too; size/name or a self-authored sidecar cannot establish them.
        identifier = asset['id']
        host, route, token = 'api.github.com', PREFIX + '/releases/assets/' + str(identifier), self._token
        checksum, total = hashlib.sha256(), 0
        from prepare_site_release import public_url
        for redirect in range(2):
            headers = {'User-Agent': 'PeerBrush-immutable-release', 'Accept': 'application/octet-stream',
                       'Cache-Control': 'no-cache'}
            if token is not None:
                headers.update(Authorization='Bearer ' + token, **{'X-GitHub-Api-Version': '2022-11-28'})
            connection = http.client.HTTPSConnection(host, timeout=15)
            try:
                connection.request('GET', route, headers=headers)
                with connection.getresponse() as response:
                    if response.status == 302 and redirect == 0:
                        location = response.getheader('Location')
                        parsed = urlsplit(location) if isinstance(location, str) else None
                        if (parsed is None or len(location) > 8192 or not location.isascii()
                                or not public_url(location, redirected=True)):
                            raise ValueError('Release asset redirect is outside the approved public storage hosts.')
                        host, route, token = parsed.netloc, parsed.path + ('?' + parsed.query if parsed.query else ''), None
                        continue
                    if response.status != 200 or response.getheader('Content-Encoding') not in [None, 'identity']:
                        raise ValueError('Selected immutable release bytes unavailable.')
                    lengths = [v for k, v in response.getheaders() if k.lower() == 'content-length']
                    if len(lengths) > 1 or (lengths and (not lengths[0].isdecimal() or int(lengths[0]) != asset['size'])):
                        raise ValueError('Selected release asset has an unexpected byte count.')
                    deadline = time.monotonic() + 120
                    while True:
                        if time.monotonic() >= deadline:
                            raise ValueError('Release asset readback exceeded its deadline.')
                        chunk = response.read1(min(64 * 1024, asset['size'] + 1 - total))
                        if time.monotonic() >= deadline:
                            raise ValueError('Release asset readback exceeded its deadline.')
                        if not chunk:
                            break
                        total += len(chunk)
                        if total > asset['size']:
                            raise ValueError('Release asset readback exceeds its selected bytes.')
                        checksum.update(chunk)
                    if total != asset['size']:
                        raise ValueError('Release asset readback was truncated.')
                    return checksum.hexdigest()
            except (OSError, http.client.HTTPException):
                raise ValueError('Release asset readback failed; no mutation was retried.') from None
            finally:
                connection.close()
        raise ValueError('Release asset redirect did not reach bounded selected bytes.')


def local_files(directory, manifest):
    records = dict(validate_manifest(manifest))
    for name, data in [(MANIFEST, encode(manifest)), ('SHA256SUMS', checksum_bytes(manifest))]:
        records[name] = dict(file=name, bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
    if {path.name for path in directory.iterdir()} != set(records):
        raise ValueError('Publication inputs contain missing or unexpected files.')
    for name, record in records.items():
        if bounded_file(regular_file(directory, name), LIMIT) != (record['sha256'], record['bytes']):
            raise ValueError('Publication input differs from the immutable final manifest.')
    return records


def release_identity(release, tag, commit, name, notes):
    if (not isinstance(release, dict) or type(release.get('id')) is not int or not 0 < release['id'] < 10 ** 20
            or release.get('tag_name') != tag or release.get('target_commitish') != commit
            or release.get('name') != name or release.get('body') != notes or type(release.get('draft')) is not bool
            or release.get('prerelease') is not False):
        raise ValueError('Existing release identity/source/notes differ; no replacement is allowed.')


def remote_files(client, release, records, *, complete, verify_names=None):
    assets = release.get('assets')
    if not isinstance(assets, list) or len(assets) > 64:
        raise ValueError('Unexpected release asset listing.')
    names, identifiers = set(), set()
    for asset in assets:
        if (not isinstance(asset, dict) or type(asset.get('id')) is not int or not 0 < asset['id'] < 10 ** 20
                or type(asset.get('name')) is not str or asset['name'] not in records or asset['name'] in names
                or asset['id'] in identifiers or asset.get('state') != 'uploaded'
                or type(asset.get('size')) is not int or asset['size'] != records[asset['name']]['bytes']
                or asset.get('digest') not in [None, 'sha256:' + records[asset['name']]['sha256']]):
            raise ValueError('Release contains unexpected, duplicate, incomplete or different immutable files.')
        names.add(asset['name'])
        identifiers.add(asset['id'])
        if (verify_names is None or asset['name'] in verify_names) and client.asset_hash(asset) != records[asset['name']]['sha256']:
            raise ValueError('Actual uploaded release bytes differ; replacement is prohibited.')
    if complete and names != set(records):
        raise ValueError('Published or completed draft release has missing immutable files.')
    return names


def public_readback(manifest, get=None, notes=None):
    """Only actual public release bytes establish availability; never native readiness."""
    from prepare_site_release import public_get, API, DOWNLOAD, RELEASE
    import tempfile
    get = get or public_get
    tag = 'v' + manifest['version']
    release = json.loads(get(API + 'releases/tags/' + tag, API_LIMIT, api=True), object_pairs_hook=unique_fields)
    resolved = json.loads(get(API + 'commits/' + tag, API_LIMIT, api=True), object_pairs_hook=unique_fields)
    if (not isinstance(release, dict) or release.get('draft') is not False or release.get('prerelease') is not False
            or release.get('tag_name') != tag or release.get('html_url') != RELEASE + tag
            or not isinstance(resolved, dict) or resolved.get('sha') != manifest['commit']
            or (notes is not None and release.get('body') != notes)):
        raise ValueError('Stable public release/tag is not verified.')
    expected = dict(validate_manifest(manifest))
    for name, data in [(MANIFEST, encode(manifest)), ('SHA256SUMS', checksum_bytes(manifest))]:
        expected[name] = dict(file=name, bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
    assets = release.get('assets')
    if (not isinstance(assets, list) or len(assets) != len(expected)
            or any(not isinstance(a, dict) or type(a.get('name')) is not str for a in assets)
            or len({a['name'] for a in assets}) != len(assets)):
        raise ValueError('Public release asset set differs from the final selection.')
    assets = {a['name']: a for a in assets}
    if set(assets) != set(expected):
        raise ValueError('Public release has missing or unexpected final assets.')
    # Canonicalize the OS temp root before creating owned scratch on macOS.
    parent = Path(tempfile.gettempdir()).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='peerbrush-release-readback-', dir=parent) as temporary:
        for name, record in expected.items():
            asset = assets[name]
            url = DOWNLOAD + tag + '/' + quote(name, safe='')
            if (asset.get('browser_download_url') != url or asset.get('state') != 'uploaded'
                    or type(asset.get('size')) is not int or asset['size'] != record['bytes']
                    or asset.get('digest') not in [None, 'sha256:' + record['sha256']]):
                raise ValueError('Public release metadata differs from the exact final bytes.')
            path = Path(temporary) / name
            get(url, record['bytes'], destination=path)
            if bounded_file(path, LIMIT) != (record['sha256'], record['bytes']):
                raise ValueError('Actual public download differs from the immutable final bytes.')
    return dict(public_availability='verified', native_installed_acceptance='not-established-by-download',
                production_ready=False, store=manifest['channels']['microsoft-store'])


def _transfer(client, directory, manifest, notes, readback=None):
    """Mechanical transaction; only publish() may supply production authority.

    Offline tests exercise this layer with a service double, never credentials.
    A failed/uncertain write is never automatically retried or deleted.
    """
    records = local_files(directory, manifest)
    if readback is None:
        readback = lambda value: public_readback(value, notes=notes)
    version, commit = manifest['version'], manifest['commit']
    tag, name = 'v' + version, 'PeerBrush ' + version
    if type(notes) is not str or not 0 < len(notes.encode('utf-8')) <= 128 * 1024:
        raise ValueError('Reviewed bounded public release notes are required.')
    main = client.commit('main')
    resolved = client.commit(tag)
    if (not isinstance(main, dict) or main.get('sha') != commit
            or (resolved is not None and (not isinstance(resolved, dict) or resolved.get('sha') != commit))):
        raise ValueError('Protected main or existing release tag differs from the approved source.')
    for old in client.history():
        if not isinstance(old, dict) or type(old.get('draft')) is not bool or type(old.get('prerelease')) is not bool:
            raise ValueError('Release history cannot establish version ordering.')
        if old['draft'] or old['prerelease']:
            continue
        match = re.fullmatch(r'v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:\.(0|[1-9][0-9]*))?', str(old.get('tag_name')))
        if match is None:
            raise ValueError('Unrecognized stable release history requires explicit reconciliation.')
        if tuple(int(part or 0) for part in match.groups()) > tuple(map(int, version.split('.'))):
            raise ValueError('A later stable version is already published; no downgrade is permitted.')
    release = client.release(tag)
    if release is None:
        release = client.create(tag, commit, name, notes)
    release_identity(release, tag, commit, name, notes)
    uploaded = remote_files(client, release, records, complete=not release['draft'])
    if release['draft']:
        for filename in sorted(set(records) - uploaded):
            path = regular_file(directory, filename)
            if bounded_file(path, LIMIT) != (records[filename]['sha256'], records[filename]['bytes']):
                raise ValueError('Selected upload changed before its request.')
            client.upload(release['id'], path)
            # A missing response/failed readback stops; the next invocation
            # reconciles actual remote bytes before deciding what remains.
            release = client.release(tag)
            release_identity(release, tag, commit, name, notes)
            if not release['draft']:
                raise ValueError('Draft changed during upload; no publication was requested.')
            remote_files(client, release, records, complete=False, verify_names={filename})
        local_files(directory, manifest)
        remote_files(client, release, records, complete=True)
        main, resolved = client.commit('main'), client.commit(tag)
        if (not isinstance(main, dict) or main.get('sha') != commit
                or (resolved is not None and (not isinstance(resolved, dict) or resolved.get('sha') != commit))):
            raise ValueError('Main or selected tag changed before publication; preserve the immutable draft.')
        client.publish(release['id'])
    # Public visibility can lag a successful PATCH. Never turn that into another
    # create/publish request or ready website row: a later run reads back only.
    release = client.release(tag)
    release_identity(release, tag, commit, name, notes)
    if release['draft']:
        raise ValueError('Published state is not confirmed; reconcile without retrying the mutation.')
    remote_files(client, release, records, complete=True)
    result = readback(manifest)
    if not isinstance(result, dict) or result.get('public_availability') != 'verified':
        raise ValueError('Publication remains unverified until actual public readback succeeds.')
    return result


def publish(candidate, client, notes, *, execute=False, environment=None):
    if not isinstance(candidate, Candidate):
        raise ValueError('A JSON report cannot authorize publication; compose actual final inputs.')
    candidate.verify()
    if candidate.blockers or candidate.assessment.get('production_ready') is not True:
        return dict(phase='blocked', production_ready=False, publication_performed=False,
                    blockers=list(candidate.blockers), store=candidate.manifest['channels']['microsoft-store'])
    if not execute:
        return dict(phase='prepared', production_ready=False, publication_performed=False)
    env = os.environ if environment is None else environment
    commit = candidate.manifest['commit']
    required = {'GITHUB_REPOSITORY': REPOSITORY, 'GITHUB_REF': 'refs/heads/main',
                'GITHUB_SHA': commit, 'GITHUB_WORKFLOW_SHA': commit, 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                'GITHUB_WORKFLOW_REF': REPOSITORY + '/.github/workflows/production.yml@refs/heads/main',
                'PEERBRUSH_PRODUCTION_APPROVED': 'true'}
    if any(env.get(key) != value for key, value in required.items()):
        raise ValueError('Publication requires the exact reviewed main workflow and protected explicit approval.')
    return _transfer(client, candidate.directory, candidate.manifest, notes)


def main(argv=None):
    import argparse
    from final_release import from_selection
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'directory', 'output', 'notes']:
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ['selection-sha256', 'notes-sha256']:
        parser.add_argument('--' + name, required=True)
    selected = parser.add_mutually_exclusive_group(required=True)
    selected.add_argument('--selection', type=Path)
    selected.add_argument('--final-input-index', type=Path)
    parser.add_argument('--final-input-index-sha256')
    parser.add_argument('--work', type=Path)
    parser.add_argument('--execute', action='store_true', help='Still requires actual native authority and protected explicit approval; current source remains blocked.')
    args = parser.parse_args(argv)
    if args.final_input_index is not None:
        if args.final_input_index_sha256 is None or args.work is None:
            parser.error('Final-input mode requires an independently selected index digest and private work directory.')
    elif args.final_input_index_sha256 is not None or args.work is not None:
        parser.error('Final-input digest and work options require --final-input-index.')
    try:
        notes = read(args.notes, 128 * 1024, args.notes_sha256).decode('utf-8')
        if args.final_input_index is not None:
            from prepare_final_inputs import MAX_INDEX, consume, json_input
            index = json_input(args.final_input_index, MAX_INDEX, args.final_input_index_sha256)
            candidate = consume(args.source_root, args.directory, args.output, args.work, index, args.selection_sha256)
        else:
            candidate = from_selection(args.source_root, args.directory, args.output, args.selection, args.selection_sha256)
        # Missing native producers must stop before even requesting a write token.
        client = GitHub(os.environ.get('GH_TOKEN')) if (args.execute and not candidate.blockers
            and candidate.assessment.get('production_ready') is True) else None
        result = publish(candidate, client, notes, execute=args.execute)
    except Exception:
        parser.exit(1, 'Production transaction stopped. Preserve private work and reconcile remote state before retry; no mutation was automatically retried.\n')
    print(json.dumps(result, sort_keys=True))
    return 2 if result.get('phase') in {'blocked', 'prepared'} else 0


if __name__ == '__main__':
    raise SystemExit(main())
