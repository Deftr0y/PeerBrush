"""Prepare unpublished Scoop/Homebrew definitions from verified public release bytes.

No catalog submission, installation, upgrade, signing or publication occurs here.
APT repository delivery and a reproducible Nix source build remain separate work.
"""
import argparse
import json
import pathlib
import plistlib
import re
import zipfile

from final_release import LIMIT, read
from package import regular_file, relative_path
from package_macos_dmg import SUPPORT as DMG_SUPPORT, bounded_file
import prepare_site_release as release
from production_readiness import unique_fields
from verify_release import MACOS_SIGNING_KIND


CHANNELS = ('scoop', 'homebrew')
LICENSE_FILES = ('LICENSE', 'THIRD_PARTY_NOTICES.md')


def require_support_inventory(asset):
    hashes = asset.get('support_files')
    if (not isinstance(hashes, dict) or not set(LICENSE_FILES) < hashes.keys()
            or len(hashes) > 4096 or len({name.casefold() for name in hashes}) != len(hashes)):
        raise ValueError('Package definition requires the exact license and notice inventory.')
    for name, checksum in hashes.items():
        if ((name not in LICENSE_FILES and not name.startswith('licenses/'))
                or not isinstance(checksum, str) or not re.fullmatch(r'[0-9a-f]{64}', checksum)):
            raise ValueError('Package definition has an invalid license or notice binding.')
        relative_path(name)


def verify_installed_support(directory, version, asset):
    hashes = dict(asset['support_files'])
    hashes[f'PeerBrush-{version}-source.zip'] = asset['source_sha256']
    for name, checksum in hashes.items():
        if bounded_file(regular_file(directory, name), LIMIT)[0] != checksum:
            raise ValueError('Installed corresponding source or notices differ from the release.')


def definition(channel, path, inspected, url):
    """Called only with the public verifier's actual inspected archive, not a receipt."""
    manifest = inspected['manifest']
    version = manifest['version']
    prefix = inspected['prefix'].removesuffix('/')
    checksum = inspected['record']['sha256']
    if channel == 'scoop':
        if manifest['platform'] != 'Windows' or manifest['architecture'] != 'x64':
            raise ValueError('Scoop requires the inspected Windows x64 archive.')
        return release.encode({
            'version': version,
            'description': 'Native collaborative image editor',
            'homepage': release.WEBSITE,
            'license': 'GPL-3.0-only',
            'architecture': {'64bit': {'url': url, 'hash': checksum, 'extract_dir': prefix}},
            'bin': 'peerbrush.exe',
            'shortcuts': [['peerbrush.exe', 'PeerBrush']],
        })
    if channel != 'homebrew' or manifest['platform'] != 'macOS' or manifest['architecture'] != 'arm64':
        raise ValueError('Homebrew requires the inspected macOS arm64 archive.')
    if not notarized_candidate(inspected['record']):
        raise ValueError('Homebrew requires the Developer ID notarized release ZIP.')
    with zipfile.ZipFile(path) as archive:
        info = plistlib.loads(archive.read(prefix + '/PeerBrush.app/Contents/Info.plist'))
    minimum = info.get('LSMinimumSystemVersion')
    # shortcut: current bundles declare macOS 11, Homebrew's current floor;
    # revise the dependency stanza when the bundle minimum or Brew floor changes.
    if minimum != '11.0':
        raise ValueError('Bundle macOS minimum changed; review the Homebrew dependency stanza.')
    return homebrew_cask(version, checksum, url)


def notarized_candidate(record):
    return (record.get('native_publisher_signing') == MACOS_SIGNING_KIND
            and record.get('signing_verification') == 'metadata-and-hashes-only')


def homebrew_layout(version, *, dmg):
    prefix = f'PeerBrush-{version}-macOS-arm64'
    return ('PeerBrush.app', DMG_SUPPORT) if dmg else (f'{prefix}/PeerBrush.app', prefix)


def homebrew_cask(version, checksum, url, *, dmg=False):
    app, support = homebrew_layout(version, dmg=dmg)
    # These strings are generated from canonical versions, inspected archive
    # paths and verified public URLs; no arbitrary input becomes Ruby code.
    text = (f'cask "peerbrush" do\n'
            f'  version "{version}"\n'
            f'  sha256 "{checksum}"\n\n'
            f'  url "{url}"\n'
            '  name "PeerBrush"\n'
            '  desc "Native collaborative image editor"\n'
            f'  homepage "{release.WEBSITE}"\n\n'
            '  depends_on arch: :arm64\n'
            '  depends_on :macos\n\n'
            f'  app "{app}"\n'
            '  binary "#{appdir}/PeerBrush.app/Contents/MacOS/peerbrush"\n')
    # Keep the complete GPL source and notice tree even for older app bundles
    # that did not enclose these files inside Contents/Resources.
    for name in (f'PeerBrush-{version}-source.zip', *LICENSE_FILES, 'licenses'):
        text += (f'  artifact "{support}/{name}",\n'
                 f'           target: "#{{HOMEBREW_PREFIX}}/share/peerbrush/{version}/{name}"\n')
    return (text + 'end\n').encode('utf-8')


