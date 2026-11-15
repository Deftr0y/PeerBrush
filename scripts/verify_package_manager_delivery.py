"""Reconcile public delivery, clean install and native upgrade readbacks for all four channels."""
import argparse
import json
from pathlib import Path

from attach_apt_signatures import current_time, repository_origin
from final_release import read
from prepare_apt_repository import validity
from prepare_site_release import STABLE, encode
from production_readiness import unique_fields
from verify_apt_signatures import FINGERPRINT
from verify_package_manager_acceptance import (DESKTOP, DESKTOP_CONNECT, FORMATS, HEX, NAR_HASH, SHELL_DESKTOP,
                                               matches, require_fields)


COMMIT = r'[0-9a-f]{40}'


def receipt(path, checksum):
    if not matches(HEX, checksum):
        raise ValueError('Select an independently authenticated readback SHA-256.')
    report = json.loads(read(Path(path), 128 * 1024, checksum), object_pairs_hook=unique_fields)
    if not isinstance(report, dict):
        raise ValueError('Package delivery readback must be an object.')
    return report


def catalog_hashes(public, channel, version, artifact_file):
    require_fields(public, dict(schema=1, format='PeerBrushPackageCatalogReadbackV1', channel=channel,
        catalog_publication='exact-bytes-and-modes-verified',
        release_downloads_readback='exact-bytes-verified'), 'Public package catalog')
    downloads = public.get('release_downloads')
    source_name = f'PeerBrush-{version}-source.zip'
    expected = {source_name, artifact_file}
    if channel == 'homebrew' and artifact_file.endswith('.dmg'):
        if not matches(r'.+-apple-signed-r[1-9][0-9]*-dmg-signed-r[1-9][0-9]*\.dmg', artifact_file):
            raise ValueError('Public catalog does not identify the signed Homebrew DMG.')
        expected.update({
            artifact_file.rsplit('-dmg-signed-r', 1)[0] + '.zip',
            artifact_file + '.qualification.json',
        })
    if (not matches(COMMIT, public.get('catalog_commit')) or not isinstance(downloads, dict)
            or (artifact_file == source_name) != (channel == 'nix')
            or downloads.keys() != expected
            or any(not isinstance(row, dict) or not matches(HEX, row.get('sha256'))
                   or type(row.get('bytes')) is not int or not 0 < row['bytes'] <= 512 * 1024 * 1024
                   for row in downloads.values())):
        raise ValueError('Public catalog lacks evidence for the exact release download filenames.')
    source = downloads[source_name]
    return downloads[artifact_file]['sha256'], source['sha256'], source['bytes']


