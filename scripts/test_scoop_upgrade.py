"""Qualify a first install or upgrade through Scoop on disposable Windows x64."""
import argparse
from contextlib import contextmanager
import ctypes
from ctypes import wintypes
from datetime import datetime
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import time
import zipfile

from final_release import LIMIT, read
from package import regular_file, relative_path, release_version_order
from package_macos_dmg import bounded_file, inspection_path, private_inspection_directory
from prepare_package_managers import definition, require_support_inventory, verify_installed_support
from prepare_site_release import DOWNLOAD, STABLE
from production_readiness import handoff_path, unique_fields
from test_deb_installation import receipt_path, write_receipt
from test_deb_version_upgrade import PackageRegistration, PackageWorkspace, fixture_rpc
from test_msix_runtime import smoke


DIGEST = r'[0-9a-f]{64}'
COMMIT = r'[0-9a-f]{40}'
BUCKET = 'peerbrush-fixture'
SHELL_DESKTOP = 'installed-shortcut-rendered-frame-and-clean-close'
DESKTOP_CONNECT = 'connect-ai-button-private-config-and-live-stdio'


def select(selection, *, for_install=False):
    directory, digest = selection
    directory = handoff_path(directory)
    if not re.fullmatch(DIGEST, digest):
        raise ValueError('Select an independently authenticated definition receipt SHA-256.')
    report = json.loads(read(directory / 'package-managers.json', 128 * 1024, digest), object_pairs_hook=unique_fields)
    if not isinstance(report, dict):
        raise ValueError('Scoop definition receipt must be an object.')
    tag = report.get('version', '')
    if (report.get('schema') != 1 or report.get('published') is not False or report.get('release_ready') is not False
            or not isinstance(tag, str) or not re.fullmatch('v' + STABLE, tag)
            or not re.fullmatch(COMMIT, report.get('commit', ''))):
        raise ValueError('Select an unpublished Scoop definition from the release verifier.')
    version = tag[1:]
    prefix = f'PeerBrush-{version}-Windows-x64'
    channel = report.get('channels', {}).get('scoop', {})
    asset = channel.get('asset')
    if (channel.get('definition_prepared') is not True or not isinstance(asset, dict)
            or not re.fullmatch(re.escape(prefix) + r'(?:-repacked-[0-9]{8})?\.zip', asset.get('file', ''))
            or not re.fullmatch(COMMIT, asset.get('binary_source_commit', ''))
            or any(not re.fullmatch(DIGEST, asset.get(name, '')) for name in ('sha256', 'executable_sha256', 'source_sha256'))
            or asset.get('url') != DOWNLOAD + tag + '/' + asset['file']):
        raise ValueError('Scoop receipt lacks authenticated Windows archive and installed payload bindings.')
    if for_install or 'support_files' in asset:
        require_support_inventory(asset)
    checksum = report.get('files', {}).get('peerbrush.json', '')
    if not re.fullmatch(DIGEST, checksum):
        raise ValueError('Scoop receipt does not identify its manifest.')
    manifest = read(directory / 'peerbrush.json', 128 * 1024, checksum)
    inspected = dict(manifest=dict(platform='Windows', architecture='x64', version=version),
                     prefix=prefix + '/', record=asset)
    if manifest != definition('scoop', None, inspected, asset['url']):
        raise ValueError('Scoop manifest differs from the generated hook-free Windows definition.')
    return version, asset, manifest


