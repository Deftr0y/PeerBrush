"""Bind actual disposable Linux receipts; authentication is a separate consumer."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import tomllib

from package import git
from package_deb import deb_version, inspect_final
from package_macos_dmg import bounded_file, private_inspection_directory
import production_readiness as readiness
import test_deb_version_upgrade as compatibility

REPORT = 'linux-deb-qualification.json'
FORMAT = 'PeerBrushLinuxDebQualificationV1'
GUI_FORMAT = 'PeerBrushLinuxDebQualificationV2'
LIMIT = 16 * 1024
INPUT_FIELDS = {'commit', 'version', 'portable', 'portable_sha256', 'package', 'package_sha256', 'maintainer'}
GUI_LIMITS = dict(display='private-Xvfb-software-rendering-requested', physical_desktop_consent='not-performed',
    gpu_hardware='not-qualified', default_app_choice='not-performed', unsaved_close_review='not-exercised',
    interruption_and_updates='not-performed', publisher_signature='not-verified', publication='not-assessed')
GUI_CAPTURES = [name for index in range(4) for name in [f'canvas-{index}.png', f'activation-{index}.png']] + \
    ['settings-settled.png'] + [f'restart-canvas-{index}.png' for index in range(4)]


def sha(value):
    return type(value) is str and re.fullmatch('[0-9a-f]{64}', value) is not None


def encode(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode('ascii')


def read(path, checksum, limit):
    if not sha(checksum):
        raise ValueError('Explicit independent Linux input digest required.')
    path = readiness.handoff_path(path)
    actual, size = bounded_file(path, limit)
    if not size or actual != checksum:
        raise ValueError('Linux input differs from its independent digest.')
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) != size or hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError('Linux input changed during its bounded read.')
    return data


def read_selection(path, checksum):
    return json.loads(read(path, checksum, 32768), object_pairs_hook=readiness.unique_fields)


def stable_source(root, commit):
    if (type(commit) is not str or not re.fullmatch('[0-9a-f]{40}', commit)
            or git(root, 'rev-parse', 'HEAD').decode().strip() != commit
            or git(root, 'status', '--porcelain', '--untracked-files=no').strip()):
        raise ValueError('Linux qualification requires clean exact committed source.')
    version = tomllib.loads(git(root, 'show', 'HEAD:Cargo.toml').decode())['package']['version']
    if type(version) is not str or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version):
        raise ValueError('Linux qualification requires a stable exact version.')
    return version


def inputs(root, selection):
    if not isinstance(selection, dict) or set(selection) != INPUT_FIELDS:
        raise ValueError('Strict Linux qualification inputs required.')
    version = stable_source(root, selection['commit'])
    if (selection['version'] != version or not sha(selection['portable_sha256']) or not sha(selection['package_sha256'])
            or Path(selection['portable']).name != f'PeerBrush-{version}-Linux-x64.zip'
            or Path(selection['package']).name != f'peerbrush_{deb_version(version)}_amd64.deb'):
        raise ValueError('Linux input source, canonical filenames or independent pins differ.')
    portable, package = map(readiness.handoff_path, [selection['portable'], selection['package']])
    return inspect_final(root, portable, package, commit=selection['commit'], version=version,
        portable_sha256=selection['portable_sha256'], package_sha256=selection['package_sha256'], maintainer=selection['maintainer'])


def gui_summary(data, assessment, installed, *, commit, version):
    """Strict metadata binding only; neither receipt JSON nor captures grant native authority."""
    if not isinstance(data, bytes) or not 0 < len(data) <= readiness.MAX_INSTALLATION_RECEIPT_BYTES:
        raise ValueError('A bounded actual installed GUI receipt is required.')
    actual = json.loads(data, object_pairs_hook=readiness.unique_fields)
    checks = actual.get('checks') if isinstance(actual, dict) else None
    if (not isinstance(checks, dict) or set(checks) != {'captures', 'native_cells', 'graceful_restarts',
            'current_profile_sha256', 'native_gui', 'ready'}
            or type(checks['native_cells']) is not int or checks['native_cells'] != 4
            or type(checks['graceful_restarts']) is not int or checks['graceful_restarts'] != 1
            or checks['native_gui'] != 'isolated-Xvfb-only' or checks['ready'] is not False
            or not sha(checks['current_profile_sha256']) or not isinstance(checks['captures'], list)
            or len(checks['captures']) != len(GUI_CAPTURES)):
        raise ValueError('Installed GUI checks differ from the bounded actual driver contract.')
    for item, name in zip(checks['captures'], GUI_CAPTURES):
        if (not isinstance(item, dict) or set(item) != {'file', 'sha256', 'width', 'height', 'canvas_regions'}
                or item['file'] != name or not sha(item['sha256'])
                or type(item['width']) is not int or item['width'] != 1360
                or type(item['height']) is not int or item['height'] != 900
                or not isinstance(item['canvas_regions'], list) or len(item['canvas_regions']) != 4):
            raise ValueError('Installed GUI capture inventory differs from the exact private driver output.')
        for box in item['canvas_regions']:
            if (not isinstance(box, list) or len(box) != 4 or any(type(value) is not int for value in box)
                    or not 0 <= box[0] < box[2] < 1360 or not 0 <= box[1] < box[3] < 900
                    or box[2] - box[0] < 80 or box[3] - box[1] < 60):
                raise ValueError('Installed GUI capture has invalid rendered region metadata.')
        a, b, c, d = item['canvas_regions']
        near = lambda first, second: abs(first - second) <= 2
        span_x, span_y = b[2] - a[0] + 1, c[3] - a[1] + 1
        if (not all(near(first, second) for first, second in [(a[0], c[0]), (a[2], c[2]),
                (b[0], d[0]), (b[2], d[2]), (a[1], b[1]), (a[3], b[3]), (c[1], d[1]), (c[3], d[3])])
                or not 1 <= b[0] - a[2] <= (span_x + 31) // 32 + 2
                or not 1 <= c[1] - a[3] <= (span_y + 23) // 24 + 2
                or not near(a[2] - a[0], b[2] - b[0]) or not near(a[3] - a[1], c[3] - c[1])
                or abs(span_x * 24 - span_y * 32) > 56):
            raise ValueError('Installed GUI region arrangement differs from the actual rendered-canvas oracle.')
    expected = dict(schema=1, format='PeerBrushInstalledGuiV1', commit=commit, version=version,
        package_sha256=assessment['record']['sha256'], executable_sha256=installed['executable_sha256'],
        source_sha256=assessment['record']['source_sha256'], target='Ubuntu 24.04 amd64', checks=checks,
        limits=GUI_LIMITS, candidate_verified=False, ready=False, production_ready=False)
    if encode(actual) != encode(expected):
        raise ValueError('Installed GUI receipt differs from selected source/package/runtime or original limits.')
    return dict(sha256=hashlib.sha256(data).hexdigest(), native_cells=4, graceful_restarts=1,
        capture_count=len(GUI_CAPTURES), native_gui='isolated-Xvfb-only', limits=dict(GUI_LIMITS))


def report(assessment, installation, upgrade, *, commit, version, portable_sha256, maintainer, gui=None):
    """Callers must supply their actual assess_deb result, never a ready JSON file."""
    if any(not isinstance(data, bytes) or not 0 < len(data) <= readiness.MAX_INSTALLATION_RECEIPT_BYTES
           for data in [installation, upgrade]):
        raise ValueError('Both bounded actual Linux receipts are required.')
    installed = json.loads(installation, object_pairs_hook=readiness.unique_fields)
    actual = json.loads(upgrade, object_pairs_hook=readiness.unique_fields)
    if not isinstance(actual, dict) or any(not sha(actual.get(name)) for name in ['old_executable_sha256', 'old_source_sha256']):
        raise ValueError('Portable-to-DEB receipt requires its actual historical identities.')
    expected = dict(schema=1, format='PeerBrushPortableToDebCompatibilityV1',
        old_version=compatibility.OLD_VERSION, old_commit=compatibility.OLD_COMMIT, old_portable_sha256=compatibility.OLD_SHA256,
        old_executable_sha256=actual['old_executable_sha256'], old_source_sha256=actual['old_source_sha256'],
        version=version, commit=commit, package_sha256=assessment['record']['sha256'],
        checks=['exact-public-historical-assets-and-corresponding-source',
                'explicit-instance-settings-continuity', 'opt-in-per-user-migration-and-default-restart',
                'native8-and16-PSD-and-PSB-rendered-and-editable-source-readback', 'original-settings-and-artwork-retained'],
        limits={'historical-deb-upgrade': 'not-available', 'automatic-default-discovery-migration': 'not-performed',
                'native-gui': 'not-performed', 'interrupted-update-and-unsaved-work': 'not-performed',
                'publisher-signature-and-repository': 'not-assessed', 'public-readiness': 'not-assessed'},
        candidate_verified=False, ready=False, production_ready=False)
    if encode(actual) != encode(expected):
        raise ValueError('Portable-to-DEB receipt differs from exact source, history, checks or original limits.')
    value = dict(schema=1, format=FORMAT, commit=commit, version=version,
        package=assessment['record'], portable_sha256=portable_sha256, maintainer=maintainer,
        installation=dict(sha256=hashlib.sha256(installation).hexdigest(), receipt=installed),
        portable_to_deb=dict(sha256=hashlib.sha256(upgrade).hexdigest(), receipt=actual),
        native_producer_authentication='unverified-requires-trusted-native-job',
        installed_acceptance='incomplete-headless-container-checks-only', public_availability='unverified',
        ready=False, production_ready=False)
    if gui is not None:
        value.update(schema=2, format=GUI_FORMAT,
            installed_gui=gui_summary(gui, assessment, installed, commit=commit, version=version),
            installed_acceptance='incomplete-isolated-gui-and-container-checks-only')
    return value


def qualify(root, selection, installation, installation_sha256, upgrade, upgrade_sha256, output, *, gui=None, gui_sha256=None):
    if (gui is None) != (gui_sha256 is None):
        raise ValueError('Installed GUI selection requires both its path and independent digest.')
    root = readiness.handoff_path(root)
    checked = inputs(root, selection)
    installed = read(installation, installation_sha256, readiness.MAX_INSTALLATION_RECEIPT_BYTES)
    compatible = read(upgrade, upgrade_sha256, readiness.MAX_INSTALLATION_RECEIPT_BYTES)
    graphical = read(gui, gui_sha256, readiness.MAX_INSTALLATION_RECEIPT_BYTES) if gui is not None else None
    assessment = readiness.assess_deb(root, Path(selection['portable']), Path(selection['package']),
        commit=selection['commit'], version=selection['version'], portable_sha256=selection['portable_sha256'],
        package_sha256=selection['package_sha256'], maintainer=selection['maintainer'], installation_receipt=installed)
    value = report(assessment, installed, compatible, commit=selection['commit'], version=selection['version'],
        portable_sha256=selection['portable_sha256'], maintainer=selection['maintainer'], gui=graphical)
    data = encode(value) + b'\n'
    if len(data) > LIMIT:
        raise ValueError('Linux qualification exceeds its fixed report budget.')
    output = private_inspection_directory(output, root, fresh=True)
    sources = [Path(selection['portable']), Path(selection['package']), Path(installation), Path(upgrade)]
    if gui is not None:
        sources.append(Path(gui))
    if any(output.is_relative_to(readiness.handoff_path(path).parent) for path in sources):
        raise ValueError('Linux report output must be separate from selected input directories.')
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    with (output / REPORT).open('xb') as stream:
        stream.write(data)
    (output / REPORT).chmod(0o400)
    if (inputs(root, selection) != checked
            or read(installation, installation_sha256, readiness.MAX_INSTALLATION_RECEIPT_BYTES) != installed
            or read(upgrade, upgrade_sha256, readiness.MAX_INSTALLATION_RECEIPT_BYTES) != compatible
            or (gui is not None and read(gui, gui_sha256, readiness.MAX_INSTALLATION_RECEIPT_BYTES) != graphical)
            or read(output / REPORT, hashlib.sha256(data).hexdigest(), LIMIT) != data):
        raise ValueError('Linux qualification inputs changed; preserve partial owned output.')
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'selection', 'installation', 'upgrade', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ['selection-sha256', 'installation-sha256', 'upgrade-sha256']:
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--gui', type=Path)
    parser.add_argument('--gui-sha256')
    args = parser.parse_args()
    try:
        selected = read_selection(args.selection, args.selection_sha256)
        qualify(args.source_root, selected, args.installation, args.installation_sha256,
                args.upgrade, args.upgrade_sha256, args.output, gui=args.gui, gui_sha256=args.gui_sha256)
        read_selection(args.selection, args.selection_sha256)
    except Exception:
        print('Linux qualification failed; preserve private work. No handoff or publication occurred.', file=sys.stderr)
        return 1
    print('Linux receipt bytes bound; native producer authentication and complete installed acceptance remain separate.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
