"""Read official deployed files and public downloads; never deploy or publish."""
import argparse
import html.parser
import json
import pathlib
import re
import subprocess
import urllib.parse

from package import git, regular_file
from prepare_site_release import (API, DOWNLOAD, PLATFORMS, RELEASE, SITE_FILES, STABLE, WEBSITE, Unavailable,
                                  encode, inventory_sha, public_get, public_url,
                                  published, recognize, render, sha)


LIMIT = 2 * 1024 * 1024
REPORT_FIELDS = {'schema', 'version', 'commit', 'site_commit', 'site_update_prepared',
                 'deployment_performed', 'production_readiness', 'store_public_availability',
                 'release_inventory_sha256', 'excluded', 'files'}
INDEPENDENT_STATUS = dict(native_signatures='not-verified-by-readback', installed_acceptance='not-assessed',
                          store_public_availability='unverified', production_readiness='not-assessed',
                          deployment_performed=False)


def selected_platforms(entry, version):
    platforms = entry.get('platforms')
    fields = {'id', 'name', 'architecture', 'url', 'bytes', 'sha256',
              'nativeSigning', 'packageReadiness', 'description'}
    if (not isinstance(platforms, list) or not 1 <= len(platforms) <= 3
            or any(not isinstance(p, dict) or set(p) != fields or type(p['id']) is not str for p in platforms)
            or len({p['id'] for p in platforms}) != len(platforms)):
        raise ValueError('Expected selectable platform metadata is invalid.')
    definitions = {identifier: (name, arch) for identifier, name, arch in PLATFORMS.values()}
    for platform in platforms:
        if (platform['id'] not in definitions or (platform['name'], platform['architecture']) != definitions[platform['id']]
                or type(platform['url']) is not str or not platform['url'].startswith(DOWNLOAD + version + '/')
                or not public_url(platform['url']) or type(platform['bytes']) is not int
                or not 0 < platform['bytes'] <= 512 * 1024 * 1024
                or type(platform['sha256']) is not str or not re.fullmatch('[0-9a-f]{64}', platform['sha256'])
                or type(platform['nativeSigning']) is not str or platform['nativeSigning'] not in {'unsigned', 'unverified'}
                or platform['packageReadiness'] != 'portable-archive-verified'
                or type(platform['description']) is not str or len(platform['description']) > 500):
            raise ValueError('Expected platform requires the reviewed portable/unverified trust contract.')
        name = urllib.parse.unquote(platform['url'].rsplit('/', 1)[-1])
        if recognize(name, version[1:]) != ('zip', platform['name']):
            raise ValueError('Expected platform URL differs from its version/architecture.')


