"""Attach verified, externally produced APT signatures to a fresh candidate.

Produce client setup files without private key access, signing, installation,
system trust changes, repository publishing or source mutation.
"""
import argparse
import datetime
import json
import os
import pathlib
import re
import stat
import tempfile
from urllib.parse import urlsplit

from final_release import read
from package_macos_dmg import bounded_file
from package import release_version_order
import prepare_apt_repository as apt
from prepare_site_release import encode, output_path, sha
from production_readiness import handoff_path, unique_fields
import verify_apt_signatures as signatures


SIGNED_FORMATS = {'PeerBrushSignedAptRepositoryV2', 'PeerBrushSignedAptRepositoryV3'}
MAX_SIGNED_RECEIPT_BYTES = 512 * 1024


def current_time():
    return datetime.datetime.now(datetime.timezone.utc)


def repository_origin(uri):
    # Accept only a plain HTTPS origin and unescaped directory path in Deb822.
    if not isinstance(uri, str) or len(uri) > 2048 or not re.fullmatch(
            r'https://[a-z0-9.-]+(?:/[A-Za-z0-9_-][A-Za-z0-9._~-]*)*/?', uri):
        raise ValueError('Select a canonical HTTPS APT repository URI without credentials, port or parameters.')
    origin = urlsplit(uri)
    host = origin.netloc
    labels = host.split('.')
    if (len(host) > 253 or len(labels) < 2 or not labels[-1][:1].isalpha()
            or any(not re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', label) for label in labels)):
        raise ValueError('Select a canonical DNS name for the HTTPS APT repository.')
    return origin


def client_sources(uri, plan, fingerprint):
    repository_origin(uri)
    return (f'Types: deb\nURIs: {uri}\nSuites: {plan["suite"]}\nComponents: {plan["component"]}\n'
            f'Architectures: {plan["architecture"]}\n'
            f'Signed-By: /etc/apt/keyrings/{signatures.KEYRING_NAME} {fingerprint}\n'
            'Check-Date: yes\nCheck-Valid-Until: yes\n').encode('ascii')


def verify_tree(repository, records):
    """Reject extra entries/links before comparing every bounded file to its plan."""
    directories = {''}
    for name in records:
        directories.update(str(parent) for parent in pathlib.PurePosixPath(name).parents if str(parent) != '.')
    pending, found = [repository], set()
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                path = pathlib.Path(entry.path)
                name = path.relative_to(repository).as_posix()
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & 0x400:
                    raise ValueError('APT repository inputs cannot contain links or reparse points.')
                if stat.S_ISDIR(info.st_mode) and name in directories:
                    pending.append(path)
                elif stat.S_ISREG(info.st_mode) and name in records:
                    found.add(name)
                else:
                    raise ValueError('APT repository inventory differs from the inspected plan.')
    if found != set(records):
        raise ValueError('APT repository inventory is incomplete.')
    for name, record in records.items():
        actual, size = bounded_file(repository / name, record['bytes'])
        if (actual, size) != (record['sha256'], record['bytes']):
            raise ValueError('APT repository bytes differ from the inspected plan.')


def signed_receipt(path, digest):
    if not isinstance(digest, str) or not re.fullmatch('[0-9a-f]{64}', digest):
        raise ValueError('Select an independently authenticated signed APT candidate receipt digest.')
    report = json.loads(read(path, MAX_SIGNED_RECEIPT_BYTES, digest), object_pairs_hook=unique_fields)
    if (not isinstance(report, dict) or not isinstance(report.get('format'), str)
            or report['format'] not in SIGNED_FORMATS or report.get('signing') != 'openpgp-verified'
            or (report.get('suite'), report.get('component'), report.get('architecture')) != (apt.SUITE, 'main', 'amd64')
            or not isinstance(report.get('bootstrap'), dict)):
        raise ValueError('Select a signed PeerBrush APT candidate receipt.')
    version = report.get('version')
    if not isinstance(version, str) or len(version) > 128 or not re.fullmatch(apt.STABLE, version):
        raise ValueError('Select a signed APT candidate with a stable package version.')
    mutable = {apt.DIST + name: limit * 1024 for name, limit in (
        ('Release', 64), ('InRelease', 64), ('Release.gpg', 16), (apt.INDEX, 128), (apt.INDEX + '.gz', 128))}
    inventory = report.get('files')
    if not isinstance(inventory, dict) or not mutable.keys() <= inventory.keys():
        raise ValueError('The signed APT candidate inventory is incomplete.')
    hash_prefix = apt.DIST + apt.BY_HASH
    hash_pattern = re.escape(hash_prefix) + '[0-9a-f]{64}'
    pool_pattern = re.escape(apt.POOL) + r'peerbrush_' + apt.STABLE + r'-1_amd64\.deb'
    immutable = {}
    for name, record in inventory.items():
        if name in mutable:
            validate_record(record, mutable[name])
            continue
        if re.fullmatch(hash_pattern, name):
            validate_record(record, 128 * 1024)
            if name != hash_prefix + record['sha256']:
                raise ValueError('An APT by-hash path differs from its recorded digest.')
        elif re.fullmatch(pool_pattern, name):
            validate_record(record, apt.deb.MAX_DEB_BYTES)
        else:
            raise ValueError('The signed APT inventory contains an unexpected repository path.')
        immutable[name] = record
    if (apt.POOL + apt.deb.deb_filename(version) not in immutable
            or any(immutable.get(hash_prefix + inventory[apt.DIST + index]['sha256']) != inventory[apt.DIST + index]
                   for index in (apt.INDEX, apt.INDEX + '.gz'))):
        raise ValueError('The signed APT candidate is missing its immutable indexes or package.')
    return report, immutable


def validate_record(record, limit):
    if (not isinstance(record, dict) or record.keys() != {'bytes', 'sha256'}
            or type(record['bytes']) is not int or not 0 < record['bytes'] <= limit
            or not isinstance(record['sha256'], str) or not re.fullmatch('[0-9a-f]{64}', record['sha256'])):
        raise ValueError('The APT receipt has invalid file metadata.')


def check_predecessor(prior, current, uri):
    if prior['bootstrap'].get('repository_uri') != uri:
        raise ValueError('Previous APT receipt must describe the same signed repository.')
    date, _ = apt.validity(prior.get('date'), prior.get('valid_until'))
    if (release_version_order(prior['version']) > release_version_order(current['version'])
            or date >= apt.timestamp(current['date'])):
        raise ValueError('APT updates require newer metadata and a nondecreasing package version.')


def check_client_trust(prior, current):
    def client_trust(report):
        verification = report.get('signature_verification')
        files = report['bootstrap'].get('files')
        fingerprint = verification.get('primary_fingerprint') if isinstance(verification, dict) else None
        keyring = files.get('bootstrap/' + signatures.KEYRING_NAME) if isinstance(files, dict) else None
        if not isinstance(fingerprint, str) or not re.fullmatch(signatures.FINGERPRINT, fingerprint):
            raise ValueError('APT update history lacks a verified client signing fingerprint.')
        validate_record(keyring, signatures.OUTPUT_LIMIT)
        return fingerprint, keyring

    if client_trust(prior) != client_trust(current):
        raise ValueError('APT updates require the same approved client fingerprint and keyring.')


def predecessor(directory, digest, output, current, uri):
    directory = handoff_path(directory)
    if output.is_relative_to(directory) or directory.is_relative_to(output):
        raise ValueError('Signed output must not overlap the previous candidate.')
    prior, retained = signed_receipt(directory / 'apt-repository.json', digest)
    check_predecessor(prior, current, uri)
    return prior, handoff_path(directory / 'repository'), retained


def attach(root, portable, package, unsigned, output, *, commit, version, portable_sha256,
           package_sha256, maintainer, unsigned_sha256, inrelease, inrelease_sha256,
           detached, detached_sha256, public_key, public_key_sha256, fingerprint, tool, tool_sha256,
           repository_uri, previous=None):
    for value in [unsigned_sha256, inrelease_sha256, detached_sha256, public_key_sha256, tool_sha256]:
        if not isinstance(value, str) or not re.fullmatch('[0-9a-f]{64}', value):
            raise ValueError('Select independent SHA-256 hashes for repository, signatures, public key and verifier.')
    output = output_path(root, pathlib.Path(output))
    unsigned = handoff_path(unsigned)
    if output.is_relative_to(unsigned) or unsigned.is_relative_to(output):
        raise ValueError('Signed output must not overlap the immutable unsigned candidate.')
    raw = read(unsigned / 'apt-repository.json', 32 * 1024, unsigned_sha256)
    selected = json.loads(raw, object_pairs_hook=unique_fields)
    if not isinstance(selected, dict):
        raise ValueError('Select an unsigned APT repository manifest.')
    package, filename, files, expected = apt.plan(root, portable, package,
        commit=commit, version=version, portable_sha256=portable_sha256,
        package_sha256=package_sha256, maintainer=maintainer,
        date=selected.get('date'), valid_until=selected.get('valid_until'))
    if raw != encode(expected):
        raise ValueError('Unsigned repository manifest differs from the actual inspected package/source plan.')
    if not isinstance(fingerprint, str) or not re.fullmatch(signatures.FINGERPRINT, fingerprint):
        raise ValueError('Select the full approved uppercase public-key fingerprint.')
    sources = client_sources(repository_uri, expected, fingerprint)
    repository = handoff_path(unsigned / 'repository')
    verify_tree(repository, expected['files'])
    retained = {}
    if previous is not None:
        prior, prior_repository, retained = predecessor(*previous, output, expected, repository_uri)
        for name in retained.keys() & expected['files'].keys():
            if retained[name] != expected['files'][name]:
                raise ValueError('An APT update would replace an immutable package or index URL.')
            del retained[name]
    release = files[apt.DIST + 'Release']
    inline = read(inrelease, 64 * 1024, inrelease_sha256)
    signature = read(detached, 16 * 1024, detached_sha256)
    key = read(public_key, 256 * 1024, public_key_sha256)
    start, end = apt.validity(expected['date'], expected['valid_until'])
    temporary_root = pathlib.Path(tempfile.gettempdir()).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='peerbrush-apt-signatures-', dir=temporary_root) as temporary:
        verified, keyring = signatures.verify(release, inline, signature, key,
            tool=tool, tool_sha256=tool_sha256, fingerprint=fingerprint,
            work=pathlib.Path(temporary), start=start, end=end, now=current_time())
    if not start <= current_time() < end:
        raise ValueError('Repository metadata expired before signature attachment.')
    # Recheck the selected tree after native verification, before creating output.
    verify_tree(repository, expected['files'])
    files.update({apt.DIST + 'InRelease': inline, apt.DIST + 'Release.gpg': signature})
    bootstrap = {'peerbrush.sources': sources, signatures.KEYRING_NAME: keyring}
    report = dict(expected, format='PeerBrushSignedAptRepositoryV3', signing='openpgp-verified',
        unsigned_manifest_sha256=unsigned_sha256, public_key_sha256=public_key_sha256,
        bootstrap=dict(repository_uri=repository_uri, files={
            f'bootstrap/{name}': dict(bytes=len(data), sha256=sha(data)) for name, data in bootstrap.items()}),
        signature_verification=verified, files={**retained, **expected['files'], **{
            name: dict(bytes=len(data), sha256=sha(data)) for name, data in files.items()}})
    if previous is not None:
        check_client_trust(prior, report)
        report.update(by_hash_retention='previous-candidate-chain', predecessor_manifest_sha256=previous[1])
    receipt = encode(report)
    if len(receipt) > MAX_SIGNED_RECEIPT_BYTES:
        raise ValueError('The signed APT candidate exceeds its bounded inventory budget.')
    output = output_path(root, output)
    output.mkdir(mode=0o700)
    destination = output / 'repository'
    destination.mkdir(mode=0o700)
    for name, data in files.items():
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open('xb') as stream:
            stream.write(data)
    target = destination / filename
    target.parent.mkdir(parents=True, exist_ok=True)
    apt.copy_package(package, target, expected['package'])
    for name, record in retained.items():
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        actual = bounded_file(prior_repository / name, record['bytes'], destination=target)
        if actual != (record['sha256'], record['bytes']):
            raise ValueError('A retained APT object differs from the selected predecessor receipt.')
    (output / 'bootstrap').mkdir(mode=0o700)
    for name, data in bootstrap.items():
        with (output / 'bootstrap' / name).open('xb') as stream:
            stream.write(data)
    if not start <= current_time() < end:
        raise ValueError('Repository metadata expired while constructing the signed candidate.')
    with (output / 'apt-repository.json').open('xb') as stream:
        stream.write(receipt)
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['portable', 'package', 'unsigned', 'output', 'inrelease', 'detached', 'public-key', 'tool']:
        parser.add_argument('--' + name, required=True, type=pathlib.Path)
    for name in ['commit', 'version', 'portable-sha256', 'package-sha256', 'maintainer', 'unsigned-sha256',
                 'inrelease-sha256', 'detached-sha256', 'public-key-sha256', 'fingerprint', 'tool-sha256',
                 'repository-uri']:
        parser.add_argument('--' + name, required=True)
    history = parser.add_mutually_exclusive_group(required=True)
    history.add_argument('--initial', action='store_true', help='The repository has never been published.')
    history.add_argument('--previous', nargs=2, metavar=('CANDIDATE', 'SHA256'),
        help='The exact last published candidate and its independently authenticated receipt digest.')
    args = parser.parse_args(argv)
    root = pathlib.Path(__file__).resolve().parents[1]
    attach(root, args.portable, args.package, args.unsigned, args.output, commit=args.commit,
        version=args.version, portable_sha256=args.portable_sha256, package_sha256=args.package_sha256,
        maintainer=args.maintainer, unsigned_sha256=args.unsigned_sha256, inrelease=args.inrelease,
        inrelease_sha256=args.inrelease_sha256, detached=args.detached, detached_sha256=args.detached_sha256,
        public_key=args.public_key, public_key_sha256=args.public_key_sha256,
        fingerprint=args.fingerprint, tool=args.tool, tool_sha256=args.tool_sha256,
        repository_uri=args.repository_uri,
        previous=(pathlib.Path(args.previous[0]), args.previous[1]) if args.previous else None)
    print('Verified APT candidate and client setup files prepared. Publisher approval, native acceptance and publication remain pending.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
