"""Protected Store certification and immutable GitHub release transactions.

No ready-report input, replacement/delete API or automatic mutation retry.
Store submission is separate from final installed acceptance and public release.
"""
import hashlib
from contextlib import contextmanager
import http.client
import json
import os
from pathlib import Path
import re
import tempfile
import time
from urllib.parse import quote, urlsplit

from final_release import Candidate, LIMIT, MANIFEST, STORE_FORMAT, checksum_bytes, encode, public_records, read, validate_manifest
from package import regular_file, release_version_order
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
        action_read = (method == 'GET' and payload is None and upload is None and isinstance(route, str)
            and re.fullmatch(re.escape(PREFIX) + r'/actions/(?:workflows/production\.yml/runs\?per_page=100&page=(?:[1-9]|10)|runs/[1-9][0-9]{0,19}/(?:attempts/1/jobs|artifacts)\?per_page=100&page=1|artifacts/[1-9][0-9]{0,19})', route))
        if (method not in {'GET', 'POST', 'PATCH'} or type(route) is not str or not route.startswith(PREFIX + '/')
                or not route.isascii() or any(ord(c) < 33 for c in route)
                or (not action_read and not re.fullmatch(re.escape(PREFIX) + r'/(?:releases(?:\?per_page=100&page=[1-9][0-9]?)?|releases/tags/v[0-9.]+|releases/[1-9][0-9]{0,19}(?:/assets(?:\?name=[A-Za-z0-9._-]+)?)?|releases/assets/[1-9][0-9]{0,19}|commits/(?:main|v[0-9.]+))', route))):
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

    def receipt_bytes(self, identifier, size):
        from store_handoff_history import MAX_ZIP
        return self._artifact_bytes(identifier, size, MAX_ZIP)

    def package_bytes(self, identifier, size):
        from store_handoff_history import MAX_PACKAGE_ZIP
        return self._artifact_bytes(identifier, size, MAX_PACKAGE_ZIP)

    def _artifact_bytes(self, identifier, size, limit):
        import acquire_macos_qualification_inputs as transport
        if type(identifier) is not int or not 0 < identifier < 10 ** 20 or type(size) is not int or not 22 <= size <= limit:
            raise ValueError('Store history requires an exact bounded artifact ID and size.')
        location = transport.request('api.github.com', transport.API + 'artifacts/' + str(identifier) + '/zip',
                                     8192, token=self._token, redirect=True)
        parsed = urlsplit(location)
        target = parsed.path + ('?' + parsed.query if parsed.query else '')
        if size <= 1024 * 1024:
            return transport.request(parsed.hostname, target, size)
        with tempfile.TemporaryDirectory(prefix='peerbrush-store-history-') as temporary:
            path = Path(temporary) / 'package-artifact.zip'
            transport.request(parsed.hostname, target, size, destination=path)
            return read(path, size)

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
    public = public_records(manifest)
    return {name: record for name, record in records.items() if name in public or name in (MANIFEST, 'SHA256SUMS')}


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
    expected = public_records(manifest)
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


def _transfer(client, directory, manifest, notes, readback=None, after_publish=None):
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
        published = '.'.join(part or '0' for part in match.groups())
        if release_version_order(published) > release_version_order(version):
            raise ValueError('A later stable version is already published; no downgrade is permitted.')
    release = client.release(tag)
    if release is None:
        release = client.create(tag, commit, name, notes)
    release_identity(release, tag, commit, name, notes)
    uploaded = remote_files(client, release, records, complete=not release['draft'])
    published_here = False
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
        published_here = True
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
    if after_publish is not None:
        # A response-lost PATCH never reaches here. Already-public releases may
        # only reconcile: neither reruns nor lost/expired receipts recreate a
        # Store mutation permit. The callback never crosses a process boundary.
        if published_here:
            result['store_handoff'] = after_publish()
        else:
            result['store_handoff'] = dict(submission_attempted=False,
                reason='existing-publication-read-only-reconciliation-required', public_availability='unverified')
    return result


def _prepare_store_handoff(candidate):
    """Read-only credential/product/pending/version preflight before GitHub writes."""
    import submit_store_release as store
    candidate.verify()
    manifest = candidate.manifest
    records = validate_manifest(manifest)
    package = manifest['channels']['microsoft-store']['file']
    report = json.loads(read(candidate.directory / (package + '.json'), 64 * 1024,
        records[package + '.json']['sha256']), object_pairs_hook=unique_fields)
    data = read(candidate.directory / package, LIMIT, records[package]['sha256'])
    provider = _StoreWrites(store.Store())
    store.submit(provider, package, data, report, execute=False)
    return provider


