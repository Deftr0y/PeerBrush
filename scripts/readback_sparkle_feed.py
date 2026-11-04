"""Compare the live stable feed and DMG with the protected producers' reviewed hashes."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile
from xml.etree import ElementTree as ET

from prepare_site_release import DOWNLOAD, STABLE, WEBSITE, public_get, recognize


def check(version, feed_hash, dmg_hash, get=public_get):
    if not re.fullmatch(STABLE, version) or not all(re.fullmatch('[0-9a-f]{64}', h) for h in (feed_hash, dmg_hash)):
        raise ValueError('Expected a stable version and two reviewed SHA-256 values.')
    feed = get(WEBSITE + 'updates/macos/stable/appcast.xml', 256 * 1024, website=True)
    if hashlib.sha256(feed).hexdigest() != feed_hash:
        raise ValueError('The live appcast differs from the reviewed signed feed.')
    items = ET.fromstring(feed).findall('channel/item')
    ns = '{http://www.andymatuschak.org/xml-namespaces/sparkle}'
    if len(items) != 1 or items[0].findtext(ns + 'version') != version or len(items[0].findall('enclosure')) != 1:
        raise ValueError('The signed feed must select the reviewed version.')
    enclosure = items[0].find('enclosure')
    url = enclosure.get('url', '')
    name = url.rsplit('/', 1)[-1]
    if url != DOWNLOAD + 'v' + version + '/' + name or recognize(name, version) != ('dmg', 'macOS'):
        raise ValueError('The enclosure must be the versioned public macOS DMG.')
    length = int(enclosure.get('length', '0'))
    if not 0 < length <= 512 * 1024 * 1024:
        raise ValueError('Invalid public DMG length.')
    with tempfile.TemporaryDirectory() as temp:
        path = Path(temp) / name
        get(url, length, destination=path)
        with path.open('rb') as stream:
            if path.stat().st_size != length or hashlib.file_digest(stream, 'sha256').hexdigest() != dmg_hash:
                raise ValueError('Public DMG differs from the qualified enclosure.')
    return dict(version=version, appcast_sha256=feed_hash, dmg_sha256=dmg_hash, dmg=name, public_bytes='verified')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('version', 'appcast-sha256', 'dmg-sha256'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(check(args.version, args.appcast_sha256, args.dmg_sha256)))
    except (ValueError, OSError, ET.ParseError):
        parser.exit(2, 'Public appcast and DMG could not be matched to the reviewed release.\n')