def prepare(root, inventory, tag, commit, output, channels=CHANNELS, get=release.public_get):
    if (not channels or len(set(channels)) != len(channels)
            or any(channel not in CHANNELS for channel in channels)):
        raise ValueError('Select Scoop and/or Homebrew once each.')
    output = release.output_path(root, pathlib.Path(output))
    files, bindings, withheld = {}, {}, {}

    def consume(path, inspected, url):
        platform = inspected['manifest']['platform']
        channel = {'Windows': 'scoop', 'macOS': 'homebrew'}.get(platform)
        if channel not in channels:
            return
        record = inspected['record']
        if channel == 'homebrew' and not notarized_candidate(record):
            withheld[channel] = 'developer-id-notarized-archive-required'
            return
        name = 'peerbrush.json' if channel == 'scoop' else 'peerbrush.rb'
        files[name] = definition(channel, path, inspected, url)
        binding = dict(file=path.name, sha256=record['sha256'], url=url,
                       executable_sha256=record['executable_sha256'], source_sha256=record['source_sha256'],
                       binary_source_commit=record['binary_source_commit'],
                       native_publisher_signing=record['native_publisher_signing'],
                       signing_verification=record['signing_verification'])
        manifest = inspected['manifest']
        if channel == 'homebrew':
            binding['publisher_team_id'] = manifest['macos_signing']['team_id']
        support = dict(manifest['notice_files'])
        with zipfile.ZipFile(path) as archive:
            for name in LICENSE_FILES:
                support[name] = release.sha(archive.read(inspected['prefix'] + name))
        binding['support_files'] = support
        require_support_inventory(binding)
        bindings[channel] = binding

    def consume_dmg(bound, url):
        if 'homebrew' not in channels:
            return
        companion = bindings.get('homebrew')
        if companion is None or bound['signed_zip'] != {key: companion[key] for key in ('file', 'sha256')}:
            raise ValueError('Homebrew DMG requires the matching inspected signed ZIP.')
        record = bound['record']
        files['peerbrush.rb'] = homebrew_cask(tag[1:], record['sha256'], url, dmg=True)
        bindings['homebrew'] = dict(companion, file=record['file'], sha256=record['sha256'], url=url,
                                   signed_zip=bound['signed_zip'], qualification_sha256=bound['qualification_sha256'])

    entry, _ = release.published(root, inventory, tag, commit,
        dict(notes=['Package-manager definition preparation.'], caution='Unpublished definitions; native acceptance pending.'),
        get, on_portable=consume, on_dmg=consume_dmg)
    report = dict(schema=1, version=tag, commit=commit, release_url=entry['releaseUrl'],
        release_inventory_sha256=entry['releaseInventorySha256'],
        source_url=entry['sourceUrl'], checksums_url=entry['checksumsUrl'],
        published=False, release_ready=False,
        channels={channel: dict(definition_prepared=channel in bindings,
            definition_unavailable_reason=None if channel in bindings else withheld.get(channel, 'verified-archive-unavailable'),
            asset=bindings.get(channel), catalog_publication='not-performed',
            native_publisher_trust='unverified', installed_acceptance='not-performed',
            update_owner_acceptance='not-performed') for channel in channels},
        files={name: release.sha(data) for name, data in files.items()})
    # All transfers and inspectors must succeed before any output is created.
    output = release.output_path(root, output)
    output.mkdir(mode=0o700)
    for name, data in {**files, 'package-managers.json': release.encode(report)}.items():
        with (output / name).open('xb') as stream:
            stream.write(data)
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inventory', type=pathlib.Path, required=True)
    parser.add_argument('--inventory-sha256', required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--channel', choices=CHANNELS, action='append')
    args = parser.parse_args(argv)
    if not re.fullmatch('[0-9a-f]{64}', args.inventory_sha256):
        parser.error('Select the release inventory with its independent SHA-256.')
    inventory = json.loads(read(args.inventory, 128 * 1024, args.inventory_sha256), object_pairs_hook=unique_fields)
    root = pathlib.Path(__file__).resolve().parents[1]
    report = prepare(root, inventory, args.tag, args.commit, args.output, args.channel or CHANNELS)
    print('Unpublished package-manager definitions prepared; installation, update ownership and catalog acceptance remain pending.')
    return 0 if all(row['definition_prepared'] for row in report['channels'].values()) else 2


if __name__ == '__main__':
    raise SystemExit(main())
