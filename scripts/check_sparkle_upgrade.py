"""Drive real signed PeerBrush installations on an isolated, credential-free hosted Mac."""
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import sys
import tarfile
import tempfile
import threading
import time

from package import digest
from sparkle_upgrade_evidence import CASES, validate
from test_deb_version_upgrade import PROJECT_SIZE, png_samples


TEMP = Path(os.environ.get('RUNNER_TEMP', '.')).resolve()
START, FINAL = '0.29.99', '0.30.0'


def wait(label, predicate, seconds=90):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        result = predicate()
        if result:
            return result
        time.sleep(0.15)
    raise RuntimeError('Timed out waiting for ' + label)


def ui(pid, action, text):
    def observe():
        result = subprocess.run([TEMP / 'lab-ui', str(pid), action, text], timeout=15, stderr=subprocess.DEVNULL)
        return result.returncode == 0
    wait('visible editor: ' + action + ' ' + text, observe)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        srv = self.server
        if self.path == '/stable/appcast.xml':
            srv.requested.set()
            srv.feed_gate.wait(16)
            if srv.fail:
                self.send_error(503)
                return
            path = srv.feeds[srv.current]
        elif self.path == '/downloads/v' + FINAL + '/' + srv.image.name:
            srv.downloading.set()
            path = srv.image
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Length', str(path.stat().st_size))
        self.send_header('Content-Type', 'application/xml' if path.suffix == '.xml' else 'application/octet-stream')
        self.end_headers()
        try:
            with path.open('rb') as source:
                if path == srv.image:
                    self.wfile.write(source.read(4096))
                    srv.download_gate.wait(45)
                shutil.copyfileobj(source, self.wfile)
        except (BrokenPipeError, ConnectionResetError):
            pass


def unpack():
    names = {}
    for version, folder, zip_rev, dmg_rev in [
            (START, 'baseline-signed', '1', '1'),
            (FINAL, 'macos-signed', os.environ['ZIP_REVISION'], os.environ['DMG_REVISION'])]:
        stem = f'{folder}/PeerBrush-{version}-macOS-arm64-apple-signed-r{zip_rev}'
        names[version] = [stem + '.zip', stem + '-dmg-signed-r' + dmg_rev + '.dmg', f'lab-feed-{version}/appcast.xml']
    allowed = {p for paths in names.values() for p in paths}
    root = TEMP / 'private-pairs'
    root.mkdir(mode=0o700)
    with tarfile.open(TEMP / 'lab.tar', 'r:') as archive:
        for member in archive:
            if (member.name not in allowed or not member.isfile() or not 0 < member.size <=
                    (256 * 1024 if member.name.endswith('.xml') else 512 * 1024 * 1024)):
                raise ValueError('Unexpected private delivery member.')
            allowed.remove(member.name)
            path = root / member.name
            path.parent.mkdir(mode=0o700, exist_ok=True)
            with archive.extractfile(member) as src, path.open('xb') as dest:
                shutil.copyfileobj(src, dest)
    if allowed:
        raise ValueError('Incomplete private delivery.')
    return {v: [root / p for p in paths] for v, paths in names.items()}


def inspect_pair(version, paths):
    root = Path(__file__).resolve().parents[1]
    if version == START:
        root = TEMP / 'peerbrush-sparkle-baseline'
    commit = subprocess.check_output(['git', '-C', root, 'rev-parse', 'HEAD'], text=True).strip()
    archive, image, _ = paths
    stage = TEMP / ('mounted-' + version)
    # The fixture's exact five-file delta includes its plist contract.
    code = ("from pathlib import Path; import sys; from package_macos_dmg import inspect_final; "
            "v,c,z,d,zh,dh,w,out=sys.argv[1:]; "
            "inspect_final(Path.cwd(),Path(z),Path(d),version=v,commit=c,archive_sha256=zh,image_sha256=dh,"
            "team_id='KU3Q7F9Q84',work=Path(w),copy_mounted_to=Path(out))")
    subprocess.run([sys.executable, '-B', '-c', "import sys;sys.path.insert(0,'scripts');" + code,
                    version, commit, archive, image, digest(archive), digest(image), TEMP / ('verify-' + version),
                    stage], cwd=root, check=True, timeout=480)
    return stage / 'PeerBrush.app'


