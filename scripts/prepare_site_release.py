"""Prepare reviewed website output from public release bytes; never publish/deploy."""
import argparse
import datetime
import hashlib
import html
import http.client
import io
import json
import os
import pathlib
import re
import stat
import tempfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import zipfile

from package import git, relative_path, source_files
from production_readiness import immutable
from package_deb import deb_version, inspect_final
from verify_release import inspect_archive


REPOSITORY = 'Deftr0y/PeerBrush'
API = 'https://api.github.com/repos/' + REPOSITORY + '/'
DOWNLOAD = 'https://github.com/' + REPOSITORY + '/releases/download/'
RELEASE = 'https://github.com/' + REPOSITORY + '/releases/tag/'
WEBSITE = 'https://peerbrush.com/'
SITE_FILES = ['downloads.json', 'index.html', 'main.js', 'platform.js',
              'releases.js', 'release-page.js', 'releases.html']
NUMERIC = r'(?:0|[1-9][0-9]*)'
STABLE = NUMERIC + r'\.' + NUMERIC + r'\.' + NUMERIC
PLATFORMS = {'Windows': ('windows', 'Windows', 'x64'),
             'macOS': ('macos', 'macOS', 'arm64'), 'Linux': ('linux', 'Linux', 'x64')}


def encode(value):
    return (json.dumps(value, indent=2, ensure_ascii=False) + '\n').encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def file_sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def inventory_sha(inventory):
    return sha(json.dumps(inventory, sort_keys=True, separators=(',', ':')).encode())


class Unavailable(ValueError):
    """Public bytes were not available for verification; contains no remote URL."""


def public_url(url, *, api=False, redirected=False, website=False):
    try:
        parsed = urllib.parse.urlsplit(url)
        if (parsed.scheme != 'https' or parsed.username or parsed.password or parsed.port
                or parsed.fragment or any(ord(c) < 33 for c in url)):
            return False
        if website:
            if (len(url) > 512 or redirected or parsed.hostname != 'peerbrush.com'
                    or parsed.path not in {'/', *('/' + name for name in SITE_FILES)}):
                return False
            pairs = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True, max_num_fields=2)
            values = dict(pairs)
            if pairs and (len(values) != len(pairs) or not set(values) <= {'release', 'layout'}
                          or not re.fullmatch('v' + STABLE, values.get('release', ''))
                          or ('layout' in values and not re.fullmatch(NUMERIC, values['layout']))):
                return False
            return url == WEBSITE[:-1] + parsed.path + ('?' + urllib.parse.urlencode(pairs) if pairs else '')
        if redirected:
            return parsed.hostname in {'release-assets.githubusercontent.com', 'objects.githubusercontent.com'}
        expected = API if api else DOWNLOAD
        return url.startswith(expected) and not parsed.query
    except (ValueError, TypeError):
        return False


class PublicRedirect(urllib.request.HTTPRedirectHandler):
    def __init__(self, website=False):
        self.website = website

    def redirect_request(self, request, response, code, message, headers, url):
        if self.website or not public_url(url, redirected=True):
            raise Unavailable('Public asset redirect left the allowed HTTPS download hosts.')
        return super().redirect_request(request, response, code, message, headers, url)


