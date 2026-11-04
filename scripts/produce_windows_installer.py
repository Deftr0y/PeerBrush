"""Prepare an existing-certificate signing handoff; no enrollment, install or publication."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

from package_macos_dmg import bounded_file, private_inspection_directory
from production_readiness import unique_fields
from qualify_windows_installer import read_selection
import sign_windows_msix as signer


def produce(root, portable, selection, *, work, output, approve_existing_certificate_signing=False):
    if approve_existing_certificate_signing is not True:
        raise ValueError('Existing-certificate signing requires explicit approval.')
    paths = {'runtime', 'identity', 'runtime_license', 'signtool', 'powershell', 'makeappx'}
    values = {'commit', 'version', 'package_family', 'portable_sha256', 'certificate_sha1', 'certificate_sha256',
              'timestamp_uri', 'signtool_sha256', 'powershell_sha256', 'makeappx_sha256', 'revision',
              'runtime_sha256', 'runtime_license_sha256'}
    if not isinstance(selection, dict) or set(selection) != paths | values:
        raise ValueError('Strict public existing-certificate signing/media/tool selection required.')
    selected = dict(selection)
    for name in paths:
        selected[name] = signer.inspection_path(Path(selected[name]))
    work = private_inspection_directory(work, root, fresh=True)
    output = private_inspection_directory(output, root, fresh=True)
    sources = [signer.inspection_path(portable), *[selected[name] for name in paths]]
    if (work.is_relative_to(output) or output.is_relative_to(work)
            or any(area.is_relative_to(path.parent) for area in [work, output] for path in sources)):
        raise ValueError('Use separate fresh private signing handoff work/output and selected inputs.')
    identity_hash, identity_size = bounded_file(selected['identity'], 16 * 1024)
    with selected['identity'].open('rb') as stream:
        identity_data = stream.read(16 * 1024 + 1)
    if (not 0 < len(identity_data) <= 16 * 1024 or len(identity_data) != identity_size
            or hashlib.sha256(identity_data).hexdigest() != identity_hash):
        raise ValueError('Public identity exceeds its budget.')
    identity = json.loads(identity_data, object_pairs_hook=unique_fields)
    media = [(sources[0], selection['portable_sha256']), (selected['runtime'], selection['runtime_sha256']),
             (selected['runtime_license'], selection['runtime_license_sha256'])]
    for path, checksum in media:
        if not signer.qualified._sha(checksum) or bounded_file(path, signer.msix.PACKAGE_LIMIT)[0] != checksum:
            raise ValueError('Signing handoff input differs from independent bytes.')
    work.mkdir(mode=0o700)
    signing = {key: selected[key] for key in values - {'runtime_sha256', 'runtime_license_sha256'}}
    signing.update({key: selected[key] for key in ['signtool', 'powershell', 'makeappx']})
    result = signer.produce(root, sources[0], identity=identity, **signing, work=work / 'signing',
        output=work / 'signed', approve_existing_certificate_signing=True)
    package = work / 'signed' / result['package']['file']
    unsigned = work / 'signed' / ('PeerBrush-' + result['package_version'] + '-Windows-x64.msix')
    names = [sources[0].name, unsigned.name, package.name, selected['runtime'].name, 'direct-identity.json', 'runtime-license.txt']
    signer.msix.validate_casefold_paths(names)
    copies = [(sources[0], names[0], selection['portable_sha256']), (unsigned, unsigned.name, result['unsigned_sha256']),
              (package, package.name, result['package']['sha256']), (selected['runtime'], selected['runtime'].name, selection['runtime_sha256']),
              (selected['identity'], 'direct-identity.json', hashlib.sha256(identity_data).hexdigest()),
              (selected['runtime_license'], 'runtime-license.txt', selection['runtime_license_sha256'])]
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    for source, name, checksum in copies:
        if bounded_file(source, signer.msix.PACKAGE_LIMIT, output / name)[0] != checksum:
            raise ValueError('Signing handoff changed during exclusive retention.')
        (output / name).chmod(0o400)
    signer.qualified._source(root, selection['commit'])
    if {path.name for path in output.iterdir()} != set(names):
        raise ValueError('Unexpected signing handoff inventory.')
    for source, name, checksum in copies:
        if (bounded_file(source, signer.msix.PACKAGE_LIMIT)[0] != checksum
                or bounded_file(output / name, signer.msix.PACKAGE_LIMIT)[0] != checksum):
            raise ValueError('Original or copied signing handoff changed; preserve private work.')
    for key, name in [('signtool', 'signtool.exe'), ('powershell', 'pwsh.exe'), ('makeappx', 'makeappx.exe')]:
        signer.qualified._selected_tool(selected[key], selected[key + '_sha256'], name)
    private_inspection_directory(output, root, fresh=False)
    return {name: checksum for _, name, checksum in copies}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'portable', 'selection', 'work', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--selection-sha256', required=True)
    parser.add_argument('--approve-existing-certificate-signing', action='store_true')
    args = parser.parse_args()
    try:
        selected = read_selection(args.selection, args.selection_sha256)
        produce(args.source_root, args.portable, selected, work=args.work, output=args.output,
            approve_existing_certificate_signing=args.approve_existing_certificate_signing)
        read_selection(args.selection, args.selection_sha256)
    except Exception:
        print('Windows signing handoff failed or disabled; preserve private work. No install or publication occurred.', file=sys.stderr)
        return 1
    print('Windows signing bytes retained for independent hosted qualification; signer-host provenance is not authenticated.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
