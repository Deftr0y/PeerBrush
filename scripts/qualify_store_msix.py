"""Check local Store candidate bytes without authentication, installation or upload."""
import argparse
import json
from pathlib import Path
import re
import sys
import tempfile

from package import git, regular_file
from package_macos_dmg import bounded_file, private_inspection_directory
from production_readiness import handoff_path, unique_fields
import submit_store_release as store


def selected(path, checksum, limit):
    path = handoff_path(path)
    path = regular_file(path.parent, path.name)
    if (not isinstance(checksum, str) or not re.fullmatch('[0-9a-f]{64}', checksum)
            or path.stat().st_size > limit or path.stat().st_size <= 0):
        raise ValueError('Expected bounded regular candidate bytes and an independent SHA-256.')
    with tempfile.TemporaryDirectory(prefix='peerbrush-store-selected-') as temporary:
        copied = Path(temporary) / 'input'
        actual, size = bounded_file(path, limit, copied)
        if size <= 0 or actual != checksum:
            raise ValueError('Candidate input differs from its independent SHA-256.')
        data = copied.read_bytes()
    return path, data


def qualify(root, portable, package, report, *, commit, version, previous_version,
            portable_sha256, package_sha256, report_sha256):
    """Reports/reconstructed payloads must agree; a producer report grants no SDK authority."""
    if (type(commit) is not str or not re.fullmatch('[0-9a-f]{40}', commit)
            or type(version) is not str
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version)):
        raise ValueError('Store preflight requires an exact source commit and stable application version.')
    _, target = store.release_version(version)
    store.version(previous_version)
    store.msix.package_version(version, previous=previous_version)
    portable, portable_data = selected(portable, portable_sha256, store.msix.PACKAGE_LIMIT)
    package, package_data = selected(package, package_sha256, store.msix.PACKAGE_LIMIT)
    report, report_data = selected(report, report_sha256, 64 * 1024)
    if (portable.name != f'PeerBrush-{version}-Windows-x64.zip'
            or package.name != f'PeerBrush-{target}-Windows-x64.msix'
            or report.name != package.name + '.json'):
        raise ValueError('Store preflight filenames differ from the selected release.')
    evidence = json.loads(report_data, object_pairs_hook=unique_fields)
    # Freeze the portable input before the shared inspector reopens it. The
    # package/report have already been read once and are validated from those bytes.
    with tempfile.TemporaryDirectory(prefix='peerbrush-store-preflight-') as temporary:
        copied = Path(temporary) / portable.name
        copied.write_bytes(portable_data)
        store.validate_package(package.name, package_data, evidence,
            f'{package_sha256}  {package.name}\n', commit, version, target,
            source_root=root, portable_path=copied,
            portable_sha256=portable_sha256, msix_sha256=package_sha256)
    if (git(root, 'rev-parse', 'HEAD').decode().strip() != commit
            or git(root, 'status', '--porcelain', '--untracked-files=no').strip()
            or any(bounded_file(path, store.msix.PACKAGE_LIMIT)[0] != checksum for path, checksum in
                   [(portable, portable_sha256), (package, package_sha256), (report, report_sha256)])):
        raise ValueError('Selected source or candidate input changed during preflight.')
    return dict(schema=1, format='PeerBrushStoreMsixQualificationV1',
                commit=commit, version=version, package_version=target,
                package=dict(file=package.name, bytes=len(package_data), sha256=package_sha256),
                portable_sha256=portable_sha256, producer_report_sha256=report_sha256,
                previous_version=previous_version, channel='microsoft-store',
                package_family_name=store.FAMILY, content_verified=True,
                checks=['exact-source-and-embedded-build', 'source-derived-manifest-and-payload',
                        'complete-byte-and-block-hash-verification', 'configured-store-identity-and-family',
                        'strict-producer-report-consistency', 'version-greater-than-selected-previous'],
                native_sdk_validation='not-performed-by-consumer',
                native_producer_authentication='unverified-requires-trusted-native-job',
                previous_version_authority='operator-selected-not-service-reconciled',
                installed_acceptance='not-performed', signing='unsigned-store-submission-input',
                store_submission='not-performed', public_availability='unverified',
                ready=False, production_ready=False)


def write_report(result, output, root):
    output = private_inspection_directory(output, root, fresh=True)
    data = (json.dumps(result, indent=2, sort_keys=True) + '\n').encode('utf-8')
    if len(data) > 8192:
        raise ValueError('Store qualification exceeds its public report budget.')
    output.mkdir(mode=0o700)
    private_inspection_directory(output, root, fresh=False)
    with (output / 'store-msix-qualification.json').open('xb') as stream:
        stream.write(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'portable', 'package', 'report', 'output']:
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ['commit', 'version', 'previous-version', 'portable-sha256', 'package-sha256', 'report-sha256']:
        parser.add_argument('--' + name, required=True)
    args = vars(parser.parse_args())
    root, output = args.pop('source_root'), args.pop('output')
    try:
        # Fail on an existing/unsafe output before examining any input.
        private_inspection_directory(output, root, fresh=True)
        result = qualify(root, **args)
        write_report(result, output, root)
    except Exception:
        print('Store candidate preflight failed; no authentication, installation or upload performed.', file=sys.stderr)
        return 1
    print('Store candidate content verified; native authority, installed acceptance and public availability remain unverified.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
