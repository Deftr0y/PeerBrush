"""Qualify a pinned Nix install or upgrade in a disposable Linux runner."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import zipfile

from final_release import LIMIT, read
from package import regular_file, relative_path, release_version_order
from package_macos_dmg import bounded_file, private_inspection_directory
from prepare_nix_package import validate_lock
from prepare_site_release import STABLE
from production_readiness import handoff_path, unique_fields
from test_deb_installation import receipt_path, write_receipt
from test_deb_installed_gui import Window, check_startup_window, desktop_session_environment, isolated_display_environment
from test_deb_version_upgrade import PackageRegistration, PackageWorkspace
from test_msix_runtime import require_linux_update_owner


HEX = r'[0-9a-f]{64}'
STORE = r'/nix/store/[0123456789abcdfghijklmnpqrsvwxyz]{32}-[A-Za-z0-9+._?=-]+'
NAR_HASH = r'sha256-[A-Za-z0-9+/]{43}='
DEFINITION = {'flake.nix', 'default.nix', 'package.nix', 'Cargo.lock', 'source-inputs.json'}
DESKTOP = 'share/applications/com.peerbrush.PeerBrush.desktop'
MIME = 'share/mime/packages/com.peerbrush.PeerBrush.xml'
ICON = 'share/peerbrush/assets/peerbrush-logo.png'


def definition_inventory(report):
    if (not isinstance(report, dict) or report.get('schema') != 1 or report.get('format') != 'PeerBrushNixPackagePreparationV1'
            or report.get('platforms') != ['x86_64-linux'] or report.get('release_ready') is not False):
        raise ValueError('Select an unpublished native Nix package definition.')
    files, executable = report.get('files'), report.get('executable_files')
    if (not isinstance(files, dict) or not DEFINITION <= files.keys() or len(files) > 4096
            or not isinstance(executable, list) or any(not isinstance(name, str) for name in executable)
            or sorted(set(executable)) != executable or not set(executable) <= files.keys() - DEFINITION):
        raise ValueError('Nix definition has no complete canonical file and mode inventory.')
    for name, checksum in files.items():
        relative_path(name)
        if (name not in DEFINITION and not name.startswith('support/')) or not isinstance(checksum, str) or not re.fullmatch(HEX, checksum):
            raise ValueError('Nix definition inventory contains an unsupported file.')
    return files, executable


def source_inputs(report, files, data):
    inputs = json.loads(data, object_pairs_hook=unique_fields)
    if (not isinstance(inputs, dict) or inputs != report.get('inputs') or inputs.get('format') != 'PeerBrushNixSourceInputsV1'
            or not isinstance(inputs.get('version'), str) or not re.fullmatch(STABLE, inputs['version'])
            or inputs['version'] != report.get('version') or inputs.get('source_commit') != report.get('commit')
            or not isinstance(inputs.get('source_sha256'), str) or not re.fullmatch(HEX, inputs['source_sha256'])
            or inputs.get('cargo_lock_sha256') != files['Cargo.lock']):
        raise ValueError('Nix definition differs from its source inputs.')
    return inputs


def snapshot(selection, destination):
    directory, digest = selection
    directory = handoff_path(directory)
    if not re.fullmatch(HEX, digest):
        raise ValueError('Select an independently authenticated Nix definition receipt digest.')
    report = json.loads(read(directory / 'nix-package.json', 512 * 1024, digest), object_pairs_hook=unique_fields)
    files, executable = definition_inventory(report)
    total = 0
    for name, checksum in files.items():
        data = read(directory / name, 32 * 1024 * 1024, checksum)
        total += len(data)
        if total > 128 * 1024 * 1024:
            raise ValueError('Nix definition exceeds its private snapshot budget.')
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open('xb') as stream:
            stream.write(data)
        target.chmod(0o755 if name in executable else 0o644)
    inputs = source_inputs(report, files, (destination / 'source-inputs.json').read_bytes())
    return inputs, files['source-inputs.json']


def command(args, env, *, data=None, timeout=45, limit=256 * 1024):
    with tempfile.TemporaryFile() as output:
        with subprocess.Popen(args, env=env, stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                              stdout=output, stderr=subprocess.DEVNULL, start_new_session=True) as process:
            try:
                process.communicate(data, timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                raise ValueError('Native Nix acceptance command was interrupted.') from None
            if process.returncode:
                raise ValueError('Native Nix acceptance command failed.')
        if output.tell() > limit:
            raise ValueError('Native Nix acceptance output exceeded its bound.')
        output.seek(0)
        return output.read(limit).decode('utf-8', errors='strict').strip()


def verify_profile(manifest, *, source=None, output=None):
    elements = manifest.get('elements') if isinstance(manifest, dict) else None
    if not isinstance(elements, dict) or manifest.get('version') != 3:
        raise ValueError('Nix profile has an unsupported manifest.')
    if output is None:
        if elements:
            raise ValueError('Nix profile removal left package records.')
        return
    if len(elements) != 1:
        raise ValueError('Nix profile does not contain exactly one package.')
    element = next(iter(elements.values()))
    if (not isinstance(element, dict) or element.get('active') is not True
            or element.get('attrPath') != 'packages.x86_64-linux.peerbrush'
            or (element.get('originalUrl'), element.get('url')) != source
            or element.get('storePaths') != [str(output)]):
        raise ValueError('Nix profile does not select the expected PeerBrush output and flake.')


def profile_generation(profile):
    if not profile.is_symlink():
        raise ValueError('Nix did not create an active profile link.')
    link = os.readlink(profile)
    number = re.fullmatch(rf'{re.escape(profile.name)}-([1-9][0-9]{{0,9}})-link', link)
    if number is None or not (profile.parent / link).is_symlink():
        raise ValueError('Nix profile did not select a local generation link.')
    target = os.readlink(profile.parent / link)
    if not re.fullmatch(STORE, target) or not target.endswith('-profile'):
        raise ValueError('Nix profile generation did not select a store profile.')
    store = Path(target)
    if store.is_symlink() or not store.is_dir():
        raise ValueError('Nix profile generation selected an invalid store directory.')
    return int(number[1]), store


def closure_graph(raw, output):
    records = json.loads(raw, object_pairs_hook=unique_fields)
    if not isinstance(records, dict) or len(records) > 8192 or str(output) not in records:
        raise ValueError('Nix did not return the selected output closure.')
    graph = {}
    for path, row in records.items():
        if not re.fullmatch(STORE, path) or not isinstance(row, dict):
            raise ValueError('Nix closure contains an invalid store object.')
        digest, size, refs = row.get('narHash'), row.get('narSize'), row.get('references')
        if (not isinstance(digest, str) or not re.fullmatch(NAR_HASH, digest)
                or type(size) is not int or size <= 0 or not isinstance(refs, list)
                or any(not isinstance(ref, str) or ref not in records for ref in refs)
                or len(refs) != len(set(refs))):
            raise ValueError('Nix closure contains invalid NAR metadata or references.')
        graph[path] = dict(narHash=digest, narSize=size, references=sorted(refs))

    pending, visited = [str(output)], set()
    while pending:
        path = pending.pop()
        if path not in visited:
            visited.add(path)
            pending.extend(graph[path]['references'])
    if visited != graph.keys():
        raise ValueError('Nix closure contains objects unreachable from the selected output.')
    return graph


def store_reference(path, references):
    target = path.resolve(strict=True)
    if str(target) not in references:
        raise ValueError('Installed Nix source is outside its declared store references.')
    return target


def verify_dependencies(dependencies, references, lock_hash):
    references = set(references)
    lock = (dependencies / 'Cargo.lock').resolve(strict=True)
    store_object = re.match(STORE + r'(?=/|$)', str(lock))
    if not store_object or store_object.group() not in {str(dependencies), *references}:
        raise ValueError('Installed Cargo lockfile is outside the dependency closure.')
    crates = validate_lock(read(lock, 4 * 1024 * 1024, lock_hash))
    expected = {'.cargo', 'Cargo.lock', *crates}
    if any(entry.name not in expected for entry in dependencies.iterdir()):
        raise ValueError('Installed Cargo vendor directory contains an unexpected entry.')
    for name, checksum in crates.items():
        crate = store_reference(dependencies / name, references)
        metadata = regular_file(crate, '.cargo-checksum.json')
        info = json.loads(read(metadata, 32 * 1024), object_pairs_hook=unique_fields)
        if crate.name.partition('-')[2] != name or not isinstance(info, dict) or info.get('package') != checksum:
            raise ValueError('Installed crate checksum differs from the reviewed Cargo lockfile.')
    return len(crates)


def live_bridge(launcher, native_image, state, version, env, workspace, registration, *, custom_profile):
    connection = state / 'connection.json'

    def observe_connection():
        try:
            with connection.open('rb') as stream:
                return stream.read(64 * 1024 + 1)
        except FileNotFoundError:
            return b''

    before = observe_connection()
    app = subprocess.Popen([launcher, '--state-dir', state, '--headless'], env=env,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 30
        while True:
            current = observe_connection()
            if len(current) > 64 * 1024:
                raise ValueError('Nix fixture connection exceeds its byte budget.')
            if current and current != before:
                break
            if app.poll() is not None or time.monotonic() >= deadline:
                raise ValueError('Installed Nix launcher did not start a new PeerBrush connection.')
            time.sleep(0.05)
        def discover(args, environment):
            if custom_profile and args[2] == '--connect':
                # The absolute launcher must work without a PATH hint.
                environment = dict(environment, PATH=os.pathsep.join(
                    part for part in environment['PATH'].split(os.pathsep)
                    if part != str(launcher.parent)))
            elif custom_profile and args[2] == '--status':
                # Desktop entries launch the store wrapper. A custom profile's
                # share directory must identify its launcher without a PATH hint.
                args = [launcher.resolve(strict=True), *args[1:]]
                profile_share = launcher.parent.parent / 'share'
                environment = dict(environment, PATH=os.pathsep.join(
                    part for part in environment['PATH'].split(os.pathsep)
                    if part != str(launcher.parent)),
                    XDG_DATA_DIRS=os.pathsep.join([str(profile_share), '/usr/share']))
            return command(args, environment)

        route = registration.check(launcher, native_image, env, discover)
        requests = [
            dict(jsonrpc='2.0', id=1, method='initialize', params=dict(
                protocolVersion='2025-11-25', capabilities={}, clientInfo=dict(name='PeerBrush Nix QA', version='1'))),
            dict(jsonrpc='2.0', method='notifications/initialized'),
            dict(jsonrpc='2.0', id=2, method='tools/call', params=dict(name='peerbrush_projects', arguments={'action': 'list'})),
        ]
        data = ''.join(json.dumps(request) + '\n' for request in requests).encode('utf-8')
        replies = [json.loads(line) for line in command(route, env, data=data).splitlines()]
        if (len(replies) != 2 or any(not isinstance(reply, dict) or reply.get('jsonrpc') != '2.0' or 'error' in reply for reply in replies)
                or [reply.get('id') for reply in replies] != [1, 2]
                or replies[0].get('result', {}).get('serverInfo') != {'name': 'PeerBrush', 'version': version}):
            raise ValueError('Nix MCP launcher did not identify the selected generation.')
        result = replies[1].get('result', {})
        projects = json.loads(next((part['text'] for part in result.get('content', []) if part.get('type') == 'text'), 'null'))
        if result.get('isError') is not False or not isinstance(projects, dict) or not projects.get('active_project_id') or not projects.get('projects'):
            raise ValueError('Nix MCP adapter did not reach the live PeerBrush workspace.')
        workspace.check_launcher(launcher, env)
    finally:
        if app.poll() is None:
            app.terminate()
        try:
            app.wait(timeout=5)
        except subprocess.TimeoutExpired:
            app.kill()
            app.wait(timeout=5)


def verify_desktop_integration(profile, output, source):
    members = {
        DESKTOP: ('packaging/linux/com.peerbrush.PeerBrush.desktop', 64 * 1024),
        MIME: ('packaging/linux/com.peerbrush.PeerBrush.xml', 64 * 1024),
        ICON: ('assets/peerbrush-logo.png', 4 * 1024 * 1024),
    }
    with zipfile.ZipFile(source) as archive:
        entries = archive.infolist()
        for installed, (original, limit) in members.items():
            matches = [entry for entry in entries if entry.filename == original]
            if len(matches) != 1 or not 0 < matches[0].file_size <= limit:
                raise ValueError(f'Pinned source lacks a unique bounded desktop asset: {original}')
            with archive.open(matches[0]) as stream:
                expected = stream.read(limit + 1)
            if len(expected) != matches[0].file_size:
                raise ValueError(f'Pinned desktop asset size differs: {original}')
            if installed == DESKTOP:
                lines = [line.strip() for line in expected.splitlines() if line.strip()]
                required = {
                    b'Exec': [b'Exec=/usr/bin/peerbrush -- %F'],
                    b'TryExec': [b'TryExec=/usr/bin/peerbrush'],
                    b'Icon': [b'Icon=/usr/share/peerbrush/peerbrush-logo.png'],
                    b'Terminal': [b'Terminal=false'],
                    b'MimeType': [b'MimeType=image/vnd.adobe.photoshop;'],
                    b'DBusActivatable': [],
                }
                if ([line for line in lines if line.startswith(b'[')] != [b'[Desktop Entry]']
                        or any([line for line in lines if line.partition(b'=')[0].strip() == key] != values
                               for key, values in required.items())):
                    raise ValueError('Pinned desktop entry does not select the PeerBrush file launcher.')
                expected = (expected.replace(b'/usr/bin/peerbrush', os.fsencode(output / 'bin/peerbrush'))
                            .replace(b'/usr/share/peerbrush/peerbrush-logo.png', os.fsencode(output / ICON)))
            path = regular_file(output, installed)
            if read(path, limit) != expected:
                raise ValueError(f'Installed desktop asset differs from the pinned source: {installed}')
            if installed != ICON and (profile / installed).resolve(strict=True) != path:
                raise ValueError(f'Profile desktop asset does not resolve to the selected output: {installed}')


def inspect_install(launcher, output, definition, graph, state, env, workspace, registration, *, custom_profile):
    inputs, input_hash = definition
    if launcher.resolve(strict=True) != regular_file(output, 'bin/peerbrush'):
        raise ValueError('Stable profile launcher does not resolve to the selected output.')
    native = regular_file(output, 'bin/.peerbrush-wrapped')
    with native.open('rb') as file:
        if file.read(4) != b'\x7fELF' or not os.access(native, os.X_OK):
            raise ValueError('Nix profile does not contain an executable native Linux image.')
    support = output / 'share/peerbrush'
    read(support / 'source-inputs.json', 64 * 1024, input_hash)
    refs = graph[str(output)]['references']
    source = store_reference(support / f'PeerBrush-{inputs["version"]}-source.zip', refs)
    if bounded_file(source, LIMIT)[0] != inputs['source_sha256']:
        raise ValueError('Installed corresponding source differs from the pinned public ZIP.')
    verify_desktop_integration(launcher.parent.parent, output, source)
    dependencies = store_reference(support / 'cargo-dependencies', refs)
    crates = verify_dependencies(dependencies, graph[str(dependencies)]['references'], inputs['cargo_lock_sha256'])
    if command([launcher, '--version'], env) != 'PeerBrush ' + inputs['version']:
        raise ValueError('Nix profile launched another PeerBrush version.')
    build = json.loads(command([launcher, '--build-info'], env))
    expected = dict(schema=1, format='PeerBrushBuildInfoV1', product='PeerBrush', version=inputs['version'],
                    tree='unknown', commit='unknown', end='PeerBrushBuildInfoEnd')
    if build != expected:
        raise ValueError('Nix source archive build provenance differs from the selected version.')
    raw = command([launcher, '--installation-info'], env, limit=16 * 1024).encode('utf-8')
    require_linux_update_owner(raw, 'NixPackage', expected)
    live_bridge(launcher, native, state, inputs['version'], env, workspace, registration,
                custom_profile=custom_profile)
    return crates


def verify_desktop(launcher, output, work, env, workspace):
    root = Path(tempfile.mkdtemp(prefix='desktop-', dir=work))
    profile = launcher.parent.parent
    env = desktop_session_environment(root, dict(env, XDG_DATA_DIRS=f'{profile / "share"}:/usr/local/share:/usr/share'))
    native = regular_file(output, 'bin/.peerbrush-wrapped')
    wrapper = regular_file(output, 'bin/peerbrush')
    entry = profile / DESKTOP
    standard = regular_file(output, DESKTOP)
    if entry.resolve(strict=True) != standard:
        raise ValueError('Desktop entry no longer resolves to the selected Nix output.')
    facts = dict(desktop=read(standard, 64 * 1024).decode('utf-8'),
                 expected=dict(executable_sha256=bounded_file(native, LIMIT)[0]))
    window = Window(facts, root, desktop=entry, connection=Path(env['TMPDIR']) / 'PeerBrush/connection.json',
                    cli=launcher, executable=native, argv0=(os.fsencode(wrapper), os.fsencode(native)), env=env)
    check_startup_window(window, workspace)


def check(older, newer, output, *, nix, nix_sha256, custom_profile=False, desktop=False):
    if (sys.platform != 'linux' or os.uname().machine != 'x86_64' or os.geteuid() == 0
            or os.environ.get('PEERBRUSH_DISPOSABLE_NIX_TEST') != '1'):
        raise ValueError('Select a disposable non-root x86_64 Linux Nix runner.')
    display = isolated_display_environment() if desktop else None
    output = receipt_path(output)
    private_inspection_directory(output.parent, Path(__file__).resolve().parents[1], fresh=False)
    nix = handoff_path(nix)
    if not re.fullmatch(HEX, nix_sha256) or bounded_file(nix, 64 * 1024 * 1024, executable=True)[0] != nix_sha256 or not os.access(nix, os.X_OK):
        raise ValueError('Nix executable differs from the independently selected tool.')
    with tempfile.TemporaryDirectory(prefix='peerbrush-nix-qa-', dir='/tmp') as directory:
        work = Path(directory)
        active, replacement, original = (work / name for name in ('channel', 'newer', 'older'))
        old = snapshot(older, active)
        new = snapshot(newer, replacement) if newer is not None else None
        if new is not None and release_version_order(new[0]['version']) <= release_version_order(old[0]['version']):
            raise ValueError('Nix acceptance requires two increasing application versions.')
        home = work / 'home'
        state, config, cache, data, runtime = (home / name for name in ('.local/state', '.config', '.cache', '.local/share', 'runtime'))
        for path in (state, config, cache, data, runtime):
            path.mkdir(parents=True, mode=0o700)
        workspace = runtime / 'peerbrush-fixture'
        workspace.mkdir(mode=0o700)
        artwork = home / 'artwork'
        artwork.mkdir(mode=0o700)
        native_workspace = PackageWorkspace(workspace, artwork)
        registration = PackageRegistration(work / 'client-home', workspace)
        profile = home / 'profiles/Creative 工具' if custom_profile else state / 'nix/profile'
        profile.parent.mkdir()
        launcher = profile / 'bin/peerbrush'
        fixtures = {workspace / 'preview.json': b'{"version":1,"reduced_resolution":true}\n',
                    home / 'Artwork 作品.psd': b'SYNTHETIC_USER_OWNED_NIX_RETENTION_FIXTURE\n'}
        for path, payload in fixtures.items():
            path.write_bytes(payload)
        env = dict(HOME=str(home), XDG_STATE_HOME=str(state), XDG_CONFIG_HOME=str(config), XDG_CACHE_HOME=str(cache),
                   XDG_DATA_HOME=str(data), XDG_RUNTIME_DIR=str(runtime), PATH=f'{nix.parent}:/usr/bin:/bin', LC_ALL='C')
        if custom_profile:
            env['PATH'] = f'{launcher.parent}:{env["PATH"]}'
        tool_version = command([nix, '--version'], env)
        if not re.fullmatch(r'nix \(Nix\) 2\.35\.[0-9]+', tool_version):
            raise ValueError('Select a reviewed Nix 2.35 CLI.')
        cli = [nix, '--extra-experimental-features', 'nix-command flakes', '--store', 'daemon',
               '--option', 'sandbox', 'true', '--option', 'sandbox-fallback', 'false', '--option', 'builders', '',
               '--option', 'auto-optimise-store', 'false']
        channel = f'path:{active}'
        flake = f'{channel}#peerbrush'
        build = ['build', '--no-link', '--print-out-paths', '--no-write-lock-file']

        def native(*args, limit=256 * 1024):
            return command([*cli, *args], env, timeout=7200, limit=limit)

        def realise(definition):
            path = native(*build, flake)
            if not re.fullmatch(STORE, path) or not path.endswith('-peerbrush-' + definition[0]['version']):
                raise ValueError('Nix build did not produce the expected native package output.')
            return Path(path)

        def flake_source():
            metadata = json.loads(native('flake', 'metadata', '--json', '--no-write-lock-file', channel),
                                  object_pairs_hook=unique_fields)
            url = metadata.get('url') if isinstance(metadata, dict) else None
            if (not isinstance(url, str) or not url.startswith(channel + '?')
                    or metadata.get('originalUrl') != channel):
                raise ValueError('Nix metadata did not describe the selected local flake.')
            # The 2.35 path fetcher can report a resolved URL with only lastModified.
            digest = native('hash', 'path', '--type', 'sha256', '--sri', active)
            if not re.fullmatch(NAR_HASH, digest):
                raise ValueError('Nix did not return the local flake snapshot digest.')
            return (channel, url), digest

        def observe_profile(*, source=None, path=None):
            number, store = profile_generation(profile)
            manifest = json.loads(native('profile', 'list', '--json', '--profile', profile),
                                  object_pairs_hook=unique_fields)
            saved = json.loads(read(regular_file(store, 'manifest.json'), 256 * 1024), object_pairs_hook=unique_fields)
            if manifest != saved:
                raise ValueError('Nix profile listing differs from the active store manifest.')
            verify_profile(manifest, source=source, output=path)
            return number, store

        def inspect_graph(path):
            raw = native('path-info', '--json', '--json-format', '1', '--recursive', path, limit=8 * 1024 * 1024)
            return closure_graph(raw, path)

        def check_generation(definition, path):
            graph = inspect_graph(path)
            native('store', 'verify', '--recursive', path)
            crates = inspect_install(launcher, path, definition, graph, workspace, env, native_workspace,
                                     registration, custom_profile=custom_profile)
            if display is not None:
                verify_desktop(launcher, path, work, dict(env, **display), native_workspace)
            if any(file.is_symlink() or file.read_bytes() != expected for file, expected in fixtures.items()):
                raise ValueError('Nix profile lifecycle changed a user fixture.')
            return graph, crates

        def rebuild(path, graph, crates):
            digest = native('hash', 'path', '--type', 'sha256', '--sri', path)
            if (digest != graph[str(path)]['narHash']
                    or native(*build, '--rebuild', flake) != str(path)
                    or native('hash', 'path', '--type', 'sha256', '--sri', path) != digest
                    or inspect_graph(path) != graph):
                raise ValueError('Nix rebuild did not reproduce the installed output and closure.')
            canonical = json.dumps(graph, sort_keys=True, separators=(',', ':')).encode('ascii')
            return dict(nar_sha256=digest, closure=dict(objects=len(graph), registry_crates=crates,
                                                        graph_sha256=hashlib.sha256(canonical).hexdigest()))

        daemon = json.loads(native('store', 'info', '--json'))
        if daemon.get('url') != 'daemon' or daemon.get('version') != tool_version.split()[-1]:
            raise ValueError('Select a dedicated local Nix daemon matching the reviewed CLI version.')

        print('Checking the first Nix generation and its rebuild.', flush=True)
        first_source, first_flake = flake_source()
        first = realise(old)
        native('profile', 'install', '--profile', profile, '--no-write-lock-file', flake)
        first_profile = latest_profile = observe_profile(source=first_source, path=first)
        first_generation = check_generation(old, first)
        initial = dict(inputs=old[0], generation=first_profile[0], flake_nar_sha256=first_flake,
                       **rebuild(first, *first_generation))
        if new is not None:
            active.rename(original)
            replacement.rename(active)
            print('Checking the Nix profile upgrade and newer rebuild.', flush=True)
            second_source, second_flake = flake_source()
            if second_flake == first_flake:
                raise ValueError('The upgrade did not select a different flake snapshot.')
            second = realise(new)
            native('profile', 'upgrade', '--all', '--profile', profile, '--refresh', '--no-write-lock-file')
            latest_profile = observe_profile(source=second_source, path=second)
            if latest_profile[0] <= first_profile[0] or latest_profile[1] == first_profile[1]:
                raise ValueError('Nix upgrade did not advance to a new profile generation.')
            updated = dict(inputs=new[0], generation=latest_profile[0], flake_nar_sha256=second_flake,
                           **rebuild(second, *check_generation(new, second)))
            print('Checking Nix profile rollback.', flush=True)
            native('profile', 'rollback', '--profile', profile)
            if observe_profile(source=first_source, path=first) != first_profile:
                raise ValueError('Nix rollback did not reselect the original profile generation.')
            if check_generation(old, first) != first_generation:
                raise ValueError('Nix profile rollback did not restore the original closure.')
        print('Checking Nix profile removal and user-file preservation.', flush=True)
        native('profile', 'remove', '--all', '--profile', profile)
        removal_generation, _ = observe_profile()
        if removal_generation <= latest_profile[0]:
            raise ValueError('Nix removal did not create an empty profile generation.')
        if any(os.path.lexists(path) for path in (launcher, profile / DESKTOP, profile / MIME)):
            raise ValueError('Nix profile removal left a launcher, desktop entry or MIME file.')
        if any(file.is_symlink() or file.read_bytes() != expected for file, expected in fixtures.items()):
            raise ValueError('Nix profile removal did not preserve user files.')
        workspace_evidence = native_workspace.retention_receipt()
        registration.preserve()
    evidence = dict(schema=1, tool_sha256=nix_sha256, tool_version=tool_version,
                    profile='private-custom-profile' if custom_profile else 'private-local-flake',
                    removal='verified', removal_generation=removal_generation, update_owner='NixPackage',
                    workspace=workspace_evidence,
                    mcp_registration='private-client-config-backed-up-and-reused',
                    desktop_integration='profile-entry-icon-and-mime-verified',
                    desktop_startup_documents='gio-native8-and-native16-psd-and-psb-verified' if desktop else 'not-performed',
                    user_files='preserved', mcp='live-headless-bridge',
                    public_availability='unverified', release_ready=False)
    if new is None:
        evidence.update(format='PeerBrushNixProfileInstallV1', receipt_sha256=older[1],
                        candidate=initial, install='verified', upgrade='not-performed', rollback='not-performed')
    else:
        evidence.update(format='PeerBrushNixProfileUpgradeV1', older_receipt_sha256=older[1],
                        newer_receipt_sha256=newer[1], older=initial,
                        newer=updated, upgrade='verified', rollback='verified')
    write_receipt(output, evidence)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    selection = ('DEFINITION', 'RECEIPT_SHA256')
    mode.add_argument('--install-only', nargs=2, metavar=selection)
    mode.add_argument('--older', nargs=2, metavar=selection)
    parser.add_argument('--newer', nargs=2, metavar=selection)
    parser.add_argument('--nix', type=Path, required=True)
    parser.add_argument('--nix-sha256', required=True)
    parser.add_argument('--custom-profile', action='store_true')
    parser.add_argument('--desktop', action='store_true', help='Check GIO launch in the isolated Xvfb session.')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    if args.older and args.newer is None:
        parser.error('--older requires --newer')
    if args.install_only and args.newer:
        parser.error('--install-only cannot be combined with --newer')
    check(args.install_only or args.older, args.newer, args.output, nix=args.nix,
          nix_sha256=args.nix_sha256, custom_profile=args.custom_profile, desktop=args.desktop)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
