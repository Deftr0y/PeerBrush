"""Prepare an unsigned, unpublished Ubuntu noble amd64 APT repository candidate.

Inspect the selected DEB/portable/source before generating indexes. No key access,
signing, installation, system configuration, repository deployment or publication.
"""
import argparse
import datetime
import email.utils
import gzip
import hashlib
import pathlib
import re

from package import regular_file
import package_deb as deb
from prepare_site_release import encode, output_path, sha, STABLE
from production_readiness import handoff_path


SUITE = 'noble'
DIST = f'dists/{SUITE}/'
POOL = 'pool/main/p/peerbrush/'
INDEX = 'main/binary-amd64/Packages'
BY_HASH = 'main/binary-amd64/by-hash/SHA256/'
MAX_VALIDITY = datetime.timedelta(days=14)


def timestamp(value):
    if not isinstance(value, str) or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z', value):
        raise ValueError('Select canonical UTC repository dates.')
    try:
        return datetime.datetime.strptime(value, '%Y-%m-%dT%H:%M:%SZ').replace(tzinfo=datetime.timezone.utc)
    except ValueError:
        raise ValueError('Select valid UTC repository dates.') from None


def validity(date, valid_until):
    start, end = timestamp(date), timestamp(valid_until)
    if not datetime.timedelta(0) < end - start <= MAX_VALIDITY:
        raise ValueError('Repository validity must be positive and at most fourteen days.')
    return start, end


def indexes(checked, date, valid_until):
    start, end = validity(date, valid_until)
    record = checked['record']
    filename = POOL + record['file']
    packages = (checked['control'] + f'Filename: {filename}\nSize: {record["bytes"]}\n'
                f'SHA256: {record["sha256"]}\n\n').encode('utf-8')
    compressed = bytearray(gzip.compress(packages, mtime=0))
    # Python 3.11/3.12 may copy zlib's host-specific OS byte with mtime=0.
    compressed[9] = 255
    files = {INDEX: packages, INDEX + '.gz': bytes(compressed)}
    release = ('Origin: PeerBrush\nLabel: PeerBrush\nSuite: noble\nCodename: noble\n'
               f'Date: {email.utils.format_datetime(start, usegmt=True)}\n'
               f'Valid-Until: {email.utils.format_datetime(end, usegmt=True)}\n'
               'Architectures: amd64\nComponents: main\nAcquire-By-Hash: yes\n'
               'Description: PeerBrush for Ubuntu 24.04 amd64\nSHA256:\n')
    for name, data in files.items():
        release += f' {sha(data)} {len(data)} {name}\n'
    files.update({BY_HASH + sha(data): data for data in files.values()})
    files['Release'] = release.encode('utf-8')
    return filename, {DIST + name: data for name, data in files.items()}


def copy_package(source, target, record):
    """Bound the copy to the independently inspected size and recheck its hash."""
    digest = hashlib.sha256()
    remaining = record['bytes']
    with source.open('rb') as incoming, target.open('xb') as outgoing:
        while remaining:
            block = incoming.read(min(1024 * 1024, remaining))
            if not block:
                raise ValueError('Selected DEB was truncated during repository preparation.')
            outgoing.write(block)
            digest.update(block)
            remaining -= len(block)
        if incoming.read(1) or digest.hexdigest() != record['sha256']:
            raise ValueError('Selected DEB changed during repository preparation.')


def plan(root, portable, package, *, commit, version, portable_sha256,
         package_sha256, maintainer, date, valid_until):
    if not isinstance(version, str) or not re.fullmatch(STABLE, version):
        raise ValueError('The stable APT channel requires an exact stable application version.')
    validity(date, valid_until)
    portable, package = handoff_path(portable), handoff_path(package)
    checked = deb.inspect_final(root, portable, package, commit=commit, version=version,
        portable_sha256=portable_sha256, package_sha256=package_sha256, maintainer=maintainer)
    filename, files = indexes(checked, date, valid_until)
    report = dict(schema=1, format='PeerBrushAptRepositoryV1', version=version, commit=commit,
        suite=SUITE, component='main', architecture='amd64', date=date, valid_until=valid_until,
        package=checked['record'], portable_sha256=checked['portable_sha256'],
        executable_sha256=checked['executable_sha256'], content_checks=checked['content_checks'],
        signing='not-performed', public_availability='unverified', release_ready=False,
        installed_acceptance='not-performed', repository_update_acceptance='not-performed',
        by_hash_retention='current-candidate-only',
        files={name: dict(bytes=len(data), sha256=sha(data)) for name, data in files.items()})
    report['files'][filename] = dict(bytes=checked['record']['bytes'], sha256=package_sha256)
    return package, filename, files, report


def prepare(root, portable, package, output, *, commit, version, portable_sha256,
            package_sha256, maintainer, date, valid_until):
    output = output_path(root, pathlib.Path(output))
    package, filename, files, report = plan(root, portable, package, commit=commit, version=version,
        portable_sha256=portable_sha256, package_sha256=package_sha256, maintainer=maintainer,
        date=date, valid_until=valid_until)
    output = output_path(root, output)
    output.mkdir(mode=0o700)
    repository = output / 'repository'
    repository.mkdir(mode=0o700)
    for name, data in files.items():
        target = repository / name
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open('xb') as stream:
            stream.write(data)
    target = repository / filename
    target.parent.mkdir(parents=True, exist_ok=True)
    copy_package(regular_file(package.parent, package.name), target, report['package'])
    with (output / 'apt-repository.json').open('xb') as stream:
        stream.write(encode(report))
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['portable', 'package', 'output']:
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    for name in ['commit', 'version', 'portable-sha256', 'package-sha256', 'maintainer', 'date', 'valid-until']:
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args(argv)
    root = pathlib.Path(__file__).resolve().parents[1]
    prepare(root, args.portable, args.package, args.output, commit=args.commit, version=args.version,
        portable_sha256=args.portable_sha256, package_sha256=args.package_sha256,
        maintainer=args.maintainer, date=args.date, valid_until=args.valid_until)
    print('Unsigned APT repository candidate prepared; signing, trust/bootstrap, native acceptance and publication remain pending.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
