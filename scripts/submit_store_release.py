"""Validate a published release and submit only its MSIX to Microsoft Store.

Opt-in deployment infrastructure; no credentials or API response bodies are logged.
No destructive draft replacement, automatic retry, or listing/pricing changes.
"""
import argparse
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

REPOSITORY = 'Deftr0y/PeerBrush'
STORE_ID = '9PNR6T1Q2ZL9'
IDENTITY = 'Deftroy.PeerBrush'
PUBLISHER = 'CN=438C1F79-CBE8-4F97-8C50-F9AC34C7D2AC'
FAMILY = 'Deftroy.PeerBrush_wszhgmjq5tjta'
BASE = 'https://manage.devcenter.microsoft.com/v1.0/my/applications/' + STORE_ID
BASELINE = (1, 25, 0, 0)


class SafeError(ValueError):
    """Fixed, credential-free diagnostic explicitly safe for public CI logs."""


class RemoteError(SafeError):
    def __init__(self, status):
        self.status = status if type(status) is int and 100 <= status <= 599 else None
        super().__init__(f'Remote request failed (HTTP {self.status or "network error"}); outcome may be uncertain. Inspect the service before retrying.')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def version(value):
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.0', value):
        raise SafeError('Expected a four-component Store version with final component zero.')
    result = tuple(map(int, value.split('.')))
    if result[0] < 1 or any(v > 65535 for v in result):
        raise SafeError('Store version component out of range.')
    return result


def release_version(tag):
    match = re.fullmatch(r'v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:\.(0|[1-9][0-9]*))?', tag)
    if not match:
        raise SafeError('Only stable numeric release tags are supported.')
    major, minor, patch = (int(v or 0) for v in match.groups())
    app = f'{major}.{minor}.{patch}'
    store = f'{major + 1}.{minor}.{patch}.0'
    if version(store) <= BASELINE:
        raise SafeError('The initial 0.25 release and older versions cannot be submitted.')
    return app, store


def request(url, *, method='GET', data=None, headers=None, binary=False, allow_redirects=False):
    req = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    try:
        opener = urllib.request.build_opener() if allow_redirects else urllib.request.build_opener(NoRedirect())
        with opener.open(req, timeout=120) as response:
            body = response.read()
    except (urllib.error.URLError, TimeoutError) as error:
        # Error URLs can contain upload SAS credentials. Never print the exception.
        code = getattr(error, 'code', None)
        raise RemoteError(code) from None
    if binary:
        return body
    return json.loads(body) if body else {}


def github(path):
    headers = {'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28'}
    if os.environ.get('GH_TOKEN'):
        headers['Authorization'] = 'Bearer ' + os.environ['GH_TOKEN']
    return request('https://api.github.com/repos/' + REPOSITORY + '/' + path, headers=headers)


def released_assets(tag):
    app_version, store_version = release_version(tag)
    release = github('releases/tags/' + urllib.parse.quote(tag, safe=''))
    if release.get('draft') or release.get('prerelease') or not release.get('published_at'):
        raise SafeError('Release must be published and stable.')
    if release.get('tag_name') != tag:
        raise SafeError('Release tag mismatch.')
    commit = github('commits/' + urllib.parse.quote(tag, safe=''))['sha']
    if not re.fullmatch('[0-9a-f]{40}', commit):
        raise SafeError('Invalid resolved release commit.')
    assets = release['assets']
    packages = [a for a in assets if a['name'].endswith('.msix')]
    if len(packages) != 1:
        raise SafeError('Attach exactly one reviewed x64 MSIX and its .json/.sha256 reports to this release before submitting.')
    name = packages[0]['name']
    if not re.fullmatch(r'PeerBrush-[0-9.]+-Windows-x64\.msix', name):
        raise SafeError('Unexpected MSIX asset name.')
    results = []
    for filename in (name, name + '.json', name + '.sha256'):
        matches = [a for a in assets if a['name'] == filename]
        if len(matches) != 1:
            raise SafeError('Release package reports are missing or ambiguous.')
        url = matches[0]['browser_download_url']
        expected = f'https://github.com/{REPOSITORY}/releases/download/{urllib.parse.quote(tag, safe="")}/'
        if not url.startswith(expected) or urllib.parse.unquote(url.rsplit('/', 1)[1]) != filename:
            raise SafeError('Unexpected release asset origin.')
        results.append(request(url, binary=True, allow_redirects=True))
    package, report_data, checksum = results
    report = json.loads(report_data)
    validate_package(name, package, report, checksum.decode(), commit, app_version, store_version)
    return name, package, report