def public_get(url, limit, *, api=False, destination=None, website=False):
    """Unauthenticated GET only; no environment credentials or error URLs in logs."""
    if not public_url(url, api=api, website=website) or (api and website) or type(limit) is not int or not 0 < limit <= 512 * 1024 * 1024:
        raise ValueError('Unexpected public release URL.')
    if destination is None and limit > 2 * 1024 * 1024:
        raise ValueError('Large public assets require a temporary file destination.')
    headers = {'User-Agent': 'PeerBrush-public-release-verifier',
               'Accept': 'application/vnd.github+json' if api else 'application/octet-stream',
               'Cache-Control': 'no-cache', 'Pragma': 'no-cache'}
    try:
        deadline = time.monotonic() + 120
        opener = urllib.request.build_opener(PublicRedirect(website))
        with opener.open(urllib.request.Request(url, headers=headers), timeout=15) as response:
            # API calls must never redirect to a binary host.
            if response.geturl() != url and (api or website or not public_url(response.geturl(), redirected=True)):
                raise ValueError('Unexpected final public release URL.')
            if response.status != 200:
                raise Unavailable('Public release GET did not return HTTP 200.')
            size = response.headers.get('Content-Length')
            if size is not None and (not size.isdecimal() or int(size) > limit):
                raise ValueError('Public release response exceeds its size budget.')
            data = b''
            total = 0
            # At most one 64 KiB chunk is retained for large assets. API JSON and
            # small reports are capped separately at 2 MiB. A total deadline
            # prevents a slow peer from resetting the 15-second socket timeout.
            sink = destination.open('xb') if destination is not None else io.BytesIO()
            with sink:
                while True:
                    if time.monotonic() >= deadline:
                        raise Unavailable('Public release transfer exceeded its total deadline.')
                    # HTTPResponse.read may fill the whole request across many
                    # socket reads. read1 returns available data so a dribbling
                    # body cannot indefinitely postpone the deadline check.
                    chunk = response.read1(min(64 * 1024, limit + 1 - total))
                    if time.monotonic() >= deadline:
                        raise Unavailable('Public release transfer exceeded its total deadline.')
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > limit:
                        raise ValueError('Public release response exceeds its size budget.')
                    sink.write(chunk)
                if destination is None:
                    data = sink.getvalue()
            if size is not None and total != int(size):
                raise ValueError('Public release response is oversized or truncated.')
            return data if destination is None else destination
    except (urllib.error.URLError, OSError, http.client.HTTPException):
        raise Unavailable('Public release GET failed; no website update was prepared.') from None


def recognize(name, version):
    escaped = re.escape(version)
    if name == f'PeerBrush-{version}-source.zip':
        return 'source', None
    if re.fullmatch(f'PeerBrush-{escaped}-Windows-x64\\.zip', name):
        return 'zip', 'Windows'
    if re.fullmatch(f'PeerBrush-{escaped}-Linux-x64\\.zip', name):
        return 'zip', 'Linux'
    if re.fullmatch(f'PeerBrush-{escaped}-macOS-arm64(?:-apple-signed-r[1-9][0-9]*)?\\.zip', name):
        return 'zip', 'macOS'
    if re.fullmatch(f'PeerBrush-{escaped}-macOS-arm64-apple-signed-r[1-9][0-9]*-dmg-signed-r[1-9][0-9]*\\.dmg', name):
        return 'dmg', 'macOS'
    if name == f'peerbrush_{deb_version(version)}_amd64.deb':
        return 'deb', 'Linux'
    raise ValueError('Unsupported final release asset filename; native adapter required.')


def validate_inventory(inventory, commit, version):
    if (not isinstance(inventory, dict)
            or set(inventory) != {'schema', 'version', 'commit', 'binary_source_commit', 'packaging_correction', 'assets'}
            or type(inventory['schema']) is not int or inventory['schema'] != 1
            or inventory['version'] != version or inventory['commit'] != commit
            or inventory['binary_source_commit'] != commit or inventory['packaging_correction'] is not None):
        raise ValueError('Expected one stable exact-source final release inventory.')
    records = inventory['assets']
    if not isinstance(records, list) or not 2 <= len(records) <= 6:
        raise ValueError('Expected final platform records and one matching source record.')
    found = {}
    kinds = set()
    for record in records:
        if not isinstance(record, dict):
            raise ValueError('Invalid release inventory record type.')
        name = record.get('file')
        if not isinstance(name, str) or str(relative_path(name)) != name or '/' in name or name in found:
            raise ValueError('Invalid or duplicate release inventory filename.')
        kind, platform = recognize(name, version)
        fields = {'file', 'bytes', 'sha256'}
        if platform is not None:
            fields |= {'platform', 'architecture', 'binary_source_commit', 'source_sha256'}
        if kind == 'zip':
            fields |= {'executable_sha256', 'native_publisher_signing', 'signing_verification'}
        if set(record) != fields:
            raise ValueError('Final inventory permits only the reviewed public source/asset metadata fields.')
        if ((kind, platform) in kinds or type(record.get('bytes')) is not int
                or not 0 < record['bytes'] <= 512 * 1024 * 1024
                or not re.fullmatch('[0-9a-f]{64}', str(record.get('sha256')))):
            raise ValueError('Invalid or duplicate release inventory record.')
        if platform is not None:
            expected_arch = 'amd64' if kind == 'deb' else PLATFORMS[platform][2]
            if (record.get('platform') != platform or record.get('architecture') != expected_arch
                    or record.get('binary_source_commit') != commit
                    or not re.fullmatch('[0-9a-f]{64}', str(record.get('source_sha256')))):
                raise ValueError('Final platform source/architecture metadata differs.')
        found[name] = record
        kinds.add((kind, platform))
    source = found.get(f'PeerBrush-{version}-source.zip')
    if source is None or any(r.get('source_sha256') != source['sha256'] for r in records if 'platform' in r):
        raise ValueError('Final platforms must bind the same corresponding source hash.')
    return found