def run_case(case, server, starting, final):
    # Keep every app ancestor within the normal user's protected installation hierarchy.
    home = Path(tempfile.mkdtemp(prefix='PeerBrush-lab-', dir=Path.home()))
    apps = home / 'Applications'
    tmp = home / 'tmp'
    for directory in (apps, tmp):
        directory.mkdir(mode=0o700)
    app = apps / 'PeerBrush.app'
    subprocess.run(['/usr/bin/ditto', starting, app], check=True, timeout=120)
    exe = app / 'Contents/MacOS/peerbrush'
    source_hash = digest(exe)
    final_hash = digest(final / 'Contents/MacOS/peerbrush')
    env = dict(PATH='/usr/bin:/bin', HOME=str(home), TMPDIR=str(tmp), LANG='en_US.UTF-8')
    artwork = home / 'private.psd'
    width, height = PROJECT_SIZE
    rgb = (0xc301, 0x3f29, 0x15a7)
    original = struct.pack('>4sH6xHIIHHIIIH', b'8BPS', 1, 3, height, width, 16, 3, 0, 0, 0, 0)
    original += b''.join(struct.pack('>H', value) * (width * height) for value in rgb)
    artwork.write_bytes(original)
    active = tmp / 'PeerBrush'
    data = home / 'Library/Application Support/PeerBrush'
    config = data / 'Config'
    live_settings = config / 'settings-v1/tool-settings.json'
    checkpoints = data / 'Data/update-checkpoints'
    connection = active / 'connection.json'
    processes = []
    extra_editors = set()
    def launch():
        process = subprocess.Popen([exe, '--open', artwork], env=env, cwd=home,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        processes.append(process)
        return process
    def pid_at(path):
        try:
            return json.loads(path.read_bytes())['pid']
        except (FileNotFoundError, KeyError, ValueError):
            return None
    def rpc(method, state=None, **params):
        cmd = [exe, 'cli', method, json.dumps(dict(actor='human', **params))]
        if state is not None:
            cmd.extend(['--state-dir', state])
        reply = json.loads(subprocess.check_output(cmd, env=env, cwd=home, timeout=20))
        if not reply.get('ok'):
            raise ValueError('The editor refused ' + method)
        return reply['result']
    def pixels(state=None):
        projects = rpc('projects', state, action='list')['projects']
        matches = [p for p in projects if p.get('path') == str(artwork) and p.get('loading') is None]
        if len(matches) != 1:
            return False
        project = matches[0]['project_id']
        doc = rpc('observe', state, project_id=project, max_edge=32, image=False)['state']['document']
        output = home / ('pixels-' + str(time.monotonic_ns()) + '.png')
        rpc('document', state, action='export', project_id=project, document_id=doc['id'],
            expected_revision=doc['revision'], path=str(output))
        if png_samples(output) != (width, height, 16, struct.pack('>4H', *rgb, 65535) * (width * height)):
            raise ValueError('The installed editor changed native PSD samples.')
        return True
    def dirty_layer(create=False):
        project, = [p for p in rpc('projects', action='list')['projects'] if p['path'] == str(artwork)]
        guard = dict(project_id=project['project_id'], document_id=project['document_id'], expected_revision=project['revision'])
        name = 'Unsaved upgrade test layer'
        if create:
            rpc('edit', **guard, commands=[{'op': 'layer.add', 'name': name}], feedback='request')
            return dirty_layer()
        state = rpc('observe', project_id=guard['project_id'], image=False, max_edge=32)['state']
        if not state['dirty'] or not any(layer['name'] == name for layer in state['layers']):
            raise ValueError('The updater discarded the test edit or marked it saved.')
        return guard
    before = launch()
    try:
        wait('initial project', lambda: pid_at(connection) == before.pid and pixels())
        saved = rpc('tool_settings', action='set', tool='brush', settings=dict(radius=31.5, opacity=0.375))['profile']
        saved_bytes = live_settings.read_bytes()
        ui(before.pid, 'click', 'File')
        ui(before.pid, 'click', 'Updates')
        ui(before.pid, 'see', 'Installed version ' + START)
        dirty = None
        if case in ('no-update', 'network-error', 'cancel-check'):
            dirty = dirty_layer(create=True)
        server.current = START if case == 'no-update' else FINAL
        server.fail = case == 'network-error'
        server.requested.clear()
        server.downloading.clear()
        server.feed_gate.set()
        server.download_gate.set()
        if case == 'cancel-check':
            server.feed_gate.clear()
        if case == 'cancel-download':
            server.download_gate.clear()
        ui(before.pid, 'click', 'Check app updates')
        wait('real discovery request', server.requested.is_set)
        if case in ('no-update', 'network-error', 'cancel-check'):
            if case == 'cancel-check':
                ui(before.pid, 'click', 'Cancel app check')
                server.feed_gate.set()
                ui(before.pid, 'see', 'App update check cancelled')
            elif case == 'no-update':
                ui(before.pid, 'see', 'No compatible signed app update')
            else:
                ui(before.pid, 'see', 'App update checking could not complete')
            if checkpoints.exists() and list(checkpoints.iterdir()) or server.downloading.is_set():
                raise ValueError('Discovery touched the install checkpoint or downloaded an enclosure.')
        else:
            ui(before.pid, 'see', 'New app version ' + FINAL)
            ui(before.pid, 'click', 'Prepare app download')
            # Wait for native selection before sending the user's Download decision.
            def download():
                if server.downloading.is_set():
                    return True
                ui(before.pid, 'click', 'Download app update')
                return server.downloading.wait(2)
            wait('native Sparkle enclosure download', download)
            intent = wait('armed install checkpoint', lambda: next(checkpoints.glob('*/native-start-intent.json'), None))
            if case == 'cancel-download':
                dirty = dirty_layer(create=True)
                ui(before.pid, 'click', 'Cancel download')
                server.download_gate.set()
                ui(before.pid, 'see', 'Open last-working app with separate settings')
                if not intent.exists():
                    raise ValueError('Cancelled native installation lost its recovery checkpoint.')
            else:
                ui(before.pid, 'see', 'The update is ready')
                if case == 'interrupted-before-quit':
                    before.kill()
                    before.wait(timeout=10)
                else:
                    if case == 'success':
                        guard = dirty_layer(create=True)
                        ui(before.pid, 'quit', '')
                        ui(before.pid, 'see', 'before restarting')
                        if before.poll() is not None or dirty_layer() != guard or digest(exe) != source_hash:
                            raise ValueError('Quit crossed the update boundary with unsaved changes.')
                        # Save As preserves the actual edit without overwriting the original PSD.
                        saved_edit = home / 'Saved work before update.psd'
                        rpc('document', **guard, action='save_report')
                        rpc('document', **guard, action='save', path=str(saved_edit), accept_editability_loss=True)
                        wait('private edited PSD save', lambda: saved_edit.is_file() and
                             any(not p['dirty'] and p['path'] == str(saved_edit) for p in rpc('projects', action='list')['projects']))
                        saved_edit_hash = digest(saved_edit)
                    # Stop the exact copied updater only if the sealed real Quit is observed.
                    # The application must handle uncertainty without synthesizing an install result.
                    ui(before.pid, 'quit', '')
                    if case == 'interrupted-install':
                        marker = intent.parent / 'sealed-parent-exit.json'
                        wait('sealed parent exit', marker.is_file)
                        owner = json.loads(intent.read_bytes())['owner_pid']
                        command = subprocess.check_output(['ps', '-p', str(owner), '-o', 'command='], text=True)
                        if not command.startswith(str(intent.parent / 'PeerBrush.app/Contents/MacOS/peerbrush') + ' '):
                            raise ValueError('Interrupted installer identity differs from the verified checkpoint.')
                        os.kill(owner, signal.SIGKILL)
                    if before.wait(timeout=90) != 0:
                        raise ValueError('User-reviewed Quit failed.')
                if case == 'success':
                    wait('real installed target and verified editor receipt', lambda:
                         digest(exe) == final_hash and any((data / 'Data/completed-updates').glob('*/installed-editor-verified.json')), 240)
                    new_pid = wait('relaunched default workspace', lambda:
                                   (pid if (pid := pid_at(connection)) != before.pid else None))
                    extra_editors.add((new_pid, exe))
                    # Reopen the test PSD through the newly launched editor's ordinary file activation.
                    subprocess.run([exe, '--open', artwork], env=env, cwd=home, check=True, timeout=20)
                    wait('target renders native source', pixels)
                    if rpc('tool_settings', action='get', tool='brush')['profile'] != saved:
                        raise ValueError('Installed target lost the brush settings.')
                    if digest(saved_edit) != saved_edit_hash:
                        raise ValueError('Install changed the saved project containing the live edit.')
                else:
                    prompt = launch()
                    ui(prompt.pid, 'click', 'Open recovery workspace')
                    state = config / 'update-recovery' / intent.parent.name
                    recovered_pid = wait('verified recovery workspace', lambda:
                                         pid_at(state / 'connection.json') if (state / 'connection.json').is_file() and pixels(state) else None)
                    extra_editors.add((recovered_pid, intent.parent / 'PeerBrush.app/Contents/MacOS/peerbrush'))
                    if rpc('tool_settings', state, action='get', tool='brush')['profile'] != saved:
                        raise ValueError('Recovery workspace did not inherit the saved settings.')
                    rpc('tool_settings', state, action='set', tool='brush', settings=dict(radius=7.25))
                    if (state / 'tool-settings.json').read_bytes() == saved_bytes or not intent.is_file():
                        raise ValueError('Recovery changes are not isolated from the held profile.')
        if (artwork.read_bytes() != original or live_settings.read_bytes() != saved_bytes
                or case not in ('success', 'interrupted-install') and digest(exe) != source_hash):
            raise ValueError('Installation, original settings or saved PSD unexpectedly changed.')
        if before.poll() is None and not pixels():
            raise ValueError('The editor lost the open project.')
        if dirty is not None and dirty_layer() != dirty:
            raise ValueError('Discovery or cancellation changed an unsaved edit.')
        print('Verified installed scenario: ' + case, flush=True)
    finally:
        server.feed_gate.set()
        server.download_gate.set()
        for process in processes:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=15)
        for pid, binary in extra_editors:
            command = subprocess.run(['ps', '-p', str(pid), '-o', 'command='], capture_output=True, text=True).stdout.strip()
            if command == str(binary) or command.startswith(str(binary) + ' '):
                os.kill(pid, signal.SIGTERM)


