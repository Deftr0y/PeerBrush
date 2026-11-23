"""Compare the reviewed or live website with the current GitHub stable release."""
import argparse
import json
from pathlib import Path
import re
import time
from urllib.parse import quote

from prepare_site_release import API, DOWNLOAD, RELEASE, SITE_FILES, STABLE, Unavailable, public_get, sha
from production_readiness import unique_fields
from readback_site_release import resource_urls


LIMIT = 2 * 1024 * 1024
SHARED_FIELDS = ('version', 'label', 'commit', 'releaseUrl', 'platforms',
                 'sourceUrl', 'checksumsUrl', 'caution')


def check_release(raw, get=public_get):
    feed = json.loads(raw, object_pairs_hook=unique_fields)
    release = json.loads(get(API + 'releases/latest', LIMIT, api=True))
    tag = release['tag_name']
    if (not re.fullmatch('v' + STABLE, tag) or release['draft'] is not False
            or release['prerelease'] is not False or release['html_url'] != RELEASE + tag
            or not release['published_at']):
        raise ValueError('GitHub does not identify a published stable release.')
    if feed['schema'] != 2 or feed['version'] != tag:
        raise ValueError(f'Reviewed website release data must be updated to {tag} before deployment.')
    matches = [entry for entry in feed['history'] if entry['version'] == tag]
    if len(matches) != 1:
        raise ValueError('Website history must contain one entry for the current stable release.')
    entry = matches[0]
    commit = json.loads(get(API + 'commits/' + tag, LIMIT, api=True))['sha']
    if (not re.fullmatch('[0-9a-f]{40}', commit) or feed['commit'] != commit
            or feed['releaseUrl'] != release['html_url'] or entry['kind'] != 'stable'
            or entry['publishedAt'] != release['published_at']
            or not isinstance(entry['notes'], list) or not entry['notes']
            or any(feed[field] != entry[field] for field in SHARED_FIELDS)):
        raise ValueError('Website latest and history do not match the published release and source.')

    assets = {asset['name']: asset for asset in release['assets']}
    if len(assets) != len(release['assets']):
        raise ValueError('GitHub returned ambiguous release assets.')

    def checked_asset(name, url):
        asset = assets[name]
        if (url != DOWNLOAD + tag + '/' + quote(name, safe='')
                or asset['browser_download_url'] != url or asset['state'] != 'uploaded'
                or type(asset['size']) is not int or asset['size'] <= 0):
            raise ValueError('Website download does not match an uploaded release asset.')
        return asset

    checksum_asset = checked_asset('SHA256SUMS', feed['checksumsUrl'])
    source_name = f'PeerBrush-{tag[1:]}-source.zip'
    source_asset = checked_asset(source_name, feed['sourceUrl'])
    raw_checksums = get(feed['checksumsUrl'], 64 * 1024)
    if (len(raw_checksums) != checksum_asset['size']
            or checksum_asset.get('digest') not in (None, 'sha256:' + sha(raw_checksums))):
        raise ValueError('Published checksum file differs from the GitHub asset.')
    checksums = {}
    for line in raw_checksums.decode('ascii').splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  ([^/\\]+)', line)
        if not match or match[2] in checksums:
            raise ValueError('Published checksum file is malformed or ambiguous.')
        checksums[match[2]] = match[1]
    if source_name not in checksums or source_asset.get('digest') not in (None, 'sha256:' + checksums[source_name]):
        raise ValueError('Published source archive differs from the release checksums.')
    for row in feed['platforms']:
        name = next((name for name, asset in assets.items() if asset['browser_download_url'] == row['url']), None)
        if name is None:
            raise ValueError('Website platform download is missing from GitHub.')
        asset = checked_asset(name, row['url'])
        if (checksums.get(name) != row['sha256'] or asset['size'] != row['bytes']
                or asset.get('digest') not in (None, 'sha256:' + row['sha256'])):
            raise ValueError('Website platform checksum or size differs from GitHub.')
    return tag


def live_matches(site, get=public_get):
    files = {name: (site / name).read_bytes() for name in SITE_FILES}
    return all(get(url, LIMIT, website=True) == files[name] for url, name in resource_urls(files).items())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('site', type=Path)
    parser.add_argument('--readback', action='store_true')
    parser.add_argument('--github-output', type=Path)
    args = parser.parse_args()
    try:
        tag = check_release((args.site / 'downloads.json').read_bytes())
        if args.readback:
            for delay in (0, 15, 30, 45):
                time.sleep(delay)
                try:
                    if live_matches(args.site):
                        print(f'GitHub and the live website agree on {tag}.')
                        return 0
                except Unavailable:
                    pass
            raise ValueError('Live website did not return the promoted release files.')
        print(f'Reviewed website matches published GitHub release {tag}.')
        if args.github_output:
            try:
                current = live_matches(args.site)
            except Unavailable:
                current = False
            with args.github_output.open('a', encoding='utf-8') as stream:
                stream.write('deploy=' + ('false' if current else 'true') + '\n')
    except (KeyError, TypeError, OSError, UnicodeError, json.JSONDecodeError):
        parser.exit(2, 'Website or GitHub release metadata is missing or malformed.\n')
    except ValueError as error:
        parser.exit(2, f'{error}\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