def validate_package(name, package, report, checksum, commit, app_version, store_version):
    digest = hashlib.sha256(package).hexdigest()
    if checksum.strip() != f'{digest}  {name}':
        raise SafeError('Release checksum mismatch.')
    expected = {'schema': 1, 'package': name, 'sha256': digest, 'commit': commit,
                'app_version': app_version, 'version': store_version,
                'package_family_name': FAMILY, 'development_identity': False}
    if any(report.get(k) != value for k, value in expected.items()):
        raise SafeError('MSIX provenance report mismatch.')
    identity = report.get('identity', {})
    if identity.get('name') != IDENTITY or identity.get('publisher') != PUBLISHER or identity.get('publisher_display_name') != 'Deftroy':
        raise SafeError('MSIX report identity mismatch.')
    with zipfile.ZipFile(io.BytesIO(package)) as archive:
        if archive.namelist().count('AppxManifest.xml') != 1:
            raise SafeError('Missing or duplicate MSIX manifest.')
        manifest = ET.fromstring(archive.read('AppxManifest.xml'))
    entry = manifest.find('{http://schemas.microsoft.com/appx/manifest/foundation/windows10}Identity')
    if entry is None or entry.attrib != {'Name': IDENTITY, 'Publisher': PUBLISHER,
                                         'Version': store_version, 'ProcessorArchitecture': 'x64'}:
        raise SafeError('MSIX manifest identity/version mismatch.')


class Store:
    def __init__(self):
        required = ('MSSTORE_TENANT_ID', 'MSSTORE_CLIENT_ID', 'MSSTORE_CLIENT_SECRET')
        if any(not os.environ.get(key) for key in required):
            raise SafeError('Configure MSSTORE_TENANT_ID, MSSTORE_CLIENT_ID and MSSTORE_CLIENT_SECRET securely before enabling submissions.')
        tenant = os.environ['MSSTORE_TENANT_ID']
        if not re.fullmatch(r'[0-9a-fA-F-]{36}', tenant):
            raise SafeError('Tenant ID must be a UUID.')
        form = urllib.parse.urlencode({'grant_type': 'client_credentials',
            'client_id': os.environ['MSSTORE_CLIENT_ID'],
            'client_secret': os.environ['MSSTORE_CLIENT_SECRET'],
            'resource': 'https://manage.devcenter.microsoft.com'}).encode()
        token = request(f'https://login.microsoftonline.com/{tenant}/oauth2/token', method='POST', data=form,
                        headers={'Content-Type': 'application/x-www-form-urlencoded'})['access_token']
        self.headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'}

    def call(self, path='', method='GET', data=None):
        return request(BASE + path, method=method, headers=self.headers,
                       data=json.dumps(data).encode() if data is not None else None)


def published_version(submission):
    packages = submission.get('applicationPackages', [])
    if not packages or any(p.get('fileStatus') != 'Uploaded' for p in packages):
        raise SafeError('Published package state is missing or unsupported.')
    if any(p.get('architecture', '').lower() != 'x64' for p in packages):
        raise SafeError('Unexpected existing architecture. Review package replacement policy manually.')
    return max(version(p['version']) for p in packages)