def extract_scoop(archive, checksum, destination):
    """Install no tooling. Read the selected stock source archive into the private fixture."""
    if not re.fullmatch(DIGEST, checksum):
        raise ValueError('Select the reviewed Scoop source archive SHA-256.')
    with zipfile.ZipFile(io.BytesIO(read(archive, 32 * 1024 * 1024, checksum))) as source:
        items = source.infolist()
        if len(items) > 4096 or sum(item.file_size for item in items) > 64 * 1024 * 1024:
            raise ValueError('Scoop source archive exceeds its inspection bounds.')
        seen, roots = set(), set()
        for item in items:
            name = relative_path(item.filename.removesuffix('/') if item.is_dir() else item.filename)
            mode = stat.S_IFMT(item.external_attr >> 16)
            if (str(name).casefold() in seen or any(part.casefold() == '.git' for part in name.parts)
                    or mode not in (0, stat.S_IFDIR if item.is_dir() else stat.S_IFREG)):
                raise ValueError('Scoop source archive contains ambiguous paths or unsupported members.')
            seen.add(str(name).casefold())
            roots.add(name.parts[0])
            if len(roots) != 1:
                raise ValueError('Select one complete Scoop source archive.')
            target = destination.joinpath(*name.parts[1:])
            if item.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open('xb') as stream:
                    stream.write(source.read(item))
    return regular_file(destination, 'bin/scoop.ps1')


def command(args, env, work, *, timeout=45, capture=False):
    with tempfile.TemporaryFile(dir=work) as output:
        subprocess.run(args, env=env, cwd=work, stdin=subprocess.DEVNULL,
                       stdout=output if capture else subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       timeout=timeout, check=True)
        if output.tell() > 64 * 1024:
            raise ValueError('Scoop fixture command output exceeds its byte budget.')
        output.seek(0)
        return output.read().decode('utf-8').strip()


def native_runner():
    if sys.platform != 'win32' or os.environ.get('PEERBRUSH_DISPOSABLE_SCOOP_TEST') != '1':
        raise ValueError('Use a disposable native Windows x64 VM with a fresh standard user.')
    process_machine, native_machine = wintypes.USHORT(), wintypes.USHORT()
    kernel = ctypes.windll.kernel32
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.IsWow64Process2.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.USHORT), ctypes.POINTER(wintypes.USHORT)]
    if (not kernel.IsWow64Process2(kernel.GetCurrentProcess(), ctypes.byref(process_machine), ctypes.byref(native_machine))
            or native_machine.value != 0x8664 or process_machine.value or ctypes.windll.shell32.IsUserAnAdmin()):
        raise ValueError('Run 64-bit Python on Windows x64 without administrator privileges.')


def verify_cached_archive(cache, candidate):
    version, asset, _ = candidate
    url = asset['url']
    # Stock Scoop uses a short URL hash and also recognizes its legacy URL filename.
    prefix = f'peerbrush#{version}#'
    names = [prefix + hashlib.sha256(url.encode('utf-8')).hexdigest()[:7] + '.zip',
             prefix + re.sub(r'[^A-Za-z0-9_.-]+', '_', url)]
    cache = handoff_path(cache)
    available = [name for name in names if os.path.lexists(cache / name)]
    if len(available) != 1 or bounded_file(regular_file(cache, available[0]), LIMIT)[0] != asset['sha256']:
        raise ValueError('Scoop cache does not contain one authenticated archive for the selected generation.')


def verify_payload(root, candidate):
    version, asset, manifest = candidate
    installed = handoff_path(root / 'apps' / 'peerbrush' / version)
    executable = regular_file(installed, 'peerbrush.exe')
    if bounded_file(executable, LIMIT, executable=True)[0] != asset['executable_sha256']:
        raise ValueError('Scoop executable differs from the authenticated Windows release.')
    verify_installed_support(installed, version, asset)

    def scoop_record(preferred, legacy):
        path = installed / preferred
        raw = read(path if os.path.lexists(path) else installed / legacy, 128 * 1024)
        return json.loads(raw.decode('utf-8-sig'), object_pairs_hook=unique_fields)

    record = scoop_record('scoop-install.json', 'install.json')
    if (not isinstance(record, dict) or record.get('bucket') != BUCKET or record.get('architecture') != '64bit'
            or record.get('url') is not None
            or scoop_record('scoop-manifest.json', 'manifest.json') != json.loads(manifest)):
        raise ValueError('Native Scoop receipt differs from the selected catalog generation.')
    return executable