@contextmanager
def _store_journal(candidate, environment):
    """Record no-attempt state before submission; a crash without upload still holds."""
    from store_release_state import locked
    from package_macos_dmg import private_inspection_directory
    candidate.verify()
    manifest = candidate.manifest
    directory = private_inspection_directory(Path(environment['RUNNER_TEMP']) / 'peerbrush-store-handoff', candidate.root, fresh=True)
    directory.mkdir(mode=0o700)
    records = validate_manifest(manifest)
    package = manifest['channels']['microsoft-store']['file']
    report = json.loads(read(candidate.directory / (package + '.json'), 64 * 1024,
        records[package + '.json']['sha256']), object_pairs_hook=unique_fields)
    if (report['commit'] != manifest['commit'] or report['package'] != package
            or report['sha256'] != records[package]['sha256']):
        raise ValueError('Store receipt differs from exact candidate source and package.')
    with locked(directory / 'submission.json') as journal:
        journal.bind('v' + manifest['version'], package, report)
        yield journal


def _store_handoff(candidate, provider, journal):
    """One fresh protected invocation, one submission attempt; never replay receipts.

    Local write-ahead records aid diagnosis. Artifact retention is not write
    authority: a new process cannot resume this callback from a record. A lost
    response or competing draft needs service reconciliation, not a retry.
    """
    import submit_store_release as store
    candidate.verify()
    manifest = candidate.manifest
    if manifest['format'] != STORE_FORMAT:
        raise ValueError('Automatic Store handoff requires the exact Store-only final composition.')
    records = validate_manifest(manifest)
    package = manifest['channels']['microsoft-store']['file']
    portable = next(r['file'] for r in manifest['base_inventory']['assets'] if r.get('platform') == 'Windows')
    # The Windows ZIP and submission input are never GitHub release downloads.
    data = read(candidate.directory / package, LIMIT, records[package]['sha256'])
    report = json.loads(read(candidate.directory / (package + '.json'), 64 * 1024,
        records[package + '.json']['sha256']), object_pairs_hook=unique_fields)
    checksum = read(candidate.directory / (package + '.sha256'), 1024, records[package + '.sha256']['sha256'])
    tag = 'v' + manifest['version']
    version, store_version = store.release_version(tag)
    store.validate_package(package, data, report, checksum.decode('ascii'), manifest['commit'], version, store_version,
        source_root=candidate.root, portable_path=regular_file(candidate.directory, portable),
        portable_sha256=records[portable]['sha256'], msix_sha256=records[package]['sha256'])
    candidate.verify()
    journal.bind(tag, package, report)
    store.submit(provider, package, data, report, execute=True, journal=journal)
    return dict(submission_attempted=True, service_status=journal.state['service_status'],
                public_availability='unverified')


class _StoreWrites:
    """Re-observe exact product and pending owner immediately before every write."""
    def __init__(self, provider):
        self.provider, self.submission_id, self.baseline = provider, None, None
        self.phase, self.draft = 'new', None
        self.history = None

    @staticmethod
    def snapshot(value):
        import copy
        return {key: copy.deepcopy(item) for key, item in value.items() if key != 'fileUploadUrl'}

    def check(self):
        import submit_store_release as store
        if self.history is not None:
            self.history.check(pending_id=self.submission_id)
        app = store.application(self.provider)
        if app.get('lastPublishedApplicationSubmission') != self.baseline:
            raise store.SafeError('Store product or published baseline changed before mutation; reconcile without retry.')
        pending = app.get('pendingApplicationSubmission')
        if self.submission_id is None:
            if pending:
                raise store.SafeError('Store creation conflicts with existing state; no mutation retried.')
        elif not isinstance(pending, dict) or str(pending.get('id')) != self.submission_id:
            raise store.SafeError('Store pending submission changed; no mutation retried.')
        elif self.snapshot(self.provider.call('/submissions/' + self.submission_id)) != self.draft:
            raise store.SafeError('Store draft contents changed concurrently; no mutation retried.')

    def before_upload(self, sid):
        import submit_store_release as store
        if self.submission_id != sid or self.phase != 'updated':
            raise store.SafeError('Store upload owner differs; no mutation retried.')
        self.check()
        self.phase = 'upload-requested'

    def after_upload(self, sid):
        import submit_store_release as store
        if self.submission_id != sid or self.phase != 'upload-requested':
            raise store.SafeError('Store upload completion differs; no mutation retried.')
        self.phase = 'uploaded'

    def call(self, path='', method='GET', data=None):
        import submit_store_release as store
        if method != 'GET':
            self.check()
            if self.submission_id is None:
                if path != '/submissions' or method != 'POST' or self.phase != 'new':
                    raise store.SafeError('Store creation conflicts with existing state; no mutation retried.')
                self.phase = 'create-requested'
            elif path == '/submissions/' + self.submission_id and method == 'PUT' and self.phase == 'created':
                self.phase = 'update-requested'
            elif path == '/submissions/' + self.submission_id + '/commit' and method == 'POST' and self.phase == 'uploaded':
                self.phase = 'commit-requested'
            else:
                raise store.SafeError('Store mutation was already attempted or is out of order; no retry.')
        result = self.provider.call(path, method=method, data=data)
        if path == '' and method == 'GET' and self.baseline is None:
            import copy
            self.baseline = copy.deepcopy(result.get('lastPublishedApplicationSubmission'))
        if path == '/submissions' and method == 'POST':
            sid = str(result.get('id', ''))
            if not re.fullmatch('[1-9][0-9]{0,31}', sid):
                raise store.SafeError('Store creation result is ambiguous; no mutation retried.')
            self.submission_id = sid
            self.draft, self.phase = self.snapshot(result), 'created'
        elif method == 'PUT':
            self.draft, self.phase = self.snapshot(data), 'updated'
        elif method == 'POST':
            self.phase = 'committed'
        return result