def expected(directory):
    """The operator's expected bytes are a comparison target, not a trust grant."""
    report_path = regular_file(directory, 'website-output.json')
    if report_path.stat().st_size > 64 * 1024:
        raise ValueError('Expected website report exceeds its budget.')
    report = json.loads(report_path.read_bytes())
    if (not isinstance(report, dict) or set(report) != REPORT_FIELDS
            or type(report['schema']) is not int or report['schema'] != 1
            or report['site_update_prepared'] is not True or report['deployment_performed'] is not False
            or report['production_readiness'] != 'not-assessed' or report['store_public_availability'] != 'unverified'
            or type(report['version']) is not str or len(report['version']) > 64 or not re.fullmatch('v' + STABLE, report['version'])
            or any(type(report[key]) is not str or not re.fullmatch('[0-9a-f]{40}', report[key]) for key in ['commit', 'site_commit'])
            or type(report['release_inventory_sha256']) is not str or not re.fullmatch('[0-9a-f]{64}', report['release_inventory_sha256'])
            or not isinstance(report['excluded'], list) or len(report['excluded']) > 16
            or not isinstance(report['files'], dict) or set(report['files']) != set(SITE_FILES)):
        raise ValueError('Expected a prepared seven-file report with separate unverified production/Store status.')
    files = {}
    for name, digest in report['files'].items():
        path = regular_file(directory, 'public-site/' + name)
        if type(digest) is not str or not re.fullmatch('[0-9a-f]{64}', digest) or path.stat().st_size > LIMIT:
            raise ValueError('Expected website file/hash exceeds the comparison contract.')
        data = path.read_bytes()
        if sha(data) != digest:
            raise ValueError('Expected website file differs from its prepared report.')
        files[name] = data
    if set(p.name for p in (directory / 'public-site').iterdir()) != set(SITE_FILES):
        raise ValueError('Expected overlay contains unsupported additional files.')
    decoded = {name: data.decode('utf-8') for name, data in files.items()}
    data = json.loads(decoded['downloads.json'])
    if (not isinstance(data, dict) or not isinstance(data.get('history'), list)
            or not 1 <= len(data['history']) <= 100
            or any(not isinstance(r, dict) or type(r.get('version')) is not str
                   or not isinstance(r.get('platforms'), list) or len(r['platforms']) > 3
                   or any(not isinstance(p, dict) or type(p.get('url')) is not str
                          or (p.get('sha256') is not None and type(p['sha256']) is not str)
                          for p in r['platforms']) for r in data['history'])):
        raise ValueError('Expected website release history is invalid.')
    entries = [r for r in data['history'] if isinstance(r, dict) and r.get('version') == report['version']]
    if len(entries) != 1 or entries[0].get('commit') != report['commit'] or entries[0].get('kind') != 'stable':
        raise ValueError('Expected report differs from its selected stable release source.')
    entry = entries[0]
    if (type(entry.get('notes')) is not list or not 1 <= len(entry['notes']) <= 30
            or any(type(n) is not str or not 0 < len(n) <= 2000 for n in entry['notes'])
            or type(entry.get('caution')) is not str or len(entry['caution']) > 4000
            or entry.get('releaseUrl') != RELEASE + report['version']
            or entry.get('sourceUrl') != DOWNLOAD + report['version'] + '/PeerBrush-' + report['version'][1:] + '-source.zip'
            or entry.get('checksumsUrl') != DOWNLOAD + report['version'] + '/SHA256SUMS'
            or entry.get('releaseInventorySha256') != report['release_inventory_sha256']):
        raise ValueError('Expected release links/notes differ from the prepared contract.')
    # Check row types before the renderer reads them for immutability/layout.
    selected_platforms(entry, report['version'])
    if render(decoded, entry) != files:
        raise ValueError('Expected static/dynamic website bytes do not form one consistent prepared overlay.')
    return report, files, entry


def resource_urls(files):
    """Check canonical files and the actual cache URLs the reviewed pages use."""
    requests = {WEBSITE + name: name for name in SITE_FILES}
    requests[WEBSITE] = 'index.html'

    class Scripts(html.parser.HTMLParser):
        def handle_starttag(self, tag, attrs):
            values = dict(attrs)
            if tag == 'script' and values.get('type') == 'module':
                value = values.get('src', '')
                url = urllib.parse.urljoin(WEBSITE, value)
                name = urllib.parse.urlsplit(url).path.removeprefix('/')
                if name in SITE_FILES:
                    requests[url] = name

    for name in ['index.html', 'releases.html']:
        Scripts().feed(files[name].decode())
    for name in ['main.js', 'releases.js', 'release-page.js']:
        for value in re.findall(r'''from ['"](\./(?:platform|releases)\.js\?[^'"]+)['"]''', files[name].decode()):
            url = urllib.parse.urljoin(WEBSITE, value)
            requests[url] = urllib.parse.urlsplit(url).path.removeprefix('/')
    for url, name in list(requests.items()):
        if name in {'main.js', 'release-page.js'} and urllib.parse.urlsplit(url).query:
            requests[WEBSITE + 'downloads.json?' + urllib.parse.urlsplit(url).query] = 'downloads.json'
    if len(requests) > 16 or any(not public_url(url, website=True) for url in requests):
        raise ValueError('Expected page resource URLs left the bounded official website contract.')
    return requests


def aggregate(statuses):
    return 'unavailable' if 'unavailable' in statuses else 'mismatch' if 'mismatch' in statuses else 'verified'