def verify_current(launcher, executable):
    current = launcher.parent
    info = current.lstat()
    if (not stat.S_ISDIR(info.st_mode)
            or getattr(info, 'st_reparse_tag', None) != stat.IO_REPARSE_TAG_MOUNT_POINT
            or current.resolve(strict=True) != executable.parent
            or launcher.resolve(strict=True) != executable):
        raise ValueError('Scoop current is not a junction to the authenticated Windows installation.')


def verify_shim_target(raw, launcher):
    match = re.fullmatch(r'path = "([^"\r\n]+)"\r?\n?', raw.decode('utf-8-sig'))
    if not match or Path(match[1]) != launcher:
        raise ValueError('Scoop executable shim does not select the stable launcher without extra arguments.')


def verify_shortcut(raw, launcher):
    record = json.loads(raw, object_pairs_hook=unique_fields)
    if (not isinstance(record, dict) or not isinstance(record.get('target'), str)
            or not isinstance(record.get('directory'), str) or Path(record['target']) != launcher
            or Path(record['directory']) != launcher.parent or record.get('arguments') != ''):
        raise ValueError('Scoop Start menu shortcut did not retain the stable launcher and working directory.')
    return Path(record['target']), Path(record['directory'])


@contextmanager
def shell_application(shortcut, target, environment):
    import _winapi

    class ExecuteInfo(ctypes.Structure):
        _fields_ = [
            ('cbSize', wintypes.DWORD), ('fMask', wintypes.ULONG), ('hwnd', wintypes.HWND),
            ('lpVerb', wintypes.LPCWSTR), ('lpFile', wintypes.LPCWSTR), ('lpParameters', wintypes.LPCWSTR),
            ('lpDirectory', wintypes.LPCWSTR), ('nShow', ctypes.c_int), ('hInstApp', wintypes.HINSTANCE),
            ('lpIDList', ctypes.c_void_p), ('lpClass', wintypes.LPCWSTR), ('hkeyClass', wintypes.HKEY),
            ('dwHotKey', wintypes.DWORD), ('hIconOrMonitor', wintypes.HANDLE), ('hProcess', wintypes.HANDLE),
        ]

    shell = ctypes.WinDLL('shell32', use_last_error=True)
    shell.ShellExecuteExW.argtypes = [ctypes.POINTER(ExecuteInfo)]
    shell.ShellExecuteExW.restype = wintypes.BOOL
    ole = ctypes.WinDLL('ole32')
    ole.CoInitializeEx.argtypes = [ctypes.c_void_p, wintypes.DWORD]
    ole.CoInitializeEx.restype = ctypes.c_long
    ole.CoUninitialize.argtypes = []
    ole.CoUninitialize.restype = None
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.GetProcessId.argtypes = [wintypes.HANDLE]
    kernel.GetProcessId.restype = wintypes.DWORD
    kernel.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD),
    ]
    kernel.QueryFullProcessImageNameW.restype = wintypes.BOOL

    # Keep the process handle, finish the Shell handoff, and suppress Shell error UI.
    request = ExecuteInfo(cbSize=ctypes.sizeof(ExecuteInfo), fMask=0x40 | 0x100 | 0x400,
                          lpVerb='open', lpFile=str(shortcut), nShow=1)
    original = {key: os.environ.get(key) for key in environment}
    if ole.CoInitializeEx(None, 0x2 | 0x4) < 0:
        raise ValueError('Could not initialize COM for Windows Shell shortcut activation.')
    try:
        os.environ.update(environment)
        if not shell.ShellExecuteExW(ctypes.byref(request)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        for key, value in original.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        ole.CoUninitialize()
    if not request.hProcess:
        raise ValueError('Windows Shell did not return a new process for the installed shortcut.')

    handle = request.hProcess
    try:
        pid = kernel.GetProcessId(handle)
        executable = ctypes.create_unicode_buffer(32768)
        size = wintypes.DWORD(len(executable))
        if (not pid or not kernel.QueryFullProcessImageNameW(handle, 0, executable, ctypes.byref(size))
                or not Path(executable.value).samefile(target)):
            raise ValueError('The Shell shortcut did not start the verified installed executable.')

        class Application:
            def __init__(self):
                self.pid = pid

            def wait(self, timeout):
                if _winapi.WaitForSingleObject(handle, int(timeout * 1000)) == _winapi.WAIT_TIMEOUT:
                    raise subprocess.TimeoutExpired(str(shortcut), timeout)
                return _winapi.GetExitCodeProcess(handle)

            def poll(self):
                try:
                    return self.wait(0)
                except subprocess.TimeoutExpired:
                    return None

        app = Application()
        try:
            yield app
        finally:
            if app.poll() is None:
                try:
                    _winapi.TerminateProcess(handle, 1)
                except PermissionError:
                    if app.poll() is None:
                        raise
            app.wait(timeout=10)
    finally:
        _winapi.CloseHandle(handle)


def verify_desktop_session(app, target, state, env, ps, workspace=None, *, before_close=None):
    connection = state / 'connection.json'
    capture = state / 'workspace.png'
    def wait_for(path):
        deadline = time.monotonic() + 60
        while app.poll() is None and time.monotonic() < deadline:
            if path.is_file():
                return regular_file(state, path.name)
            time.sleep(0.1)
        raise ValueError('The Scoop desktop app did not publish its connection or rendered frame.')

    session = json.loads(read(wait_for(connection), 64 * 1024), object_pairs_hook=unique_fields)
    if not isinstance(session, dict) or session.get('pid') != app.pid:
        raise ValueError('The Scoop desktop connection belongs to a different process.')

    def rpc(method, parameters):
        if app.poll() is not None:
            raise ValueError('The Scoop desktop app exited before its workspace could be checked.')
        return fixture_rpc([target, '--state-dir', state], env, method, parameters)

    if workspace is not None:
        workspace.check_startup_projects(rpc)
    rpc('capture_ui', dict(path=str(capture), panel='workspace'))
    bounded_file(wait_for(capture), 64 * 1024 * 1024)
    if before_close is not None:
        before_close()
    if app.poll() is not None:
        raise ValueError('The Scoop desktop app exited before its window could be checked.')
    observed = command([*ps, '-Command',
        '$ErrorActionPreference = "Stop"; Add-Type -AssemblyName System.Drawing; '
        '$app = Get-Process -Id ([int]$env:PEERBRUSH_QA_PID); '
        'if ($app.Path -notin @($env:PEERBRUSH_QA_TARGET, $env:PEERBRUSH_QA_EXECUTABLE) '
        '-or $app.MainWindowHandle -eq [IntPtr]::Zero) { exit 1 }; '
        '$image = [System.Drawing.Image]::FromFile($env:PEERBRUSH_QA_FRAME); '
        'try { if ($image.RawFormat.Guid -ne [System.Drawing.Imaging.ImageFormat]::Png.Guid '
        '-or $image.Width -lt 320 -or $image.Height -lt 240) { exit 1 }; '
        '$null = $image.GetPixel(0, 0) } finally { $image.Dispose() }; '
        'if ($app.CloseMainWindow()) { "closed" }'],
        dict(env, PEERBRUSH_QA_PID=str(app.pid), PEERBRUSH_QA_FRAME=str(capture),
             PEERBRUSH_QA_TARGET=str(target), PEERBRUSH_QA_EXECUTABLE=str(target.resolve(strict=True))),
        state, capture=True)
    if observed != 'closed' or app.wait(timeout=15) != 0:
        raise ValueError('The Scoop desktop app did not close its rendered window cleanly.')


def verify_desktop(target, directory, env, ps, workspace, registration):
    state = registration.state
    env = registration.environment(env)
    workspace.preserve()
    app = subprocess.Popen([target, '--state-dir', state, '--', *workspace.projects], env=env, cwd=directory,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def wait_for_connect():
        original = registration.config.stat()
        print('Click Connect AI once in the installed PeerBrush window within 120 seconds.', flush=True)
        deadline = time.monotonic() + 120
        while app.poll() is None and time.monotonic() < deadline:
            current = registration.config.stat()
            if (current.st_ino, current.st_mtime_ns) != (original.st_ino, original.st_mtime_ns):
                return
            time.sleep(0.2)
        raise ValueError('Desktop Connect AI did not update the private fixture client configuration.')

    def check_registration():
        route = registration.check(target, target.resolve(strict=True), env,
                                   lambda args, environment: command(args, environment, state, capture=True),
                                   desktop_connect=wait_for_connect)
        smoke(route[0], Path(route[2]), allow_portable=True, env=env)
        registration.preserve()

    try:
        verify_desktop_session(app, target, state, env, ps, workspace, before_close=check_registration)
    finally:
        if app.poll() is None:
            app.terminate()
        app.wait(timeout=10)
    # A later generation must publish its own connection and frame in the same workspace.
    (state / 'connection.json').unlink(missing_ok=True)
    (state / 'workspace.png').unlink()


def untouched_default_workspace(env):
    roots = {env['APPDATA'], env['LOCALAPPDATA'], tempfile.gettempdir()}
    roots.update(os.environ[key] for key in ('TEMP', 'TMP') if os.environ.get(key))
    if any(not Path(root).is_absolute() or os.path.lexists(Path(root) / 'PeerBrush') for root in roots):
        raise ValueError('The disposable account must have no existing default PeerBrush workspace.')


def verify_shell_desktop(shortcut, shortcut_sha256, target, work, env, ps):
    untouched_default_workspace(env)
    work.mkdir()
    paths = {key: work / name for key, name in [('APPDATA', 'roaming'), ('LOCALAPPDATA', 'local'), ('TEMP', 'temp')]}
    for directory in paths.values():
        directory.mkdir()
    paths['TMP'] = paths['TEMP']
    shell_env = dict(env, **{key: str(value) for key, value in paths.items()})
    state = paths['TEMP'] / 'PeerBrush'
    read(shortcut, 64 * 1024, shortcut_sha256)
    try:
        with shell_application(shortcut, target, shell_env) as app:
            verify_desktop_session(app, target, state, shell_env, ps)
    finally:
        untouched_default_workspace(env)
    read(shortcut, 64 * 1024, shortcut_sha256)


def verify_install(root, launcher, candidate, state, env, workspace, *, require_owner):
    version, asset, _ = candidate
    executable = verify_payload(root, candidate)
    verify_current(launcher, executable)
    shim = regular_file(root, 'shims/peerbrush.exe')
    verify_shim_target(read(root / 'shims/peerbrush.shim', 16 * 1024), launcher)
    if command([shim, '--version', '--state-dir', state], env, state, capture=True) != 'PeerBrush ' + version:
        raise ValueError('The Scoop executable shim did not launch the selected generation.')

    connection = state / 'connection.json'

    def observe_connection():
        try:
            with connection.open('rb') as stream:
                return stream.read(64 * 1024 + 1)
        except FileNotFoundError:
            return b''

    before = observe_connection()
    app = subprocess.Popen([launcher, '--state-dir', state, '--headless'], env=env, cwd=state,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 30
        while True:
            current = observe_connection()
            if len(current) > 64 * 1024:
                raise ValueError('Scoop fixture connection exceeds its byte budget.')
            if current and current != before:
                break
            if app.poll() is not None or time.monotonic() >= deadline:
                raise ValueError('Installed Scoop launcher did not start a new PeerBrush connection.')
            time.sleep(0.05)
        observation = smoke(launcher, state, allow_portable=True, env=env)['installation_observation']
        build = observation['build']
        owner = observation.get('update_owner')
        if (observation['package'] is not None or build['version'] != version or build['tree'] != 'clean'
                or build['commit'] != asset['binary_source_commit']
                or ((owner is not None or require_owner) and owner != 'ScoopPackage')):
            raise ValueError('Installed PeerBrush provenance or Scoop update owner differs from the selection.')
        workspace.check_launcher(launcher, env)
        return owner == 'ScoopPackage'
    finally:
        if app.poll() is None:
            app.terminate()
        app.wait(timeout=10)


def check(older, newer, output, *, scoop_archive, scoop_sha256):
    native_runner()
    root = Path(__file__).resolve().parents[1]
    output = receipt_path(output)
    private_inspection_directory(output.parent, root, fresh=False)
    old = select(older, for_install=True)
    new = select(newer, for_install=True) if newer is not None else None
    if new is not None and release_version_order(new[0]) <= release_version_order(old[0]):
        raise ValueError('Scoop acceptance requires two increasing application versions.')
    scratch = private_inspection_directory(Path(tempfile.gettempdir()).resolve(), root, fresh=False)
    with tempfile.TemporaryDirectory(prefix='peerbrush-scoop-qa-', dir=scratch) as directory:
        work = private_inspection_directory(Path(directory), root, fresh=False)
        install, config, state, tmp, cache = (work / name for name in ('scoop', 'config', 'workspace', 'tmp', 'cache'))
        script = extract_scoop(scoop_archive, scoop_sha256, install / 'apps/scoop/current')
        for path in (config / 'scoop', state, tmp):
            path.mkdir(parents=True)
        system = handoff_path(os.environ['SYSTEMROOT'])
        shell = regular_file(system, 'System32/WindowsPowerShell/v1.0/powershell.exe')
        ps = [shell, '-NoLogo', '-NoProfile', '-NonInteractive']
        env = {key: os.environ[key] for key in ('SYSTEMROOT', 'WINDIR', 'USERPROFILE', 'USERNAME', 'APPDATA',
                                               'LOCALAPPDATA', 'PROGRAMDATA', 'COMSPEC', 'OS', 'SYSTEMDRIVE',
                                               'PROCESSOR_ARCHITECTURE') if key in os.environ}
        env.update(PATH=os.pathsep.join(map(str, [system / 'System32', system, shell.parent])),
                   TEMP=str(tmp), TMP=str(tmp), XDG_CONFIG_HOME=str(config), SCOOP=str(install),
                   SCOOP_GLOBAL=str(work / 'global'), SCOOP_CACHE=str(cache))
        untouched_default_workspace(env)
        get_start = '[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); [Environment]::GetFolderPath("StartMenu")'
        start = Path(command([*ps, '-Command', get_start], env, work, capture=True))
        if not start.is_absolute() or not handoff_path(start).is_relative_to(handoff_path(env['APPDATA'])):
            raise ValueError('The disposable account needs a local user Start menu.')
        shortcut = start / 'Programs/Scoop Apps/PeerBrush.lnk'
        for path in (shortcut.parent.parent, shortcut.parent, shortcut):
            inspection_path(path)
            if not path.exists():
                break
        else:
            raise ValueError('The disposable account already has a Scoop PeerBrush shortcut.')
        (config / 'scoop/config.json').write_text(json.dumps(dict(last_update=datetime.now().astimezone().isoformat(),
            aria2_enabled=False, no_junction=False, use_external_7zip=False, use_isolated_path=True,
            use_sqlite_cache=False)), encoding='utf-8')
        bucket = install / 'buckets' / BUCKET / 'bucket'
        bucket.mkdir(parents=True)
        manifest = bucket / 'peerbrush.json'
        launcher = install / 'apps/peerbrush/current/peerbrush.exe'
        artwork = work / 'artwork'
        artwork.mkdir()
        workspace = PackageWorkspace(state, artwork)
        desktop_state = work / 'desktop'
        desktop_state.mkdir()
        registration = PackageRegistration(work / 'client-home', desktop_state)
        fixtures = {state / 'preview.json': b'{"version":1,"reduced_resolution":true}\n',
                    work / 'Artwork 作品.psd': b'SYNTHETIC_USER_OWNED_SCOOP_RETENTION_FIXTURE\n'}
        for path, payload in fixtures.items():
            path.write_bytes(payload)

        def scoop(*args):
            command([*ps, '-File', script, *args], env, work, timeout=1800)

        def verify(candidate, *, require_owner):
            verify_cached_archive(cache, candidate)
            result = verify_install(install, launcher, candidate, state, env, workspace,
                                    require_owner=require_owner)
            shortcut_path = regular_file(start, 'Programs/Scoop Apps/PeerBrush.lnk')
            shortcut_sha256 = hashlib.sha256(read(shortcut_path, 64 * 1024)).hexdigest()
            observed = command([*ps, '-Command',
                '[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); '
                '$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($env:PEERBRUSH_QA_SHORTCUT); '
                '[pscustomobject]@{target=$shortcut.TargetPath; directory=$shortcut.WorkingDirectory; '
                'arguments=$shortcut.Arguments} | ConvertTo-Json -Compress'],
                dict(env, PEERBRUSH_QA_SHORTCUT=str(shortcut_path)), work, capture=True)
            target, directory = verify_shortcut(observed, launcher)
            verify_desktop(target, directory, env, ps, workspace, registration)
            verify_shell_desktop(shortcut_path, shortcut_sha256, target,
                                 work / f'shell-{candidate[0]}', env, ps)
            verify_fixtures()
            return result

        def verify_fixtures():
            if any(read(path, 64 * 1024) != payload for path, payload in fixtures.items()):
                raise ValueError('Scoop changed a user fixture.')

        print('Checking Scoop install and the first live MCP connection.', flush=True)
        manifest.write_bytes(old[2])
        scoop('install', '--no-update-scoop', '--arch', '64bit', f'{BUCKET}/peerbrush')
        old_owner = verify(old, require_owner=new is None)
        if new is not None:
            print('Checking Scoop upgrade and the same live MCP launcher.', flush=True)
            manifest.write_bytes(new[2])
            scoop('update', 'peerbrush')
            verify(new, require_owner=True)
            verify_payload(install, old)
        print('Checking Scoop removal and user-file preservation.', flush=True)
        scoop('uninstall', 'peerbrush')
        if any(os.path.lexists(path) for path in (install / 'apps/peerbrush', shortcut,
                                                install / 'shims/peerbrush.exe', install / 'shims/peerbrush.shim')):
            raise ValueError('Scoop did not remove the installed versions and application launch paths.')
        verify_fixtures()
        workspace_evidence = workspace.retention_receipt()
        registration.preserve()
    evidence = dict(schema=1, scoop_source_sha256=scoop_sha256, mcp='live-headless-bridge',
                    install='verified', removal='verified', user_files='preserved',
                    workspace=workspace_evidence,
                    mcp_registration='private-client-config-backed-up-and-reused',
                    archive_cache='verified', current_junction='verified', start_menu_and_shim='verified',
                    desktop_launcher='rendered-frame-and-clean-close',
                    desktop_startup_documents='native8-and-native16-psd-and-psb-verified',
                    shell_desktop=SHELL_DESKTOP, desktop_connect=DESKTOP_CONNECT,
                    publisher_trust='unverified', public_availability='unverified', release_ready=False)
    initial = dict(version=old[0], definition_sha256=older[1], archive_sha256=old[1]['sha256'],
                   owner_observed=old_owner)
    if new is None:
        evidence.update(format='PeerBrushScoopInstallV1', candidate=initial, upgrade='not-performed')
    else:
        evidence.update(format='PeerBrushScoopUpgradeV1', older=initial, upgrade='verified', previous_payload_preserved=True,
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
    parser.add_argument('--scoop-archive', type=Path, required=True)
    parser.add_argument('--scoop-sha256', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    if args.older and args.newer is None:
        parser.error('--older requires --newer')
    if args.install_only and args.newer:
        parser.error('--install-only cannot be combined with --newer')
    check(args.install_only or args.older, args.newer, args.output,
          scoop_archive=args.scoop_archive, scoop_sha256=args.scoop_sha256)


if __name__ == '__main__':
    main()
