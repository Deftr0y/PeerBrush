"""Native final installer qualification; no signing, installation or publication."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

from package_macos_dmg import bounded_file, private_inspection_directory
from production_readiness import unique_fields
import prepare_appinstaller as installer
import qualify_signed_msix as signed

REPORT = 'windows-installer-qualification.json'
TIMESTAMPS = ('timestamp_certificate_sha256', 'executable_timestamp_certificate_sha256', 'runtime_timestamp_certificate_sha256')


def qualify(root, report_output, *, timestamp_certificate_sha256, executable_timestamp_certificate_sha256,
            runtime_timestamp_certificate_sha256, **inputs):
    pins = (timestamp_certificate_sha256, executable_timestamp_certificate_sha256, runtime_timestamp_certificate_sha256)
    if any(not signed._sha(pin) for pin in pins):
        raise ValueError('Independently selected public package, executable and runtime timestamp leaves required.')
    output = private_inspection_directory(report_output, root, fresh=True)
    work = private_inspection_directory(inputs['work'], root, fresh=True)
    prepared = private_inspection_directory(inputs['output'], root, fresh=True)
    if any(a.is_relative_to(b) for a in [output] for b in [work, prepared]) or any(b.is_relative_to(output) for b in [work, prepared]):
        raise ValueError('Native qualification output must be separate from installer work/output.')
    if any(output.is_relative_to(signed.inspection_path(Path(inputs[key])).parent)
           for key in ['portable', 'unsigned', 'package', 'runtime', 'identity', 'runtime_license', 'signtool', 'powershell']):
        raise ValueError('Native qualification output must be separate from selected input/tool directories.')
    descriptor, result = installer.prepare(root, **inputs, require_executable_signature=True, return_qualification=True)
    actual = (result['signed_package']['timestamp_certificate_sha256'],
              result['signed_package']['executable']['timestamp_certificate_sha256'],
              result['runtime']['timestamp_certificate_sha256'])
    if actual != pins:
        raise ValueError('Verified native timestamp leaves differ from independently selected public pins.')
    data = (json.dumps(result, indent=2, sort_keys=True) + '\n').encode('utf-8')
    if len(data) > signed.OUTPUT_LIMIT:
        raise ValueError('Native installer qualification exceeds its public report budget.')
    def unchanged():
        signed._source(root, inputs['commit'])
        for key, checksum, limit in [
            ('portable', inputs['portable_sha256'], signed.msix.PACKAGE_LIMIT),
            ('unsigned', inputs['unsigned_sha256'], signed.msix.PACKAGE_LIMIT),
            ('package', inputs['package_sha256'], signed.msix.PACKAGE_LIMIT),
            ('runtime', inputs['runtime_sha256'], signed.msix.PACKAGE_LIMIT),
            ('identity', result['identity_sha256'], 16 * 1024),
            ('runtime_license', inputs['runtime_license_sha256'], 256 * 1024)]:
            if bounded_file(Path(inputs[key]), limit)[0] != checksum:
                raise ValueError('Qualified native input changed before evidence retention.')
        for key, name in [('signtool', 'signtool.exe'), ('powershell', 'pwsh.exe')]:
            signed._selected_tool(inputs[key], inputs[key + '_sha256'], name)
        if bounded_file(descriptor, 16 * 1024)[0] != result['descriptor']['sha256']:
            raise ValueError('Qualified descriptor changed before evidence retention.')
    unchanged()
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    private_inspection_directory(output, root, fresh=False)
    with (output / REPORT).open('xb') as stream:
        stream.write(data)
    if bounded_file(output / REPORT, signed.OUTPUT_LIMIT)[0] != hashlib.sha256(data).hexdigest():
        raise ValueError('Retained native qualification changed.')
    unchanged()
    return result


def read_selection(path, checksum):
    if not signed._sha(checksum):
        raise ValueError('Independently reviewed selection SHA-256 required.')
    actual, size = bounded_file(path, 32 * 1024)
    with Path(path).open('rb') as stream:
        data = stream.read(32 * 1024 + 1)
    if not size or actual != checksum or len(data) != size or hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError('Public native selection changed or differs from its reviewed digest.')
    value = json.loads(data, object_pairs_hook=unique_fields)
    if not isinstance(value, dict):
        raise ValueError('Native selection must be an object.')
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'selection', 'report-output']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--selection-sha256', required=True)
    args = parser.parse_args()
    try:
        selection = read_selection(args.selection, args.selection_sha256)
        paths = ['portable', 'unsigned', 'package', 'runtime', 'identity', 'runtime_license', 'signtool', 'powershell', 'work', 'output']
        selection = dict(selection)
        for name in paths:
            selection[name] = Path(selection[name])
        qualify(args.source_root, args.report_output, **selection)
        read_selection(args.selection, args.selection_sha256)
    except Exception:
        print('Native Windows installer qualification failed; preserve private work. No installation or publication occurred.', file=sys.stderr)
        return 1
    print('Native Windows installer facts retained; authenticated origin, installed acceptance and public availability remain separate.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
