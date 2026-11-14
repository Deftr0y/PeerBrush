"""Qualify a first install or upgrade of signed casks on a disposable Apple Silicon desktop."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import subprocess
import sys
import tempfile
import time

from final_release import LIMIT, read
from package import regular_file, release_version_order
from package_macos_dmg import bounded_file, private_inspection_directory
from prepare_package_managers import (homebrew_cask, homebrew_layout, notarized_candidate,
                                      require_support_inventory, verify_installed_support)
from prepare_site_release import DOWNLOAD, STABLE
from production_readiness import handoff_path, unique_fields
from sign_macos import BUNDLE_ID, verify_native
from test_deb_installation import receipt_path, write_receipt
from test_deb_version_upgrade import PackageRegistration, PackageWorkspace, fixture_rpc
from test_msix_runtime import smoke


SHA = r'[0-9a-f]{64}'
COMMIT = r'[0-9a-f]{40}'
PREFIX = Path('/opt/homebrew')
CASK = 'peerbrush-qa/local/peerbrush'
QUIT = '''ObjC.import("AppKit"); function run(argv) {
    const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
    if (!app || ObjC.unwrap(app.bundleURL.path) !== argv[1]) throw Error("Different app");
    if (argv[2] === "quit" && !app.terminate) throw Error("Quit refused");
}'''


def select(selection, team):
    directory, digest = selection
    directory = handoff_path(directory)
    if not re.fullmatch(SHA, digest):
        raise ValueError('Select an independently authenticated cask definition receipt SHA-256.')
    report = json.loads(read(directory / 'package-managers.json', 128 * 1024, digest), object_pairs_hook=unique_fields)
    if (not isinstance(report, dict) or report.get('schema') != 1 or report.get('published') is not False
            or report.get('release_ready') is not False or not re.fullmatch(COMMIT, report.get('commit', ''))
            or not re.fullmatch('v' + STABLE, report.get('version', ''))):
        raise ValueError('Select an unpublished Homebrew definition from the release verifier.')
    tag = report['version']
    version = tag[1:]
    prefix = f'PeerBrush-{version}-macOS-arm64'
    channel = report.get('channels', {}).get('homebrew', {})
    asset = channel.get('asset')
    filename = asset.get('file') if isinstance(asset, dict) else None
    signed_zip = re.escape(prefix) + r'-apple-signed-r[1-9][0-9]*'
    dmg = isinstance(filename, str) and re.fullmatch(signed_zip + r'-dmg-signed-r[1-9][0-9]*\.dmg', filename) is not None
    if (channel.get('definition_prepared') is not True or not isinstance(asset, dict)
            or not notarized_candidate(asset) or asset.get('publisher_team_id') != team
            or not isinstance(filename, str)
            or (not dmg and not re.fullmatch(re.escape(prefix) + r'(?:-repacked-[0-9]{8})?-apple-signed-r[1-9][0-9]*\.zip', filename))
            or asset.get('binary_source_commit') != report['commit']
            or any(not re.fullmatch(SHA, asset.get(key, '')) for key in ('sha256', 'executable_sha256', 'source_sha256'))
            or asset.get('url') != DOWNLOAD + tag + '/' + filename):
        raise ValueError('Cask receipt does not match the selected Apple publisher and authenticated release assets.')
    if dmg:
        companion = asset.get('signed_zip')
        qualification = asset.get('qualification_sha256')
        if (not isinstance(companion, dict) or set(companion) != {'file', 'sha256'}
                or companion['file'] != filename.rsplit('-dmg-signed-r', 1)[0] + '.zip'
                or not isinstance(companion['sha256'], str) or not re.fullmatch(SHA, companion['sha256'])
                or not isinstance(qualification, str) or not re.fullmatch(SHA, qualification)):
            raise ValueError('Cask DMG receipt lacks the signed ZIP and typed qualification binding.')
    require_support_inventory(asset)
    checksum = report.get('files', {}).get('peerbrush.rb', '')
    if not re.fullmatch(SHA, checksum):
        raise ValueError('Homebrew receipt does not identify its cask.')
    cask = read(directory / 'peerbrush.rb', 128 * 1024, checksum)
    if cask != homebrew_cask(version, asset['sha256'], asset['url'], dmg=dmg):
        raise ValueError('Homebrew cask differs from the generated hook-free definition.')
    return version, asset, cask


def command(args, env, *, timeout=45, capture=False):
    with tempfile.TemporaryFile(dir=env['TMPDIR']) as output:
        try:
            subprocess.run(args, env=env, stdin=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           stdout=output if capture else subprocess.DEVNULL, check=True, timeout=timeout)
        except (OSError, subprocess.SubprocessError):
            raise ValueError(f'{Path(args[0]).name} failed during native Homebrew acceptance.') from None
        if output.tell() > 256 * 1024:
            raise ValueError('Native Homebrew acceptance output exceeded its byte budget.')
        output.seek(0)
        return output.read().decode('utf-8').strip()


def verify_cask_state(raw, candidate, *, installed):
    version, asset, _ = candidate
    report = json.loads(raw, object_pairs_hook=unique_fields)
    rows = report.get('casks') if isinstance(report, dict) else None
    expected = dict(token='peerbrush', full_token=CASK, tap=CASK.rsplit('/', 1)[0],
                    version=version, installed=version if installed else None,
                    sha256=asset['sha256'], url=asset['url'])
    if (not isinstance(rows, list) or len(rows) != 1 or not isinstance(rows[0], dict)
            or any(key not in rows[0] or rows[0][key] != value for key, value in expected.items())):
        raise ValueError('Homebrew cask state differs from the selected definition or installation phase.')


def verify_cached_archive(cache, reported, checksum):
    cache = handoff_path(cache)
    path = Path(reported)
    if (not path.is_absolute() or '..' in path.parts or not path.is_relative_to(cache)
            or path == cache or any(char in reported for char in '\r\n\0')):
        raise ValueError('Homebrew did not select an archive inside the private download cache.')
    handoff_path(path.parent)
    target = path.resolve(strict=True)
    if not target.is_relative_to(cache) or bounded_file(handoff_path(target), LIMIT)[0] != checksum:
        raise ValueError('Homebrew cached download differs from the authenticated release archive.')


def verify_support_tree(support, version, asset):
    support = handoff_path(support)
    verify_installed_support(support, version, asset)
    expected = {}
    for name in [*asset['support_files'], f'PeerBrush-{version}-source.zip']:
        part = Path(name)
        while part.parts:
            expected.setdefault(part.parent, set()).add(part.name)
            part = part.parent
    for folder, names in expected.items():
        remaining = names.copy()
        with os.scandir(handoff_path(support / folder)) as entries:
            for entry in entries:
                if entry.name not in remaining:
                    raise ValueError('Homebrew installed an unexpected corresponding source or notice entry.')
                remaining.remove(entry.name)
        if remaining:
            raise ValueError('Homebrew corresponding source or notice inventory is incomplete.')


def verify_retired(caskroom, support, version):
    for root in (caskroom, caskroom / '.metadata'):
        if root.exists() or root.is_symlink():
            root = handoff_path(root)
            if (not root.is_dir() or any((root / name).is_symlink() or (root / name).exists()
                                         for name in (version, version + '.upgrading'))):
                raise ValueError('Homebrew retained staged files or metadata for a retired cask generation.')
    if support.exists() or support.is_symlink():
        # Homebrew may leave the empty parent directory of moved artifacts.
        with os.scandir(handoff_path(support)) as entries:
            if next(entries, None) is not None:
                raise ValueError('Homebrew retained corresponding source or notices for a retired cask generation.')


def installed_app(candidate, team, app, launcher):
    version, asset, _ = candidate
    executable = regular_file(app, 'Contents/MacOS/peerbrush')
    app_source, _ = homebrew_layout(version, dmg=asset['file'].endswith('.dmg'))
    slot = PREFIX / 'Caskroom/peerbrush' / version / app_source
    handoff_path(slot.parent)
    if (not slot.is_symlink() or slot.resolve(strict=True) != app or not launcher.is_symlink()
            or launcher.resolve(strict=True) != executable
            or bounded_file(executable, LIMIT, executable=True)[0] != asset['executable_sha256']):
        raise ValueError('The Homebrew installation links or executable differ from the selected generation.')
    info = plistlib.loads(read(app / 'Contents/Info.plist', 128 * 1024))
    if info.get('CFBundleIdentifier') != BUNDLE_ID or info.get('CFBundleShortVersionString') != version:
        raise ValueError('Installed Homebrew bundle identity differs from the selected release.')
    support = PREFIX / 'share/peerbrush' / version
    verify_support_tree(support, version, asset)
    verify_native(app, team, notarized=True)
    return support


@contextmanager
def launch_bundle(app, state, env, documents=()):
    def observe_connection():
        try:
            path = regular_file(state, 'connection.json')
        except FileNotFoundError:
            return b''
        return read(path, 64 * 1024)

    previous = observe_connection()
    proc = subprocess.Popen(['/usr/bin/open', '-W', '-n', '-a', app, *documents, '--args', '--state-dir', state],
                            env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    pid = None

    def alive():
        if proc.poll() is not None:
            raise ValueError('The installed app exited during the Launch Services check.')

    try:
        deadline = time.monotonic() + 120
        while proc.poll() is None and time.monotonic() < deadline:
            raw = observe_connection()
            if raw and raw != previous:
                session = json.loads(raw, object_pairs_hook=unique_fields)
                selected = session.get('pid') if isinstance(session, dict) else None
                if type(selected) is int and 1 < selected < 2 ** 31:
                    command(['/usr/bin/osascript', '-l', 'JavaScript', '-e', QUIT, str(selected), app, 'check'], env)
                    pid = selected
                    break
            time.sleep(0.2)
        if pid is None:
            raise ValueError('Launch Services did not start the selected PeerBrush app.')
        alive()
        yield alive
        alive()
    finally:
        if proc.poll() is None:
            try:
                if pid is not None:
                    command(['/usr/bin/osascript', '-l', 'JavaScript', '-e', QUIT, str(pid), app, 'quit'], env)
                    if proc.wait(timeout=20) != 0:
                        raise ValueError('The installed app did not quit normally.')
            finally:
                if proc.poll() is None:
                    proc.terminate()
                    proc.wait(timeout=10)


def launch(candidate, app, launcher, state, env, workspace, registration, *, require_owner):
    version, asset, _ = candidate
    quarantined = command(['/usr/bin/xattr', '-p', 'com.apple.quarantine', app], env, capture=True)
    if not re.fullmatch(r'[a-fA-F0-9]{4,8};[^\r\n]+', quarantined):
        raise ValueError('Homebrew did not retain a native quarantine record on the app.')
    with launch_bundle(app, state, env):
        route = registration.check(launcher, app / 'Contents/MacOS/peerbrush', env,
                                   lambda args, environment: command(args, environment, capture=True))
        evidence = smoke(route[0], state, allow_portable=True, env=env)['installation_observation']
        owner = evidence.get('update_owner')
        if (evidence['build']['version'] != version or evidence['build']['commit'] != asset['binary_source_commit']
                or evidence['build']['tree'] != 'clean' or evidence['package'] is not None
                or ((owner is not None or require_owner) and owner != 'HomebrewPackage')):
            raise ValueError('Live application provenance or observed Homebrew update ownership differs.')
        workspace.check_launcher(launcher, env)

    document_state = state.parent / f'{version}-document-startup'
    document_state.mkdir(mode=0o700)
    capture = document_state / 'workspace.png'
    workspace.preserve()
    with launch_bundle(app, document_state, env, workspace.projects) as alive:
        def rpc(method, parameters):
            alive()
            return fixture_rpc([launcher, '--state-dir', document_state], env, method, parameters)

        workspace.check_startup_projects(rpc)
        rpc('capture_ui', dict(path=str(capture), panel='workspace'))
        deadline = time.monotonic() + 40
        while not capture.is_file() and time.monotonic() < deadline:
            alive()
            time.sleep(0.2)
        frame = regular_file(capture.parent, capture.name)
        thumb = capture.with_suffix('.thumb.png')
        command(['/usr/bin/sips', '-Z', '128', frame, '--out', thumb], env)
        if regular_file(thumb.parent, thumb.name).stat().st_size <= 100:
            raise ValueError('The installed workspace did not yield a decodable rendered frame.')
    return owner == 'HomebrewPackage'


def check(older, newer, output, *, team, brew_commit):
    if (sys.platform != 'darwin' or platform.machine() != 'arm64' or os.geteuid() == 0
            or os.environ.get('PEERBRUSH_DISPOSABLE_HOMEBREW_TEST') != '1'):
        raise ValueError('Use a disposable native Apple Silicon desktop and standard user.')
    if not re.fullmatch(r'[A-Z0-9]{10}', team) or not re.fullmatch(COMMIT, brew_commit):
        raise ValueError('Select the independently verified Apple team and Homebrew commit.')
    root = Path(__file__).resolve().parents[1]
    output = receipt_path(output)
    private_inspection_directory(output.parent, root, fresh=False)
    old = select(older, team)
    new = select(newer, team) if newer is not None else None
    if new is not None and release_version_order(new[0]) <= release_version_order(old[0]):
        raise ValueError('Homebrew acceptance requires two increasing application versions.')
    brew = regular_file(handoff_path(PREFIX), 'bin/brew')
    scratch = Path(tempfile.gettempdir()).resolve()
    with tempfile.TemporaryDirectory(prefix='peerbrush-brew-qa-', dir=scratch) as directory:
        work = private_inspection_directory(Path(directory), root, fresh=False)
        home, tmp, apps, state = (work / name for name in ('home', 'tmp', 'Applications', 'workspace'))
        for path in (home, tmp, apps, state):
            path.mkdir(mode=0o700)
        cache = work / 'cache'
        env = dict(HOME=str(home), TMPDIR=str(tmp), PATH='/usr/bin:/bin:/usr/sbin:/sbin', LC_ALL='C',
                   HOMEBREW_CACHE=str(cache), HOMEBREW_LOGS=str(work / 'logs'),
                   HOMEBREW_NO_ANALYTICS='1', HOMEBREW_NO_AUTO_UPDATE='1', HOMEBREW_NO_INSTALL_CLEANUP='1',
                   HOMEBREW_NO_ENV_HINTS='1', GIT_CONFIG_NOSYSTEM='1', GIT_TERMINAL_PROMPT='0')

        def native(*args, capture=False):
            return command([brew, *args], env, timeout=1800, capture=capture)

        def observe(candidate, *, installed):
            verify_cask_state(native('info', '--cask', '--json=v2', CASK, capture=True), candidate,
                              installed=installed)
            if installed:
                verify_cached_archive(cache, native('--cache', '--cask', CASK, capture=True), candidate[1]['sha256'])

        git = ['/usr/bin/git', '-C', PREFIX, '-c', 'core.fsmonitor=false']
        if (command([*git, 'rev-parse', '--show-toplevel'], env, capture=True) != str(PREFIX)
                or command([*git, 'rev-parse', 'HEAD'], env, capture=True) != brew_commit
                or command([*git, 'status', '--porcelain'], env, capture=True)
                or native('--repository', capture=True) != str(PREFIX)
                or native('--prefix', capture=True) != str(PREFIX)
                or native('--caskroom', capture=True) != str(PREFIX / 'Caskroom')):
            raise ValueError('Homebrew runner differs from the reviewed clean native installation.')
        if command(['/usr/sbin/spctl', '--status'], env, capture=True) != 'assessments enabled':
            raise ValueError('The disposable runner must use active Gatekeeper assessments.')
        tap = PREFIX / 'Library/Taps/peerbrush-qa/homebrew-local'
        launcher = PREFIX / 'bin/peerbrush'
        caskroom = PREFIX / 'Caskroom/peerbrush'
        if any(path.exists() or path.is_symlink() for path in (tap.parent, launcher, caskroom,
                                                               PREFIX / 'share/peerbrush', Path('/Applications/PeerBrush.app'))):
            raise ValueError('The disposable Homebrew runner already has a PeerBrush installation or fixture tap.')
        taps = tap.parent.parent
        if not taps.exists() and not taps.is_symlink():
            handoff_path(taps.parent)
            taps.mkdir()
        handoff_path(taps)
        casks = tap / 'Casks'
        casks.mkdir(parents=True)
        cask = casks / 'peerbrush.rb'
        app = apps / 'PeerBrush.app'
        artwork = work / 'artwork'
        artwork.mkdir(mode=0o700)
        workspace = PackageWorkspace(state, artwork)
        registration = PackageRegistration(work / 'client-home', state)
        fixtures = {state / 'preview.json': b'{"version":1,"reduced_resolution":true}\n',
                    work / 'Artwork 作品.psd': b'SYNTHETIC_USER_OWNED_HOMEBREW_RETENTION_FIXTURE\n'}
        for path, payload in fixtures.items():
            path.write_bytes(payload)

        def preserve():
            if any(read(path, 64 * 1024) != payload for path, payload in fixtures.items()):
                raise ValueError('Homebrew changed a user fixture.')

        print('Checking the first signed Homebrew cask and its quarantined launch.', flush=True)
        cask.write_bytes(old[2])
        native('install', '--cask', f'--appdir={apps}', CASK)
        observe(old, installed=True)
        old_support = installed_app(old, team, app, launcher)
        old_owner = launch(old, app, launcher, state, env, workspace, registration, require_owner=new is None)
        installations = [(old_support, old[0])]
        preserve()
        if new is not None:
            print('Checking Homebrew upgrade, Gatekeeper and the same live MCP launcher.', flush=True)
            cask.write_bytes(new[2])
            native('upgrade', '--cask', f'--appdir={apps}', CASK)
            observe(new, installed=True)
            verify_retired(caskroom, old_support, old[0])
            new_support = installed_app(new, team, app, launcher)
            launch(new, app, launcher, state, env, workspace, registration, require_owner=True)
            installations.append((new_support, new[0]))
            preserve()
        print('Checking Homebrew removal and fixture preservation.', flush=True)
        native('uninstall', '--cask', CASK)
        observe(new or old, installed=False)
        for support, version in installations:
            verify_retired(caskroom, support, version)
        if any(path.exists() or path.is_symlink() for path in (launcher, app, caskroom)):
            raise ValueError('Homebrew did not remove the application, launcher and Caskroom entry.')
        preserve()
        workspace_evidence = workspace.retention_receipt()
        registration.preserve()
        cask.unlink()
        for folder in (casks, tap, tap.parent):
            folder.rmdir()
    evidence = dict(schema=1, brew_commit=brew_commit, publisher_team_id=team, install='verified',
                    removal='verified', user_files='preserved', mcp='live-stdio-bridge',
                    workspace=workspace_evidence,
                    mcp_registration='private-client-config-backed-up-and-reused',
                    archive_cache='verified', support_tree='verified', cask_state='verified',
                    app='quarantined-LaunchServices-rendered-workspace', signing='native-Developer-ID-stapled-Gatekeeper',
                    desktop_startup_documents='launch-services-native8-and-native16-psd-and-psb-verified',
                    public_availability='unverified', release_ready=False)
    initial = dict(version=old[0], definition_sha256=older[1], archive_sha256=old[1]['sha256'],
                   owner_observed=old_owner)
    if new is None:
        evidence.update(format='PeerBrushHomebrewInstallV1', candidate=initial, upgrade='not-performed')
    else:
        evidence.update(format='PeerBrushHomebrewUpgradeV1', older=initial, upgrade='verified', previous_generation_retired=True,
                        newer=dict(version=new[0], definition_sha256=newer[1],
                                   archive_sha256=new[1]['sha256'], owner_observed=True))
    write_receipt(output, evidence)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    selection = ('DEFINITION', 'RECEIPT_SHA256')
    mode.add_argument('--install-only', nargs=2, metavar=selection)
    mode.add_argument('--older', nargs=2, metavar=selection)
    parser.add_argument('--newer', nargs=2, metavar=selection)
    parser.add_argument('--team', required=True)
    parser.add_argument('--brew-commit', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    if args.older and args.newer is None:
        parser.error('--older requires --newer')
    if args.install_only and args.newer:
        parser.error('--install-only cannot be combined with --newer')
    check(args.install_only or args.older, args.newer, args.output, team=args.team, brew_commit=args.brew_commit)


if __name__ == '__main__':
    main()