def check(entries, *, version, source_commit):
    channels = [entry[0] for entry in entries]
    if (not matches(STABLE, version) or not matches(COMMIT, source_commit)
            or len(channels) != len(FORMATS) or set(channels) != set(FORMATS)):
        raise ValueError('Select one version, one source commit and one evidence set for each of the four channels.')
    selected = dict(version=version, source_commit=source_commit)
    results, source_hashes, source_sizes = {}, set(), set()
    for channel, public_path, public_sha, install_path, install_sha, upgrade_path, upgrade_sha in entries:
        public = receipt(public_path, public_sha)
        installed = receipt(install_path, install_sha)
        native = receipt(upgrade_path, upgrade_sha)
        require_fields(public, selected, 'Public readback')
        native_scope = dict(selected, schema=1, format='PeerBrushPackageManagerNativeReadbackV1',
                            channel=channel, desktop_startup_documents=DESKTOP[channel],
                            public_availability='unverified', release_ready=False)
        require_fields(installed, dict(native_scope, native_lifecycle='install', native_upgrade='not-performed'),
                       'Native clean install readback')
        require_fields(native, dict(native_scope, native_lifecycle='upgrade', native_upgrade='verified'),
                       'Native upgrade readback')
        definition = native.get('definition_receipt_sha256')
        baseline = native.get('previous_definition_receipt_sha256')
        standalone = native.get('standalone_deb_sha256')
        install_receipt, upgrade_receipt = installed.get('native_receipt_sha256'), native.get('native_receipt_sha256')
        if (not all(matches(HEX, value) for value in (definition, install_receipt, upgrade_receipt))
                or install_receipt == upgrade_receipt
                or (standalone is None and (not matches(HEX, baseline) or definition == baseline))
                or (standalone is not None and (channel != 'apt' or baseline is not None or not matches(HEX, standalone)))):
            raise ValueError('Native readback lacks distinct, authenticated upgrade definitions.')
        require_fields(installed, dict(definition_receipt_sha256=definition), 'Native clean install definition')
        artifact_file = native.get('artifact_file')
        if (not matches(r'[A-Za-z0-9][A-Za-z0-9._-]*', artifact_file)
                or installed.get('artifact_file') != artifact_file):
            raise ValueError('Native install and upgrade readbacks must identify the same release download filename.')

        if channel == 'apt':
            fingerprint = native.get('publisher')
            predecessor = native.get('repository_predecessor_sha256')
            if (not matches(FINGERPRINT, fingerprint)
                    or (standalone is None and not matches(HEX, predecessor))
                    or (standalone is not None and predecessor is not None)):
                raise ValueError('Native APT readback lacks the approved signer or repository predecessor.')
            require_fields(public, dict(format='PeerBrushAptStageReadbackV1', candidate_receipt_sha256=definition,
                predecessor_receipt_sha256=predecessor, primary_fingerprint=fingerprint,
                package_file=artifact_file,
                local_stage='verified', public_availability='exact-bytes-verified', release_ready=False),
                'Public APT repository')
            start, end = validity(public.get('date'), public.get('valid_until'))
            if not start <= current_time() < end:
                raise ValueError('Public APT metadata is outside its signed validity window. Reverify the active repository.')
            artifact, source = public.get('package_sha256'), public.get('source_archive_sha256')
            if standalone is not None and standalone == artifact:
                raise ValueError('Standalone DEB baseline must differ from the repository package.')
            require_fields(installed, dict(publisher=fingerprint,
                repository_predecessor_sha256=predecessor), 'APT clean install')
            scope = {key: public.get(key) for key in ('repository_uri', 'public_bootstrap_uri', 'inrelease_sha256')}
            repository_origin(scope['repository_uri'])
            repository_origin(scope['public_bootstrap_uri'])
            if (not matches(HEX, scope['inrelease_sha256'])
                    or any(report.get('apt_https') != scope for report in (installed, native))):
                raise ValueError('APT clean install and upgrade must both use the same signed generation over public HTTPS.')
            https_evidence = dict(install=scope, upgrade=scope)
        else:
            require_fields(public, dict(definition_receipt_sha256=definition), 'Public catalog definition')
            artifact, source, source_size = catalog_hashes(public, channel, version, artifact_file)
            source_sizes.add(source_size)
            if channel == 'scoop':
                scope = dict(shell_desktop=SHELL_DESKTOP, desktop_connect=DESKTOP_CONNECT)
                require_fields(installed, scope, 'Scoop clean install desktop evidence')
                require_fields(native, scope, 'Scoop upgrade desktop evidence')
            elif channel == 'homebrew':
                team = native.get('publisher')
                if (not matches(r'[A-Z0-9]{10}', team) or public.get('publisher_team_id') != team
                        or installed.get('publisher') != team):
                    raise ValueError('Homebrew public and native evidence select different publishers.')
            elif channel == 'nix':
                require_fields(native, dict(native_rollback='verified'), 'Nix rollback')
                output = native.get('nix_output')
                if (not isinstance(output, dict) or output != installed.get('nix_output')
                        or not matches(NAR_HASH, output.get('flake_nar_sha256'))
                        or not matches(NAR_HASH, output.get('nar_sha256'))
                        or not matches(HEX, output.get('closure_sha256'))):
                    raise ValueError('Nix clean install and upgrade must reproduce the same flake, output and closure.')

        if (not matches(HEX, artifact) or not matches(HEX, source)
                or any(report.get('artifact_sha256') != artifact or report.get('source_archive_sha256') != source
                       for report in (installed, native))):
            raise ValueError('Public and native package evidence do not identify the same authenticated artifact and source.')
        source_hashes.add(source)
        results[channel] = dict(public_readback_sha256=public_sha, native_install_readback_sha256=install_sha,
                                native_upgrade_readback_sha256=upgrade_sha, definition_receipt_sha256=definition,
                                artifact_file=artifact_file, artifact_sha256=artifact)
        if channel == 'apt':
            results[channel]['apt_https'] = https_evidence
            if standalone is not None:
                results[channel]['standalone_deb_sha256'] = standalone
        if channel == 'nix':
            results[channel]['nix_output'] = native['nix_output']
    if len(source_hashes) != 1:
        raise ValueError('The package channels select different corresponding source archives.')
    if len(source_sizes) != 1:
        raise ValueError('The public catalogs disagree on the corresponding source archive size.')
    return dict(selected, schema=1, format='PeerBrushPackageManagerDeliveryReadbackV1',
                source_archive_sha256=source_hashes.pop(), channels=results,
                package_manager_delivery='public-and-native-evidence-reconciled', release_ready=False)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--evidence', action='append', nargs=7, required=True,
                        metavar=('CHANNEL', 'PUBLIC', 'PUBLIC_SHA256', 'INSTALL', 'INSTALL_SHA256',
                                 'UPGRADE', 'UPGRADE_SHA256'))
    args = parser.parse_args(argv)
    print(encode(check(args.evidence, version=args.version, source_commit=args.source_commit)).decode('utf-8'), end='')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
