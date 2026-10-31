"""Read-only native final-DMG consumer. No signing, installation or publication."""
import argparse
import json
import os
import pathlib
import sys

from package_macos_dmg import inspect_final, private_inspection_directory


def qualify(root, archive, image, output, **inputs):
    output = private_inspection_directory(output, root, fresh=True)
    work = private_inspection_directory(inputs['work'], root, fresh=True)
    if (output.is_relative_to(work) or work.is_relative_to(output)
            or any(output.is_relative_to(pathlib.Path(path).absolute().parent) for path in [archive, image])):
        raise ValueError('Final DMG report output must be separate from work and input directories')
    result = inspect_final(root, archive, image, **inputs)
    data = (json.dumps(result, indent=2, sort_keys=True) + '\n').encode('utf-8')
    if len(data) > 8192:
        raise ValueError('Final DMG qualification exceeds its bounded public report contract')
    private_inspection_directory(output, root, fresh=True)
    output.mkdir(mode=0o700)
    os.chmod(output, 0o700)
    private_inspection_directory(output, root, fresh=False)
    with (output / 'dmg-qualification.json').open('xb') as stream:
        stream.write(data)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'archive', 'image', 'work', 'output']:
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    for name in ['commit', 'version', 'archive-sha256', 'image-sha256', 'team-id']:
        parser.add_argument('--' + name, required=True)
    arguments = vars(parser.parse_args())
    arguments['root'] = arguments.pop('source_root')
    try:
        qualify(**arguments)
    except Exception:
        # Neither local paths nor native tool output or account information may
        # enter public logs. Retain failed work for deliberate local inspection.
        print('Final DMG qualification failed; preserve private work for local inspection.', file=sys.stderr)
        return 1
    print('Final DMG content and native trust qualified; installation and production readiness remain unverified.')
    return 0


if __name__ == '__main__':
    sys.exit(main())