def submit(store, name, package, report, execute=False, journal=None):
    app = store.call()
    if (app.get('id') != STORE_ID or app.get('packageIdentityName') != IDENTITY or
            app.get('publisherName') != PUBLISHER or app.get('packageFamilyName') != FAMILY):
        raise SafeError('Partner Center product identity mismatch.')
    if app.get('pendingApplicationSubmission'):
        raise SafeError('A submission already exists. It was left untouched; inspect Partner Center before retrying.')
    published = app.get('lastPublishedApplicationSubmission', {}).get('id')
    if not published or not re.fullmatch(r'[0-9]+', str(published)):
        raise SafeError('A published baseline submission is required.')
    current = store.call('/submissions/' + str(published))
    if version(report['version']) <= max(BASELINE, published_version(current)):
        raise SafeError('Package version is already live or older; no duplicate or downgrade submitted.')
    if not execute:
        print('Read-only Store preflight passed. No submission created.')
        return
    if journal is None or journal.state is None:
        raise SafeError('Store mutation requires a durable private release receipt.')
    journal.bind(journal.state['tag'], name, report)
    if hashlib.sha256(package).hexdigest() != report['sha256']:
        raise SafeError('Package bytes differ from the private submission receipt.')
    # A create conflict safely stops us if another publisher won the race.
    journal.phase('create-requested')
    draft = store.call('/submissions', method='POST')
    sid = str(draft.get('id', ''))
    if not re.fullmatch(r'[0-9]+', sid):
        raise SafeError('Invalid new submission ID; inspect Partner Center before retrying.')
    journal.phase('created', sid)
    print('Created Store submission ' + sid + '. Any failure leaves this draft for inspection.')
    updated = copy.deepcopy(draft)
    if not updated.get('applicationPackages'):
        raise SafeError('New draft has no inherited packages; left untouched.')
    for old in updated['applicationPackages']:
        # Do not retire packages for other architectures/platforms accidentally.
        if old.get('architecture', '').lower() != 'x64':
            raise SafeError('Unexpected existing architecture. Review package replacement policy manually.')
        old['fileStatus'] = 'PendingDelete'
    updated['applicationPackages'].append({'fileName': name, 'fileStatus': 'PendingUpload',
        'minimumDirectXVersion': 'None', 'minimumSystemRam': 'None'})
    upload = draft.get('fileUploadUrl', '')
    parts = urllib.parse.urlparse(upload)
    if parts.scheme != 'https' or not (parts.hostname or '').endswith('.blob.core.windows.net') or parts.username or parts.port:
        raise SafeError('Unexpected Store upload endpoint; draft left untouched.')
    # Keep all inherited metadata, pricing, certification notes and publish mode.
    store.call('/submissions/' + sid, method='PUT', data=updated)
    zipped = io.BytesIO()
    with zipfile.ZipFile(zipped, 'w', zipfile.ZIP_STORED) as archive:
        archive.writestr(name, package)
    request(upload, method='PUT', data=zipped.getvalue(), headers={'x-ms-blob-type': 'BlockBlob'}, binary=True)
    journal.phase('commit-requested')
    store.call('/submissions/' + sid + '/commit', method='POST')
    journal.phase('committed')
    status = await_commit(store, sid)
    journal.observe(status)


def await_commit(store, sid, timeout=600):
    deadline = time.monotonic() + timeout
    accepted = {'PreProcessing', 'Certification', 'PendingPublication', 'Publishing', 'Published', 'Release'}
    while True:
        status = store.call('/submissions/' + sid + '/status').get('status')
        if status in accepted:
            print('Store submission ' + sid + ' reached ' + status + '. Check Partner Center for certification/publication outcome.')
            return status
        if status not in {'PendingCommit', 'CommitStarted'}:
            raise SafeError('Store commit failed or returned an unsupported status. Inspect Partner Center; no automatic retry.')
        if time.monotonic() >= deadline:
            raise SafeError('Store commit is still pending. Inspect Partner Center before retrying; submission was not removed.')
        time.sleep(15)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--submit', action='store_true', help='Create, upload and commit; default is read-only preflight.')
    parser.add_argument('--state', type=Path, help='Durable private receipt; required for submission, never store it in public source or CI artifacts.')
    args = parser.parse_args()
    try:
        if args.submit and args.state is None:
            raise SafeError('Submission requires --state in durable private storage; hosted CI needs a reviewed persistence adapter before activation.')
        name, package, report = released_assets(args.tag)
        if args.state is None:
            submit(Store(), name, package, report)
        else:
            from store_release_state import locked
            with locked(args.state) as journal:
                journal.bind(args.tag, name, report)
                submit(Store(), name, package, report, execute=args.submit, journal=journal)
    except Exception as error:
        # Known errors contain fixed strings; unexpected exceptions may include secrets.
        message = str(error) if isinstance(error, SafeError) else type(error).__name__
        print('Store automation stopped: ' + message, file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