def downloads(root, report, entry, get):
    """Only the published public inventory is read; no private receipts accepted."""
    release = json.loads(get(API + 'releases/tags/' + report['version'], LIMIT, api=True))
    assets = release.get('assets') if isinstance(release, dict) else None
    if not isinstance(assets, list) or len(assets) > 100 or any(not isinstance(a, dict) for a in assets):
        raise ValueError('Malformed public release asset collection.')
    found = [a for a in assets if a.get('name') == 'release-inventory.json']
    if not found:
        return dict(status='unavailable', reason='public-release-inventory-unavailable', verified_platforms=[])
    url = DOWNLOAD + report['version'] + '/release-inventory.json'
    if (len(found) != 1 or found[0].get('browser_download_url') != url or found[0].get('state') != 'uploaded'
            or type(found[0].get('size')) is not int or not 0 < found[0]['size'] <= LIMIT):
        raise ValueError('Unexpected public release inventory metadata.')
    raw = get(url, LIMIT)
    if len(raw) != found[0]['size'] or (found[0].get('digest') is not None and found[0]['digest'] != 'sha256:' + sha(raw)):
        raise ValueError('Public release inventory differs from its asset digest/length.')
    inventory = json.loads(raw)
    if inventory_sha(inventory) != report['release_inventory_sha256']:
        raise ValueError('Public source/asset inventory differs from the prepared comparison target.')
    actual, _ = published(root, inventory, report['version'], report['commit'],
                          dict(notes=entry['notes'], caution=entry['caution']), get)
    expected_ids = {p['id'] for p in entry['platforms']}
    if not expected_ids <= {p['id'] for p in actual['platforms']}:
        return dict(status='unavailable', reason='selected-public-platform-unavailable',
                    verified_platforms=[p['id'] for p in actual['platforms'] if p['id'] in expected_ids])
    return dict(status='verified' if actual == entry else 'mismatch',
                reason='public-bytes-source-checksums-match' if actual == entry else 'public-release-selection-differs',
                verified_platforms=sorted(expected_ids))


def check(directory, source_root, get=public_get):
    try:
        report, files, entry = expected(directory)
        requests = resource_urls(files)
        if git(source_root, 'rev-parse', 'HEAD').decode().strip() != report['commit']:
            raise ValueError('Exact selected source checkout is required.')
    except (ValueError, OSError, KeyError, TypeError, RecursionError, subprocess.SubprocessError):
        return dict(schema=1, status='unavailable', reason='expected-overlay-or-source-unavailable',
                    website=dict(status='not-checked', files=[]),
                    downloads=dict(status='not-checked', verified_platforms=[]), **INDEPENDENT_STATUS)
    results = []
    for url, name in requests.items():
        parsed = urllib.parse.urlsplit(url)
        result = dict(path=parsed.path + ('?' + parsed.query if parsed.query else ''),
                      expected_sha256=sha(files[name]))
        try:
            data = get(url, LIMIT, website=True)
            result.update(status='verified' if data == files[name] else 'mismatch', actual_sha256=sha(data))
        except (Unavailable, OSError):
            result['status'] = 'unavailable'
        except ValueError:
            result['status'] = 'mismatch'
        results.append(result)
    try:
        public = downloads(source_root, report, entry, get)
    except (Unavailable, OSError, subprocess.SubprocessError):
        public = dict(status='unavailable', reason='public-release-verification-unavailable', verified_platforms=[])
    except (ValueError, KeyError, TypeError, RecursionError):
        public = dict(status='mismatch', reason='public-release-verification-failed', verified_platforms=[])
    website_status = aggregate([r['status'] for r in results])
    return dict(schema=1, version=report['version'], commit=report['commit'],
                expected_site_commit=report['site_commit'],
                status=aggregate([website_status, public['status']]),
                website=dict(status=website_status, files=results), downloads=public,
                **INDEPENDENT_STATUS)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected', required=True, type=pathlib.Path, help='Prepared external overlay/report directory')
    parser.add_argument('--source-root', required=True, type=pathlib.Path, help='Exact selected release source checkout')
    args = parser.parse_args(argv)
    result = check(args.expected, args.source_root)
    print(encode(result).decode(), end='')
    return 0 if result['status'] == 'verified' else 2


if __name__ == '__main__':
    raise SystemExit(main())