def installed_report(kind, record, data, commit, version):
    """Recognize our exact candidate receipts; never promote their native claims."""
    if kind == 'dmg':
        required = {'file': record['file'], 'version': version, 'architecture': 'arm64',
                    'binary_source_commit': commit, 'image_signing': 'developer-id-notarized',
                    'notary_status': 'Accepted', 'secure_timestamp': True, 'stapled': True,
                    'drag_to_applications_test': 'not-performed', 'fresh_quarantined_download_test': 'not-performed'}
    else:
        required = {'file': record['file'], 'version': version, 'architecture': 'amd64',
                    'target': 'Ubuntu 24.04 amd64', 'update_owner': 'apt-dpkg',
                    'update_repository': 'not-configured', 'signing': 'not-performed',
                    'installation_test': 'not-performed', 'default_app': 'user-choice-only'}
    required.update(schema=1, commit=commit, sha256=record['sha256'],
                    source_sha256=record['source_sha256'], release_ready=False)
    if not isinstance(data, dict) or any(type(data.get(k)) is not type(v) or data.get(k) != v for k, v in required.items()):
        raise ValueError('Final installer receipt is unsupported or differs from its inventory.')
    # shortcut: exact public bytes and candidate receipt only; a reviewed native
    # final-installer/source/acceptance adapter is required before website selection.
    return 'native-final-installer-validation-not-configured'


