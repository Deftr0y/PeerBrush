"""Verify an APT publication stage and optionally read back its public bytes."""
import argparse
import pathlib
import re

from attach_apt_signatures import (check_client_trust, check_predecessor, client_sources, current_time, repository_origin,
                                   signed_receipt, validate_record, verify_tree)
import prepare_apt_repository as apt
from prepare_site_release import encode, sha
from production_readiness import handoff_path
import readback_apt_repository as public
import verify_apt_signatures as signatures


def https_scope(report, bootstrap_uri):
    repository_uri = report['bootstrap']['repository_uri']
    repository_origin(repository_uri)
    repository_origin(bootstrap_uri)
    return dict(repository_uri=repository_uri, public_bootstrap_uri=bootstrap_uri,
                inrelease_sha256=report['files'][apt.DIST + 'InRelease']['sha256'])


def verify_https_scope(report, scope):
    if not isinstance(scope, dict) or scope != https_scope(report, scope.get('public_bootstrap_uri')):
        raise ValueError('Native APT HTTPS evidence does not match the signed candidate.')


def package_binding(report):
    version = report['version']
    filename = apt.deb.deb_filename(version)
    package_path = apt.POOL + filename
    record = report['files'][package_path]
    package = report.get('package')
    if (not isinstance(package, dict) or package.get('file') != filename
            or package.get('binary_source_commit') != report.get('commit')
            or {field: package.get(field) for field in ('bytes', 'sha256')} != record):
        raise ValueError('Signed APT package and repository inventory differ.')
    expected = dict(version=version, commit=report.get('commit'), package_sha256=record['sha256'],
                    executable_sha256=report.get('executable_sha256'), source_sha256=package.get('source_sha256'))
    for key, length in (('commit', 40), ('package_sha256', 64), ('executable_sha256', 64), ('source_sha256', 64)):
        value = expected[key]
        if not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{%d}' % length, value):
            raise ValueError('Signed APT package lacks exact executable and corresponding source bindings.')
    return package_path, record, expected


def check(stage, receipt, digest, *, repository_uri, fingerprint, previous=None, public_bootstrap_uri=None):
    stage = handoff_path(stage)
    report, immutable = signed_receipt(receipt, digest)
    package_path, _, package = package_binding(report)
    verified = report.get('signature_verification')
    if (not isinstance(fingerprint, str) or not re.fullmatch(signatures.FINGERPRINT, fingerprint)
            or not isinstance(verified, dict) or verified.get('primary_fingerprint') != fingerprint
            or report['bootstrap'].get('repository_uri') != repository_uri):
        raise ValueError('The signed receipt differs from the approved APT URI or primary fingerprint.')
    start, end = apt.validity(report.get('date'), report.get('valid_until'))
    if not start <= current_time() < end:
        raise ValueError('The selected APT metadata is not currently valid.')
    active = {package_path}
    active.update(apt.DIST + apt.BY_HASH + report['files'][apt.DIST + name]['sha256']
                  for name in (apt.INDEX, apt.INDEX + '.gz'))
    if previous is None:
        if report.get('by_hash_retention') != 'current-candidate-only' or 'predecessor_manifest_sha256' in report:
            raise ValueError('This APT update requires an independently selected predecessor receipt.')
    else:
        path, previous_digest = previous
        prior, inherited = signed_receipt(path, previous_digest)
        check_predecessor(prior, report, repository_uri)
        check_client_trust(prior, report)
        if (report.get('by_hash_retention') != 'previous-candidate-chain'
                or report.get('predecessor_manifest_sha256') != previous_digest
                or any(immutable.get(name) != record for name, record in inherited.items())):
            raise ValueError('The selected APT update would break previously published immutable URLs.')
        active.update(inherited)
    if active != immutable.keys():
        raise ValueError('The staged APT inventory contains unaccounted historical objects.')

    keyring = 'bootstrap/' + signatures.KEYRING_NAME
    source = 'bootstrap/peerbrush.sources'
    bootstrap = report['bootstrap'].get('files')
    if not isinstance(bootstrap, dict) or bootstrap.keys() != {keyring, source}:
        raise ValueError('The signed APT client setup inventory is incomplete.')
    validate_record(bootstrap[keyring], signatures.OUTPUT_LIMIT)
    sources = client_sources(repository_uri, report, fingerprint)
    if bootstrap[source] != dict(bytes=len(sources), sha256=sha(sources)):
        raise ValueError('The signed APT client entry differs from the approved configuration.')
    verify_tree(stage, {**bootstrap, **{f'repository/{name}': record for name, record in report['files'].items()}})
    if not start <= current_time() < end:
        raise ValueError('The selected APT metadata expired during stage verification.')
    result = dict(format='PeerBrushAptStageReadbackV1', candidate_receipt_sha256=digest,
        predecessor_receipt_sha256=previous[1] if previous else None,
        repository_uri=repository_uri, primary_fingerprint=fingerprint, version=report['version'],
        source_commit=package['commit'], package_file=package_path.rsplit('/', 1)[1],
        package_sha256=package['package_sha256'],
        source_archive_sha256=package['source_sha256'],
        date=report['date'], valid_until=report['valid_until'],
        repository_files=len(report['files']), bootstrap_files=len(bootstrap),
        local_stage='verified', public_availability='unverified', release_ready=False)
    if public_bootstrap_uri is not None:
        public.check(repository_uri, public_bootstrap_uri, report['files'], bootstrap)
        if not start <= current_time() < end:
            raise ValueError('The selected APT metadata expired during public readback.')
        result.update(https_scope(report, public_bootstrap_uri), public_availability='exact-bytes-verified')
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', required=True, type=pathlib.Path)
    parser.add_argument('--receipt', required=True, type=pathlib.Path)
    parser.add_argument('--receipt-sha256', required=True)
    parser.add_argument('--repository-uri', required=True)
    parser.add_argument('--fingerprint', required=True)
    parser.add_argument('--public-bootstrap-uri',
        help='Approved public HTTPS client-setup base. Also read back the public repository.')
    history = parser.add_mutually_exclusive_group(required=True)
    history.add_argument('--initial', action='store_true', help='The repository has never been published.')
    history.add_argument('--previous', nargs=2, metavar=('RECEIPT', 'SHA256'),
        help='The last published receipt and its independently authenticated digest.')
    args = parser.parse_args(argv)
    result = check(args.stage, args.receipt, args.receipt_sha256, repository_uri=args.repository_uri,
                   fingerprint=args.fingerprint, previous=args.previous, public_bootstrap_uri=args.public_bootstrap_uri)
    print(encode(result).decode('utf-8'), end='')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