def main():
    if (sys.platform != 'darwin' or os.environ.get('RUNNER_ARCH') != 'ARM64'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted' or os.environ.get('GITHUB_JOB') != 'private-upgrade'):
        raise ValueError('Requires the disposable credential-free test Mac.')
    packages = unpack()
    starting, target = [inspect_pair(version, packages[version]) for version in (START, FINAL)]
    with http.server.ThreadingHTTPServer(('127.0.0.1', 38573), Handler) as server:
        server.feeds = {version: paths[2] for version, paths in packages.items()}
        server.image = packages[FINAL][1]
        server.feed_gate, server.download_gate = threading.Event(), threading.Event()
        server.requested, server.downloading = threading.Event(), threading.Event()
        server.current, server.fail = START, False
        server.feed_gate.set()
        server.download_gate.set()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        for case in CASES:
            run_case(case, server, starting, target)
    receipt = dict(schema=1, commit=os.environ['SELECTED_COMMIT'], run_id=os.environ['GITHUB_RUN_ID'],
                   version=FINAL, starting_version=START, cases=list(CASES),
                   target_zip_sha256=digest(packages[FINAL][0]), target_dmg_sha256=digest(packages[FINAL][1]),
                   starting_zip_sha256=digest(packages[START][0]), starting_dmg_sha256=digest(packages[START][1]))
    with (TEMP / 'installed-sparkle.json').open('x', encoding='utf-8') as file:
        json.dump(validate(receipt, receipt['commit'], FINAL, receipt['target_zip_sha256'],
                           receipt['target_dmg_sha256'], receipt['run_id']), file, indent=2)


if __name__ == '__main__':
    main()