def published(root, inventory, tag, commit, notes, get=public_get, *, maintainer=None):
    match = re.fullmatch('v(' + STABLE + ')', tag)
    if not match or not re.fullmatch('[0-9a-f]{40}', commit):
        raise ValueError('Choose a canonical stable vA.B.C tag and full source SHA.')
    version = match[1]
    records = validate_inventory(inventory, commit, version)
    selected = source_files(root, commit)
    inputs = {name: data for name, data, _ in selected}
    if tomllib.loads(inputs['Cargo.toml'].decode())['package']['version'] != version:
        raise ValueError('Published version differs from the exact selected Cargo source.')
    policy = json.loads(inputs['packaging/release-policy.json'])
    formats = policy.get('formats') if isinstance(policy, dict) else None
    if (not isinstance(policy, dict) or set(policy) != {'schema', 'formats'} or type(policy['schema']) is not int or policy['schema'] != 1
            or not isinstance(formats, dict) or set(formats) != set(PLATFORMS)
            or formats['Windows'] not in {'zip', 'microsoft-store'}
            or formats['macOS'] not in {'zip', 'dmg'} or formats['Linux'] not in {'zip', 'deb'}):
        raise ValueError('Unsupported committed release distribution policy.')
    if (not isinstance(notes, dict) or set(notes) != {'notes', 'caution'} or not isinstance(notes['notes'], list)
            or not 1 <= len(notes['notes']) <= 30 or any(not isinstance(n, str) or not 0 < len(n) <= 2000 for n in notes['notes'])
            or not isinstance(notes['caution'], str) or len(notes['caution']) > 4000):
        raise ValueError('Supply reviewed public notes and caution text only.')
    release = json.loads(get(API + 'releases/tags/' + tag, 2 * 1024 * 1024, api=True))
    resolved = json.loads(get(API + 'commits/' + tag, 2 * 1024 * 1024, api=True))
    if (not isinstance(release, dict) or not isinstance(resolved, dict)
            or release.get('tag_name') != tag or release.get('draft') is not False
            or release.get('prerelease') is not False or release.get('html_url') != RELEASE + tag
            or resolved.get('sha') != commit or type(release.get('published_at')) is not str
            or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z', release['published_at'])):
        raise ValueError('Release is not publicly published, stable and bound to the selected source.')
    try:
        datetime.datetime.strptime(release['published_at'], '%Y-%m-%dT%H:%M:%SZ')
    except ValueError:
        raise ValueError('Public release timestamp is not a canonical UTC date.') from None
    assets = release.get('assets')
    if (not isinstance(assets, list) or len(assets) > 100
            or any(not isinstance(a, dict) or type(a.get('name')) is not str for a in assets)):
        raise ValueError('Public release asset inventory is ambiguous or oversized.')
    for asset in assets:
        name = asset['name']
        if (len(name) > 200 or '/' in str(relative_path(name)) or asset.get('state') != 'uploaded'
                or type(asset.get('size')) is not int or not 0 < asset['size'] <= 512 * 1024 * 1024
                or asset.get('browser_download_url') != DOWNLOAD + tag + '/' + urllib.parse.quote(name, safe='')
                or (asset.get('digest') is not None and not re.fullmatch('sha256:[0-9a-f]{64}', str(asset['digest'])))):
            raise ValueError('Public release asset metadata has an unsupported type, state or value.')
    if len({a['name'] for a in assets}) != len(assets):
        raise ValueError('Public release asset inventory has duplicate names.')
    assets = {a['name']: a for a in assets}

    def download(name, maximum, required=True, destination=None):
        asset = assets.get(name)
        if asset is None:
            if not required:
                return None
            raise ValueError('Required public release asset is unavailable.')
        url = DOWNLOAD + tag + '/' + urllib.parse.quote(name, safe='')
        if (asset.get('browser_download_url') != url or asset.get('state') != 'uploaded'
                or type(asset.get('size')) is not int or not 0 < asset['size'] <= maximum):
            raise ValueError('Unexpected public release asset URL/state/size.')
        data = get(url, maximum, **({'destination': destination} if destination is not None else {}))
        size = destination.stat().st_size if destination is not None else len(data)
        actual_hash = file_sha(destination) if destination is not None else sha(data)
        if size != asset['size'] or (asset.get('digest') is not None and asset['digest'] != 'sha256:' + actual_hash):
            raise ValueError('Downloaded asset differs from the public release metadata.')
        return data

    checksums = download('SHA256SUMS', 64 * 1024)
    expected_checksums = ''.join(f"{r['sha256']}  {r['file']}\n" for r in inventory['assets'])
    if checksums.decode('ascii').replace('\r\n', '\n') != expected_checksums:
        raise ValueError('Published checksum list differs from the verified final inventory.')
    source_name = f'PeerBrush-{version}-source.zip'
    # Reuse the source validator inside each eligible portable archive; no code
    # from the downloaded release is executed or extracted into the checkout.
    platforms, excluded, seen = [], [], set()
    with tempfile.TemporaryDirectory(prefix='peerbrush-public-') as temporary:
        source_path = pathlib.Path(temporary) / source_name
        download(source_name, min(records[source_name]['bytes'], 64 * 1024 * 1024), destination=source_path)
        if source_path.stat().st_size != records[source_name]['bytes'] or file_sha(source_path) != records[source_name]['sha256']:
            raise ValueError('Published corresponding source differs from its final inventory.')
        with zipfile.ZipFile(source_path) as archive:
            if (len(archive.infolist()) != len(inputs) or set(archive.namelist()) != set(inputs)
                    or any(archive.getinfo(name).file_size != len(data) for name, data in inputs.items())
                    or any(archive.read(name) != data for name, data in inputs.items())):
                raise ValueError('Published corresponding source differs from the exact selected commit.')
        # Inspect each portable companion once before final DEB content.
        for name, record in sorted(records.items(), key=lambda item: recognize(item[0], version)[0] == 'deb'):
            kind, platform = recognize(name, version)
            if kind == 'source':
                continue
            seen.add((kind, platform))
            path = pathlib.Path(temporary) / name
            if download(name, record['bytes'], required=False, destination=path) is None:
                excluded.append(dict(file=name, reason='not-publicly-available'))
                continue
            if path.stat().st_size != record['bytes'] or file_sha(path) != record['sha256']:
                raise ValueError('Published package bytes differ from the final inventory.')
            if kind in {'dmg', 'deb'}:
                suffix = '.verification.json' if kind == 'dmg' else '.json'
                report = json.loads(download(name + suffix, 2 * 1024 * 1024))
                checksum = download(name + '.sha256', 1024)
                if checksum.decode('ascii').replace('\r\n', '\n') != f"{record['sha256']}  {name}\n":
                    raise ValueError('Final installer checksum report differs.')
                reason = installed_report(kind, record, report, commit, version)
                exclusion = dict(file=name, reason=reason)
                if kind == 'deb':
                    exclusion.update(content_verified=False, ready=False, production_ready=False,
                                     installed_acceptance='unverified', update_repository='not-configured',
                                     public_availability='unverified')
                    if maintainer is None:
                        exclusion['reason'] = 'final-deb-approved-maintainer-not-configured'
                    else:
                        portable_records = [r for r in records.values() if recognize(r['file'], version) == ('zip', 'Linux')]
                        if len(portable_records) != 1:
                            exclusion['reason'] = 'final-deb-portable-not-in-inventory'
                        else:
                            portable = portable_records[0]
                            portable_path = pathlib.Path(temporary) / portable['file']
                            if not portable_path.exists():
                                exclusion['reason'] = 'final-deb-portable-not-publicly-available'
                            else:
                                checked = inspect_final(root, portable_path, path, commit=commit, version=version,
                                                        portable_sha256=portable['sha256'], package_sha256=record['sha256'],
                                                        maintainer=maintainer)
                                if checked['record'] != record or checked['portable_sha256'] != portable['sha256']:
                                    raise ValueError('Final DEB content differs from the selected public inventory.')
                                exclusion.update(content_verified=True, content_checks=checked['content_checks'])
                excluded.append(exclusion)
                continue
            inspected = inspect_archive(path, commit, version, root, selected)
            if inspected['record'] != record or sha(inspected['source']) != records[source_name]['sha256']:
                raise ValueError('Public archive/source differs from the exact verified inventory.')
            if formats[platform] != kind:
                excluded.append(dict(file=name, reason='not-selected-by-committed-distribution-policy'))
                continue
            identifier, display, architecture = PLATFORMS[platform]
            trust = 'unsigned' if record['signing_verification'] == 'unsigned' else 'unverified'
            platforms.append(dict(id=identifier, name=display, architecture=architecture,
                                  url=assets[name]['browser_download_url'], bytes=record['bytes'], sha256=record['sha256'],
                                  nativeSigning=trust, packageReadiness='portable-archive-verified',
                                  description=f'{architecture} · Portable ZIP · Native trust {trust}'))
    for platform, kind in formats.items():
        if (kind, platform) not in seen:
            excluded.append(dict(platform=platform, format=kind,
                                 reason='store-public-availability-unverified' if kind == 'microsoft-store'
                                 else 'selected-final-distribution-not-in-inventory'))
    if len({p['id'] for p in platforms}) != len(platforms):
        raise ValueError('Multiple selectable packages for one platform require an explicit channel policy.')
    entry = dict(version=tag, label=f'PeerBrush {version} · Stable', kind='stable',
                 publishedAt=release['published_at'], commit=commit, releaseUrl=release['html_url'],
                 platforms=platforms, sourceUrl=assets[source_name]['browser_download_url'],
                 checksumsUrl=assets['SHA256SUMS']['browser_download_url'],
                 releaseInventorySha256=inventory_sha(inventory),
                 caution=notes['caution'], notes=notes['notes'])
    return entry, excluded


def site_sources(root, commit):
    if not re.fullmatch('[0-9a-f]{40}', commit) or git(root, 'rev-parse', commit + '^{commit}').decode().strip() != commit:
        raise ValueError('Choose an exact reviewed site-content commit.')
    return {name: git(root, 'show', commit + ':public-site/' + name).decode('utf-8') for name in SITE_FILES}


def render(previous, entry):
    data = json.loads(previous['downloads.json'])
    if (not isinstance(data, dict) or type(data.get('schema')) is not int or data['schema'] != 2
            or not isinstance(data.get('history'), list) or any(not isinstance(r, dict) or type(r.get('version')) is not str for r in data['history'])):
        raise ValueError('Expected the existing schema2 website release history.')
    history = data['history']
    if len({r.get('version') for r in history}) != len(history) or not entry['platforms']:
        raise ValueError('History versions must be unique and a new release needs a public selectable platform.')
    selected = next((r for r in data['history'] if r['version'] == data.get('version')), None)
    latest_keys = {'version', 'label', 'commit', 'releaseUrl', 'buildUrl', 'platforms', 'caution', 'sourceUrl', 'checksumsUrl', 'releaseInventorySha256'}
    if selected is None or any(data.get(key) != selected.get(key) for key in latest_keys):
        raise ValueError('Existing website latest fields must match one retained history entry.')
    old = next((r for r in history if r['version'] == entry['version']), None)
    if old is not None:
        if selected['version'] != entry['version']:
            raise ValueError('Historical availability changes require separate exact-source latest-release verification.')
        if old != entry:
            # Late availability may select only bytes already in the pinned
            # inventory. Notes, dates, source and existing download rows stay fixed.
            unchanged = lambda row: {k: v for k, v in row.items() if k != 'platforms'}
            rows = entry['platforms']
            if (unchanged(old) != unchanged(entry) or old.get('kind') != 'stable'
                    or not re.fullmatch('[0-9a-f]{64}', str(old.get('releaseInventorySha256')))
                    or not isinstance(old.get('platforms'), list)
                    or not len(old['platforms']) < len(rows) <= 3
                    or any(not isinstance(p, dict) or type(p.get('id')) is not str
                           or p['id'] not in {v[0] for v in PLATFORMS.values()} for p in rows)
                    or len({p['id'] for p in rows}) != len(rows)
                    or any(p not in rows for p in old['platforms'])):
                raise ValueError('Published history permits only added platforms from its unchanged pinned inventory.')
        history = [entry if r['version'] == entry['version'] else r for r in history]
    else:
        history = [entry, *history]
    for retained in data['history']:
        if retained.get('kind') == 'stable':
            old_version = retained['version'].removeprefix('v')
            if not re.fullmatch(STABLE, old_version):
                raise ValueError('Retained stable history requires a canonical release version.')
            immutable(dict(schema=1, version=old_version, commit=retained['commit'],
                           assets=[dict(file=urllib.parse.unquote(p['url'].rsplit('/', 1)[-1]), sha256=p['sha256']) for p in retained['platforms']]),
                      dict(version=entry['version'][1:], commit=entry['commit'],
                           assets=[dict(file=urllib.parse.unquote(p['url'].rsplit('/', 1)[-1]), sha256=p['sha256']) for p in entry['platforms']]))
    result = dict(previous)
    latest = {key: value for key, value in entry.items() if key not in {'kind', 'publishedAt', 'notes'}}
    feed = encode(dict(latest, schema=2, history=history))
    result['downloads.json'] = feed.decode()
    # A delayed platform changes the feed without changing its release version.
    cache = 'release=' + entry['version'] + '&layout=' + str(int(sha(feed)[:12], 16))
    escape = html.escape
    rows = []
    for p in entry['platforms']:
        rows.append(f'<div class="platform-row"><div class="platform-info"><svg class="icon" aria-hidden="true"><use href="#{p["id"]}"/></svg><div><h3>{p["name"]}</h3><p>{escape(p["description"])}</p></div></div><span class="package-size">{p["bytes"]/1e6:.1f} MB</span><a class="platform-download" href="{escape(p["url"], quote=True)}">Download <svg class="icon" aria-hidden="true"><use href="#download"/></svg></a></div><p class="download-checksum">{p["name"]} SHA-256: <code>{p["sha256"]}</code></p>')

    def replace(text, pattern, replacement, count=1):
        updated, actual = re.subn(pattern, lambda _: replacement, text, flags=re.S)
        if actual != count:
            raise ValueError('Reviewed website output layout changed; update the renderer before preparing output.')
        return updated

    index = result['index.html']
    index = replace(index, r'<div class="platform-list" id="platform-list">.*?</div>\s*<p class="download-caution">', '<div class="platform-list" id="platform-list">\n' + '\n'.join(rows) + '\n</div>\n<p class="download-caution">')
    index = replace(index, r'<p class="download-caution">.*?</p>', '<p class="download-caution">' + escape(entry['caution']) + '</p>')
    index = replace(index, r'<p class="download-meta" id="download-meta">.*?</p>', '<p class="download-meta" id="download-meta">' + escape(entry['label'] + ' · ' + ' & '.join(p['name'] for p in entry['platforms'])) + '</p>')
    index = replace(index, r'<span class="build-label" id="build-label">.*?</span>', '<span class="build-label" id="build-label">' + escape(entry['label']) + '</span>')
    index = replace(index, r'(<a class="text-link" id="release-link"\s+href=")[^"]*', '<a class="text-link" id="release-link" href="' + entry['releaseUrl'])
    result['index.html'] = index
    result['main.js'] = replace(result['main.js'], r'''if \(!loaded\) document.querySelector\('#build-label'\).textContent = (?:'[^']*'|"[^"]*");''', "if (!loaded) document.querySelector('#build-label').textContent = " + json.dumps(entry['label']) + ';')
    cache_counts = {'index.html': 1, 'main.js': 2, 'platform.js': 0,
                    'releases.js': 1, 'release-page.js': 2, 'releases.html': 1}
    for name, count in cache_counts.items():
        result[name] = replace(result[name], r'release=[^\s\'"&;]+(?:(?:&amp;|&)layout=[0-9]+)?',
                               escape(cache, quote=True) if name.endswith('.html') else cache, count)
    result['releases.html'], count = re.subn(r'(data-doc="([a-z0-9.-]+)"\s+href=")https://github.com/Deftr0y/PeerBrush/blob/[^/]+/docs/[^"]+',
                                           lambda m: m[1] + 'https://github.com/' + REPOSITORY + '/blob/' + entry['commit'] + '/docs/' + m[2], result['releases.html'])
    if count != 4:
        raise ValueError('Reviewed website documentation layout changed; update the renderer.')
    return {name: text.encode('utf-8') for name, text in result.items()}


def output_path(root, output):
    output = output.absolute()
    if '..' in output.parts:
        raise ValueError('Website output path must not contain parent traversal.')
    worktrees = [root.resolve()]
    for field in git(root, 'worktree', 'list', '--porcelain', '-z').decode().split('\0'):
        if field.startswith('worktree '):
            worktrees.append(pathlib.Path(field[9:]).resolve())
    for path in [output, *output.parents]:
        if path.exists() or path.is_symlink():
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
                raise ValueError('Website output path must not traverse links or reparse points.')
    if output.exists() or any(output == tree or output.is_relative_to(tree) for tree in worktrees):
        raise ValueError('Choose a new immutable website output outside every source worktree.')
    if not output.parent.is_dir():
        raise ValueError('Choose an existing private parent directory for website candidate output.')
    return output


def prepare(root, site_commit, inventory, tag, commit, notes, output, get=public_get, *, maintainer=None):
    output = output_path(root, output)
    previous = site_sources(root, site_commit)
    entry, excluded = published(root, inventory, tag, commit, notes, get, maintainer=maintainer)
    overlay = render(previous, entry) if entry['platforms'] else {}
    report = dict(schema=1, version=tag, commit=commit, site_commit=site_commit,
                  site_update_prepared=bool(overlay), deployment_performed=False,
                  production_readiness='not-assessed', store_public_availability='unverified',
                  release_inventory_sha256=inventory_sha(inventory),
                  excluded=excluded, files={name: sha(data) for name, data in overlay.items()})
    # Verification may involve several transfers. Recheck links/worktrees at
    # the point of use instead of relying on the earlier advisory path check.
    output = output_path(root, output)
    output.mkdir(mode=0o700)
    metadata = output.lstat()
    if (not stat.S_ISDIR(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400
            or (os.name == 'posix' and (metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700))):
        raise ValueError('New website candidate directory must be private and owned by the current user.')
    if overlay:
        (output / 'public-site').mkdir(mode=0o700)
        for name, data in overlay.items():
            with (output / 'public-site' / name).open('xb') as stream:
                stream.write(data)
    with (output / 'website-output.json').open('xb') as stream:
        stream.write(encode(report))
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['site-commit', 'tag', 'commit']:
        parser.add_argument('--' + name, required=True)
    for name in ['inventory', 'notes', 'output']:
        parser.add_argument('--' + name, required=True, type=pathlib.Path)
    parser.add_argument('--maintainer', help='Approved public DEB maintainer name <email>; content inspection does not establish download readiness')
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    report = prepare(root, args.site_commit, json.loads(args.inventory.read_bytes()), args.tag,
                     args.commit, json.loads(args.notes.read_bytes()), args.output, maintainer=args.maintainer)
    print('Reviewable website overlay prepared; deployment was not performed.' if report['site_update_prepared']
          else 'No eligible public platforms; only an exclusion report was prepared.')
    return 0 if report['site_update_prepared'] else 2


if __name__ == '__main__':
    raise SystemExit(main())