def store_submission_blockers(candidate):
    if not isinstance(candidate, Candidate):
        raise ValueError('Store submission requires actual final composition, not a JSON report.')
    candidate.verify()
    assessment = candidate.assessment
    blockers = []
    if (candidate.manifest['format'] != STORE_FORMAT or
            assessment.get('store_package', {}).get('native_producer_authentication') !=
            'verified-pinned-workflow-oidc-attestation-and-successful-job'):
        blockers.append('store-native-sdk-producer-not-authenticated')
    if assessment.get('installed_candidates', {}).get('macOS', {}).get('installed_acceptance') != 'verified-signed-two-version-sparkle-upgrade':
        blockers.append('macos-signed-two-version-upgrade-not-established')
    if assessment.get('linux_installed_gui_provenance') != 'verified-pinned-workflow-oidc-attestation-and-successful-job':
        blockers.append('linux-installed-gui-and-upgrade-not-authenticated')
    return blockers


def protected_invocation(candidate, environment):
    env = os.environ if environment is None else environment
    commit = candidate.manifest['commit']
    required = {'GITHUB_REPOSITORY': REPOSITORY, 'GITHUB_REF': 'refs/heads/main',
                'GITHUB_SHA': commit, 'GITHUB_WORKFLOW_SHA': commit, 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                'GITHUB_WORKFLOW_REF': REPOSITORY + '/.github/workflows/production.yml@refs/heads/main',
                'PEERBRUSH_PRODUCTION_APPROVED': 'true', 'GITHUB_RUN_ATTEMPT': '1'}
    if any(env.get(key) != value for key, value in required.items()):
        raise ValueError('Publication requires the exact reviewed main workflow and protected explicit approval.')
    if candidate.manifest['format'] != STORE_FORMAT or env.get('PEERBRUSH_STORE_HANDOFF_ENABLED') != 'true':
        raise ValueError('Store-only publication requires its explicitly approved Store handoff.')
    return env


def submit_store(candidate, client, *, execute=False, environment=None):
    blockers = store_submission_blockers(candidate)
    if blockers:
        return dict(phase='blocked', production_ready=False, publication_performed=False, blockers=blockers)
    if not execute:
        return dict(phase='prepared', production_ready=False, publication_performed=False)
    env = protected_invocation(candidate, environment)
    tag = 'v' + candidate.manifest['version']
    head = client.commit('main')
    if (not isinstance(head, dict) or head.get('sha') != candidate.manifest['commit']
            or client.release(tag) is not None or client.commit(tag) is not None):
        raise ValueError('Store certification requires unchanged main and a new, unpublished release version.')
    from store_handoff_history import History
    with _store_journal(candidate, env) as journal:
        provider = _prepare_store_handoff(candidate)
        provider.history = History(client, provider.provider, env)
        provider.history.check()
        _store_handoff(candidate, provider, journal)
        return dict(phase='store-submitted', production_ready=False, publication_performed=False,
                    service_status=journal.state['service_status'])


def publish(candidate, client, notes, *, execute=False, environment=None):
    if not isinstance(candidate, Candidate):
        raise ValueError('A JSON report cannot authorize publication; compose actual final inputs.')
    candidate.verify()
    if candidate.blockers or candidate.assessment.get('production_ready') is not True:
        return dict(phase='blocked', production_ready=False, publication_performed=False,
                    blockers=list(candidate.blockers), store=candidate.manifest['channels']['microsoft-store'])
    if not execute:
        return dict(phase='prepared', production_ready=False, publication_performed=False)
    env = protected_invocation(candidate, environment)
    from store_handoff_history import History
    with _store_journal(candidate, env) as journal:
        provider = _prepare_store_handoff(candidate)
        provider.history = History(client, provider.provider, env)
        provider.history.check()
        handoff = lambda: _store_handoff(candidate, provider, journal)
        return _transfer(client, candidate.directory, candidate.manifest, notes, after_publish=handoff)


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
    parser.add_argument('--submit-store', action='store_true', help='Submit the qualified package for certification without publishing GitHub assets.')
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
        allowed = not store_submission_blockers(candidate) if args.submit_store else (
            not candidate.blockers and candidate.assessment.get('production_ready') is True)
        client = GitHub(os.environ.get('GH_TOKEN')) if args.execute and allowed else None
        result = (submit_store(candidate, client, execute=args.execute) if args.submit_store else
                  publish(candidate, client, notes, execute=args.execute))
    except Exception:
        parser.exit(1, 'Production transaction stopped. Preserve private work and reconcile remote state before retry; no mutation was automatically retried.\n')
    print(json.dumps(result, sort_keys=True))
    return 2 if result.get('phase') in {'blocked', 'prepared'} else 0


if __name__ == '__main__':
    raise SystemExit(main())
