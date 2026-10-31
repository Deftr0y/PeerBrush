"""Closed final-input data bundles; acquisition never grants native authority.

The local producer accepts a selected six-record base, including the fixed
upstream assembler's output. It reuses final composition before retaining data.
Both preparation and consumption remain blocked
for publication and return CLI status 2 after successful byte verification.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import zipfile

import acquire_macos_qualification_inputs as transport
import final_release as final
from package_macos_dmg import bounded_file, private_inspection_directory
import production_readiness as readiness
from verify_macos_qualification import LIMITS

FORMAT = 'PeerBrushFinalInputsV1'
INDEX = 'final-inputs.json'
ARCHIVE = 'final-inputs.zip'
MAX_FILES = 32
MAX_INDEX = 64 * 1024
MAX_TOTAL = transport.MAX_ARTIFACT
WORKFLOW = '.github/workflows/final-inputs.yml'
SELECTION = {'commit', 'version', 'macos', 'windows', 'maintainer',
             'linux_receipt', 'linux_receipt_sha256'}
STORE_SELECTION = SELECTION - {'windows'} | {'store'}


def json_input(path, limit, checksum):
    if not transport.hexadecimal(checksum, 64):
        raise ValueError('An independently selected exact input digest is required.')
    return json.loads(final.read(path, limit, checksum), object_pairs_hook=readiness.unique_fields)


def safe_name(name):
    return (type(name) is str and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,223}', name)
            and not name.endswith('.') and name.split('.')[0].upper() not in
            {'CON', 'PRN', 'AUX', 'NUL', *(f'COM{n}' for n in range(1, 10)), *(f'LPT{n}' for n in range(1, 10))})


def paths(selected):
    """The two concrete native trust callers and existing Windows/Linux roles only."""
    if not isinstance(selected, dict) or set(selected) not in [SELECTION, SELECTION | {'linux_native'},
                                                              STORE_SELECTION, STORE_SELECTION | {'linux_native'}]:
        raise ValueError('Exact existing final selection required; no readiness or credential fields.')
    mac = selected['macos']
    if not isinstance(mac, dict) or set(mac) != {'qualification', 'qualification_sha256', 'team_id', 'trust'}:
        raise ValueError('Exact Mac qualification selection required.')
    store_only = 'store' in selected
    windows = selected['store'] if store_only else selected['windows']
    required = {'unsigned', 'unsigned_sha256', 'package', 'package_sha256', 'runtime', 'runtime_sha256',
                'identity', 'identity_sha256', 'package_family', 'descriptor', 'descriptor_sha256',
                'descriptor_uri', 'package_uri', 'runtime_uri'}
    allowed = [{'package', 'package_sha256', 'report', 'report_sha256', 'previous_version'}] if store_only else [required, required | {'native'}]
    if not isinstance(windows, dict) or set(windows) not in allowed:
        raise ValueError('Exact existing Windows selection required.')
    records = [(('linux_receipt',), 'linux/installation.json', readiness.MAX_INSTALLATION_RECEIPT_BYTES,
                selected['linux_receipt_sha256']),
               (('macos', 'qualification'), 'native/macos/dmg-qualification.json', LIMITS['qualification'],
                mac['qualification_sha256'])]
    windows_roles = [('package', final.LIMIT), ('report', 64 * 1024)] if store_only else [
        ('unsigned', final.LIMIT), ('package', final.LIMIT), ('runtime', final.LIMIT),
        ('descriptor', 16 * 1024), ('identity', 16 * 1024)]
    for key, limit in windows_roles:
        name = Path(windows[key]).name if type(windows[key]) is str else ''
        if not safe_name(name):
            raise ValueError('Selected Windows role requires a bounded canonical filename.')
        records.append(((('store' if store_only else 'windows'), key), 'windows/' + name, limit, windows[key + '_sha256']))
    trusts = [(('macos', 'trust'), 'macos')]
    if not store_only and 'native' in windows:
        from verify_windows_qualification import FIELDS
        native = windows['native']
        if not isinstance(native, dict) or set(native) != FIELDS:
            raise ValueError('Exact existing native Windows fields required.')
        records.append((('windows', 'native', 'qualification'), 'native/windows/windows-installer-qualification.json',
                        16 * 1024, native['qualification_sha256']))
        trusts.append((('windows', 'native', 'trust'), 'windows'))
    if 'linux_native' in selected:
        from verify_linux_qualification import FIELDS, GUI_FIELDS
        native = selected['linux_native']
        if not isinstance(native, dict) or set(native) not in [FIELDS, GUI_FIELDS]:
            raise ValueError('Exact existing native Linux fields required.')
        records.extend([(('linux_native', 'qualification'), 'native/linux/linux-deb-qualification.json',
                         16 * 1024, native['qualification_sha256']),
                        (('linux_native', 'upgrade'), 'native/linux/portable-to-deb.json',
                         readiness.MAX_INSTALLATION_RECEIPT_BYTES, native['upgrade_sha256'])])
        if 'gui' in native:
            records.append((('linux_native', 'gui'), 'native/linux/installed-gui.json',
                            readiness.MAX_INSTALLATION_RECEIPT_BYTES, native['gui_sha256']))
        trusts.append((('linux_native', 'trust'), 'linux'))
    for keys, platform in trusts:
        trust = get(selected, keys)
        if (not isinstance(trust, dict) or set(trust) != readiness.NATIVE_TRUST_FIELDS | {'readback_job'}
                or trust['readback_job'] is not True):
            raise ValueError('Complete existing native trust and exact-job readback are required.')
        for key, filename, limit in [('bundle', 'bundle.jsonl', LIMITS['bundle']),
                                    ('trusted_root', 'trusted_root.jsonl', LIMITS['trusted-root']),
                                    ('cli', 'gh', LIMITS['gh'])]:
            records.append(((*keys, key), 'native/' + platform + '/' + filename, limit, trust[key + '_sha256']))
    names = [record[1] for record in records]
    if (len(set(name.casefold() for name in names)) != len(names)
            or any(not transport.hexadecimal(record[3], 64) for record in records)):
        raise ValueError('Selected role paths/digests collide or are invalid.')
    return records, trusts


def get(value, keys):
    for key in keys:
        value = value[key]
    return value


def put(value, keys, replacement):
    for key in keys[:-1]:
        value = value[key]
    value[keys[-1]] = replacement


def inventory(directory, selected):
    raw = final.read(directory / 'release-inventory.json', MAX_INDEX)
    value = json.loads(raw, object_pairs_hook=readiness.unique_fields)
    from prepare_site_release import validate_inventory
    assets = validate_inventory(value, selected['commit'], selected['version'])
    if len(assets) != 6:
        raise ValueError('An already assembled six-record signed base is required.')
    records = {'base/release-inventory.json': (directory / 'release-inventory.json', MAX_INDEX, hashlib.sha256(raw).hexdigest()),
               'base/SHA256SUMS': (directory / 'SHA256SUMS', MAX_INDEX, None)}
    for name, record in assets.items():
        if not safe_name(name):
            raise ValueError('Base records require bounded selected basenames.')
        records['base/' + name] = (directory / name, final.LIMIT, record['sha256'])
    return records


def checked_index(value, commit, selection_sha256):
    if (not isinstance(value, dict) or set(value) != {'schema', 'format', 'commit', 'selection_sha256', 'files'}
            or type(value['schema']) is not int or value['schema'] != 1 or value['format'] != FORMAT
            or value['commit'] != commit or not transport.hexadecimal(commit, 40)
            or value['selection_sha256'] != selection_sha256 or not transport.hexadecimal(selection_sha256, 64)
            or not isinstance(value['files'], list) or not 1 <= len(value['files']) <= MAX_FILES):
        raise ValueError('Strict independently selected final-input index required.')
    result = {}
    for record in value['files']:
        if (not isinstance(record, dict) or set(record) != {'path', 'bytes', 'sha256'}
                or type(record['path']) is not str or not record['path'].isascii()
                or len(record['path']) > 256 or '\\' in record['path']
                or any(not safe_name(part) for part in record['path'].split('/'))
                or str(PurePosixPath(record['path'])) != record['path']
                or (record['path'] != 'selection.json' and record['path'].split('/')[0] not in {'base', 'windows', 'linux', 'native'})
                or type(record['bytes']) is not int or not 0 < record['bytes'] <= final.LIMIT
                or not transport.hexadecimal(record['sha256'], 64)):
            raise ValueError('Final-input index contains an unbounded or unsafe record.')
        result[record['path']] = record
    if (list(result) != sorted(result) or len(result) != len(value['files'])
            or len({name.casefold() for name in result}) != len(result)
            or sum(record['bytes'] for record in result.values()) > MAX_TOTAL
            or result.get('selection.json', {}).get('sha256') != selection_sha256
            or result.get('selection.json', {}).get('bytes', MAX_INDEX + 1) > MAX_INDEX):
        raise ValueError('Final-input index duplicates, total budget or selection binding differ.')
    return result


def verify_data(root, directory, index, selection_sha256):
    directory = private_inspection_directory(readiness.handoff_path(directory), root, fresh=False)
    records = checked_index(index, index['commit'], selection_sha256)
    allowed_directories = {str(parent) for name in records for parent in PurePosixPath(name).parents if str(parent) != '.'}
    actual = set()
    for parent, directories, files in os.walk(directory, followlinks=False):
        for name in directories + files:
            path = Path(parent) / name
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
                raise ValueError('Final-input data cannot contain links or reparse points.')
            if name in directories and path.relative_to(directory).as_posix() not in allowed_directories:
                raise ValueError('Final-input data contains an unselected directory.')
            if name in files:
                relative = path.relative_to(directory).as_posix()
                if relative not in records:
                    raise ValueError('Final-input data contains an unselected file.')
                actual.add(relative)
    if actual != set(records):
        raise ValueError('Final-input data inventory has missing or extra files.')
    for name, record in records.items():
        if bounded_file(directory / name, record['bytes']) != (record['sha256'], record['bytes']):
            raise ValueError('Final-input bytes changed.')
    selected = json_input(directory / 'selection.json', MAX_INDEX, selection_sha256)
    roles, trusts = paths(selected)
    expected = inventory(directory / 'base', selected)
    for keys, name, limit, checksum in roles:
        if get(selected, keys) != name or records.get(name, {}).get('bytes', limit + 1) > limit:
            raise ValueError('Portable selected role differs from its fixed owned location/budget.')
        expected[name] = (directory / name, limit, checksum)
    for keys, platform in trusts:
        if get(selected, keys)['work'] != '@consumer/' + platform:
            raise ValueError('Native work must be designated by the fresh consumer.')
    if set(expected) | {'selection.json'} != set(records):
        raise ValueError('Bundle contains a file outside actual selected composition roles.')
    for name, (_, _, checksum) in expected.items():
        if checksum is not None and records[name]['sha256'] != checksum:
            raise ValueError('Bundle role/index checksums differ.')
    return selected


def consume(root, directory, output, work, index, selection_sha256):
    root, directory = map(readiness.handoff_path, [root, directory])
    selected = verify_data(root, directory, index, selection_sha256)
    final.signed._source(root, selected['commit'])
    work = private_inspection_directory(work, root, fresh=True)
    output = private_inspection_directory(output, root, fresh=True)
    if any(a.is_relative_to(b) for a, b in [(work, directory), (directory, work), (output, directory),
                                          (directory, output), (work, output), (output, work)]):
        raise ValueError('Consumer work, selected data and final output must be separate.')
    work.mkdir(mode=0o700)
    work.chmod(0o700)
    rebased = copy.deepcopy(selected)
    roles, trusts = paths(rebased)
    for keys, name, _, _ in roles:
        put(rebased, keys, str(directory / name))
    for keys, platform in trusts:
        get(rebased, keys)['work'] = str(work / platform)
    raw = final.encode(rebased)
    if len(raw) > MAX_INDEX:
        raise ValueError('Owned runtime selection exceeds the existing JSON budget.')
    runtime_selection = work / 'selection.json'
    with runtime_selection.open('xb') as stream:
        stream.write(raw)
    runtime_selection.chmod(0o400)
    candidate = final.from_selection(root, directory / 'base', output, runtime_selection, hashlib.sha256(raw).hexdigest())
    if (candidate.assessment['production_ready'] is not False or not candidate.blockers
            or (candidate.manifest['format'] != final.STORE_FORMAT
                and candidate.manifest['channels']['microsoft-store'] != final.store_channel())
            or any(channel['ready'] is not False for channel in candidate.manifest['channels'].values())):
        raise ValueError('Data handoff cannot grant production, installed or Store readiness.')
    if final.read(runtime_selection, MAX_INDEX) != raw:
        raise ValueError('Owned runtime selection changed during actual composition.')
    verify_data(root, directory, index, selection_sha256)
    candidate.verify()
    return candidate


def prepare(root, directory, selection, selection_sha256, output, work):
    root, directory, selection = map(readiness.handoff_path, [root, directory, selection])
    selected = json_input(selection, MAX_INDEX, selection_sha256)
    roles, trusts = paths(selected)
    final.signed._source(root, selected['commit'])
    work = private_inspection_directory(work, root, fresh=True)
    output = private_inspection_directory(output, root, fresh=True)
    original = inventory(directory, selected)
    portable = copy.deepcopy(selected)
    for keys, name, limit, checksum in roles:
        path = Path(get(selected, keys))
        if not path.is_absolute():
            path = selection.parent / path
        original[name] = (readiness.handoff_path(path), limit, checksum)
        put(portable, keys, name)
    for keys, platform in trusts:
        get(portable, keys)['work'] = '@consumer/' + platform
    raw = final.encode(portable)
    if not 0 < len(raw) <= min(MAX_INDEX, MAX_TOTAL):
        raise ValueError('Portable final selection exceeds its existing budget.')
    remaining = MAX_TOTAL - len(raw)
    for owned in [output, work]:
        if any(owned.is_relative_to(path.parent) or path.is_relative_to(owned)
               for path, _, _ in original.values()) or owned.is_relative_to(selection.parent):
            raise ValueError('Producer work/output must be separate from selected originals.')
    if work.is_relative_to(output) or output.is_relative_to(work):
        raise ValueError('Producer work and artifact output must be separate.')
    work.mkdir(mode=0o700)
    work.chmod(0o700)
    data = work / 'data'
    data.mkdir(mode=0o700)
    records = []
    for name, (path, limit, checksum) in original.items():
        target = data / name
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        actual, size = bounded_file(path, min(limit, remaining), target)
        if not size or (checksum is not None and actual != checksum):
            raise ValueError('Producer original differs from its selected role digest.')
        target.chmod(0o400)
        records.append(dict(path=name, bytes=size, sha256=actual))
        remaining -= size
    with (data / 'selection.json').open('xb') as stream:
        stream.write(raw)
    (data / 'selection.json').chmod(0o400)
    portable_sha256 = hashlib.sha256(raw).hexdigest()
    records.append(dict(path='selection.json', bytes=len(raw), sha256=portable_sha256))
    index = dict(schema=1, format=FORMAT, commit=selected['commit'], selection_sha256=portable_sha256,
                 files=sorted(records, key=lambda record: record['path']))
    checked_index(index, selected['commit'], portable_sha256)
    consume(root, data, work / 'composed', work / 'consumer', index, portable_sha256)
    for record in records:
        if record['path'] in original:
            path, limit, _ = original[record['path']]
            if bounded_file(path, limit) != (record['sha256'], record['bytes']):
                raise ValueError('Original input changed during producer composition.')
    if json_input(selection, MAX_INDEX, selection_sha256) != selected:
        raise ValueError('Original reviewed selection changed during preparation.')
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    with zipfile.ZipFile(output / ARCHIVE, 'x', compression=zipfile.ZIP_DEFLATED, allowZip64=False) as archive:
        for record in index['files']:
            archive.write(data / record['path'], record['path'])
    if bounded_file(output / ARCHIVE, transport.MAX_FILE)[1] > transport.MAX_FILE:
        raise ValueError('Final-input archive exceeds the existing artifact budget.')
    verify_data(root, data, index, portable_sha256)
    for record in records:
        if record['path'] in original:
            path, limit, _ = original[record['path']]
            if bounded_file(path, limit) != (record['sha256'], record['bytes']):
                raise ValueError('Original input changed during artifact construction.')
    if json_input(selection, MAX_INDEX, selection_sha256) != selected:
        raise ValueError('Original selection changed during artifact construction.')
    with (output / INDEX).open('xb') as stream:
        stream.write(final.encode(index))
    for path in output.iterdir():
        path.chmod(0o400)
    final.signed._source(root, selected['commit'])
    return index


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'consume'])
    for name in ['source-root', 'directory', 'output', 'work', 'selection']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--selection-sha256', required=True)
    parser.add_argument('--index', type=Path)
    parser.add_argument('--index-sha256')
    args = parser.parse_args(argv)
    try:
        if args.action == 'prepare':
            if args.index is not None or args.index_sha256 is not None:
                raise ValueError('Producer index is generated from actual selected bytes.')
            prepare(args.source_root, args.directory, args.selection, args.selection_sha256, args.output, args.work)
        else:
            if args.index is None or not transport.hexadecimal(args.index_sha256, 64) or args.selection != args.directory / 'selection.json':
                raise ValueError('Consumer requires the exact independently selected index and portable selection.')
            index = json_input(args.index, MAX_INDEX, args.index_sha256)
            consume(args.source_root, args.directory, args.output, args.work, index, args.selection_sha256)
        print('Final input bytes revalidated; installed, public, production and Store readiness remain blocked.')
        return 2
    except Exception:
        print('Final-input handoff failed; retain private partial work. No publication or submission occurred.', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
