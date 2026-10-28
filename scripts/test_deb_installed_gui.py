"""Actual installed-window acceptance after disposable portable-to-DEB migration.

Only synthetic container users/displays. No physical desktop consent, release,
signature, updater, installation-interruption or hardware acceptance is implied.
"""
import sys

if __name__ == '__main__' and '--inside-display' in sys.argv:
    # Import failures happen before the CLI handler below; never print arguments.
    print('PeerBrushInstalledGuiPhaseV1 python-entry', file=sys.stderr, flush=True)

import argparse
import ctypes
from itertools import product
import json
import math
import os
from pathlib import Path
import select
import stat
import subprocess
import tempfile
import time

from package import digest
from package_deb import deb_version
from package_macos_dmg import bounded_file
from production_readiness import unique_fields
from test_deb_installation import (APP_ID, MIME, installed_identity, receipt_path,
    require_disposable, run, selected_package, validate_expected, write_receipt)
from test_deb_version_upgrade import (GUI_PHASES, Session, editable_snapshot, failure_diagnostic,
    installed_runuser, png_samples)

USER = 'peerbrush-upgrade-qa'
QA_HOME = Path('/home') / USER
DESKTOP = Path('/usr/share/applications') / (APP_ID + '.desktop')
WINDOW_SIZE = (1360, 900)
SOURCE_SIZE = (32, 24)
CELLS = [(depth, extension) for depth in [8, 16] for extension in ['psd', 'psb']]


def gui_phase(name):
    require(name in GUI_PHASES, 'Invalid fixed GUI diagnostic phase.')
    print('PeerBrushInstalledGuiPhaseV1 ' + name, file=sys.stderr, flush=True)


def require(value, message):
    if not value:
        raise ValueError(message)


def palette(cell):
    return [[r - cell * 3, g + cell * 3, b + cell * 3, 255]
            for r, g, b in [(211, 17, 79), (31, 199, 101), (53, 71, 227), (239, 181, 23)]]


def canvas_quadrants(samples, colors):
    """Four distinct painted regions must appear in their actual canvas arrangement."""
    width, height, depth, pixels = samples
    require((width, height, depth) == (*WINDOW_SIZE, 8), 'Installed window capture has unexpected dimensions/depth.')
    bounds = [[width, height, -1, -1, 0] for _ in colors]
    lookup = {bytes((r + dr, g + dg, b + db, 255)): index
              for index, (r, g, b, _) in enumerate(colors)
              for dr, dg, db in product([-1, 0, 1], repeat=3)}
    for y in range(height):
        previous, start = None, 0
        for x in range(width + 1):
            position = (y * width + x) * 4
            index = lookup.get(pixels[position:position + 4]) if x < width else None
            if index != previous or x == width:
                # Layer thumbnails can contain the same colors; only canvas-size runs count.
                if previous is not None and x - start >= 80:
                    box = bounds[previous]
                    box[0], box[1] = min(box[0], start), min(box[1], y)
                    box[2], box[3], box[4] = max(box[2], x - 1), max(box[3], y), box[4] + x - start
                previous, start = index, x
    boxes = []
    for left, top, right, bottom, count in bounds:
        area = (right - left + 1) * (bottom - top + 1)
        require(right - left >= 80 and bottom - top >= 60 and count >= area * 0.98,
                'Rendered canvas region is missing, fragmented or obscured.')
        boxes.append((left, top, right, bottom))
    a, b, c, d = boxes
    near = lambda first, second: abs(first - second) <= 2
    span_x, span_y = b[2] - a[0] + 1, c[3] - a[1] + 1
    # The real 32x24 texture is LINEAR-magnified: one source texel blends at
    # each seam. Two pixels cover raster-boundary rounding, not arbitrary gaps.
    gap_x = math.ceil(span_x / SOURCE_SIZE[0]) + 2
    gap_y = math.ceil(span_y / SOURCE_SIZE[1]) + 2
    require(all(near(left, right) for left, right in [(a[0], c[0]), (a[2], c[2]),
        (b[0], d[0]), (b[2], d[2]), (a[1], b[1]), (a[3], b[3]),
        (c[1], d[1]), (c[3], d[3])])
        and 1 <= b[0] - a[2] <= gap_x and 1 <= c[1] - a[3] <= gap_y
        and near(a[2] - a[0], b[2] - b[0]) and near(a[3] - a[1], c[3] - c[1])
        and abs(span_x * SOURCE_SIZE[1] - span_y * SOURCE_SIZE[0]) <= sum(SOURCE_SIZE),
        'Actual window does not show the expected shared-engine canvas arrangement.')
    # A texel-wide transition may contain blended colors, never background holes.
    for first, second in [(0, 1), (2, 3), (0, 2), (1, 3)]:
        box, other = boxes[first], boxes[second]
        horizontal = second - first == 1
        points = (((x, (box[1] + box[3]) // 2) for x in range(box[2] + 1, other[0]))
                  if horizontal else
                  (((box[0] + box[2]) // 2, y) for y in range(box[3] + 1, other[1])))
        for x, y in points:
            rgba = pixels[(y * width + x) * 4:(y * width + x + 1) * 4]
            require(rgba[3] == 255 and all(min(colors[first][ch], colors[second][ch]) - 1 <= rgba[ch]
                    <= max(colors[first][ch], colors[second][ch]) + 1 for ch in range(3)),
                    'Rendered canvas transition is missing or obscured.')
    return boxes


def duplicate_ok(samples, *, required=True):
    """Locate one bounded primary button in the expected centered warning."""
    width, height, depth, pixels = samples
    require((width, height, depth) == (*WINDOW_SIZE, 8), 'Unexpected warning capture size/depth.')
    points = set()
    for y in range(height // 2 - 120, height // 2 + 160):
        for x in range(width // 2 - 250, width // 2 + 250):
            rgba = pixels[(y * width + x) * 4:(y * width + x + 1) * 4]
            if rgba[3] == 255 and all(abs(rgba[ch] - value) <= 1 for ch, value in enumerate([190, 70, 29])):
                points.add((x, y))
    candidates = []
    while points:
        pending = [points.pop()]
        component = []
        while pending:
            x, y = pending.pop()
            component.append((x, y))
            for point in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]:
                if point in points:
                    points.remove(point)
                    pending.append(point)
        left, right = min(x for x, _ in component), max(x for x, _ in component)
        top, bottom = min(y for _, y in component), max(y for _, y in component)
        w, h = right - left + 1, bottom - top + 1
        if 20 <= w <= 64 and 24 <= h <= 36 and len(component) >= w * h * 0.55:
            white = sum(all(pixels[(y * width + x) * 4 + ch] >= 220 for ch in range(3))
                for y in range(top + 4, bottom - 3) for x in range(left + 4, right - 3))
            if 8 <= white <= 200:
                candidates.append(((left + right) // 2, (top + bottom) // 2))
    if not candidates and not required:
        return None
    require(len(candidates) == 1, 'Visible duplicate warning OK button is missing or ambiguous.')
    return candidates[0]


def gui_environment():
    import pwd
    account = pwd.getpwnam(USER)
    require(os.geteuid() == account.pw_uid != 0 and Path(account.pw_dir) == QA_HOME
            and Path(os.environ.get('HOME', '')) == QA_HOME,
            'Installed GUI check requires its existing synthetic non-root account.')
    require(sys.platform == 'linux' and Path('/.dockerenv').is_file()
            and os.environ.get('PEERBRUSH_DISPOSABLE_PACKAGE_TEST') == '1'
            and os.environ.get('PEERBRUSH_DISPOSABLE_REAL_UPGRADE_TEST') == '1'
            and os.environ.get('DISPLAY', '').startswith(':')
            and os.environ.get('DBUS_SESSION_BUS_ADDRESS', '').startswith('unix:'),
            'Installed GUI check needs the explicitly isolated container display/session.')
    # Process-local ownership only: GIO's real Exec child is adopted after gio exits.
    library = ctypes.CDLL(None, use_errno=True)
    require(library.prctl(36, 1, 0, 0, 0) == 0, 'Could not establish owned desktop-child reaping.')


class Window:
    """The installed desktop entry launches the app; only its exact adopted PID is closed."""
    def __init__(self, facts, work):
        self.facts, self.work = facts, work
        self.pid = self.pidfd = self.identity = None
        self.connection = QA_HOME / 'runtime/PeerBrush/connection.json'

    def rpc(self, method, parameters):
        response = subprocess.run(['/usr/bin/peerbrush', 'cli', method,
            json.dumps(dict(actor='human', **parameters))], stdin=subprocess.DEVNULL,
            capture_output=True, timeout=10)
        require(response.returncode == 0 and len(response.stdout) <= 2 * 1024 * 1024,
                'Installed GUI RPC failed or exceeded its response bound.')
        reply = json.loads(response.stdout, object_pairs_hook=unique_fields)
        require(isinstance(reply, dict) and reply.get('ok') is True and isinstance(reply.get('result'), dict),
                'Installed GUI RPC did not return a successful object.')
        return reply['result']

    def process_identity(self, pid):
        process = Path('/proc') / str(pid)
        status = (process / 'status').read_text()
        uid = next(row.split()[1:] for row in status.splitlines() if row.startswith('Uid:'))
        parent = next(row.split()[1] for row in status.splitlines() if row.startswith('PPid:'))
        require(uid == [str(os.geteuid())] * 4 and (process / 'exe').resolve(strict=True) == Path('/usr/bin/peerbrush')
                and digest(process / 'exe') == self.facts['expected']['executable_sha256']
                and parent == str(os.getpid()), 'Desktop process is not the exact adopted installed application.')
        command = (process / 'cmdline').read_bytes().split(b'\0')
        require(command[0] == b'/usr/bin/peerbrush' and command[1] == b'--'
                and b'--headless' not in command, 'Desktop entry did not start the real installed GUI route.')
        # starttime prevents a later reused PID from becoming close authority.
        fields = (process / 'stat').read_text().rsplit(')', 1)[1].split()
        require(len(fields) >= 20 and fields[19].isdigit(), 'Installed process start identity is unavailable.')
        return fields[19]

    def authenticate(self):
        require(self.pid is not None and self.process_identity(self.pid) == self.identity
                and not select.select([self.pidfd], [], [], 0)[0], 'Owned installed window has exited or changed.')

    def launch(self, projects):
        require(DESKTOP.read_bytes() == self.facts['desktop'].encode(), 'Installed desktop entry changed.')
        # A launched app may retain GIO stdout/stderr; never wait for its pipe EOF.
        with (self.work / ('desktop-launch-' + str(time.monotonic_ns()) + '.log')).open('xb') as log:
            subprocess.run(['gio', 'launch', str(DESKTOP), *map(str, projects)],
                check=True, stdin=subprocess.DEVNULL, stdout=log, stderr=log, timeout=10)
        if self.pid is not None:
            self.authenticate()
            return
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                checksum, size = bounded_file(self.connection, 16 * 1024)
                value = json.loads(self.connection.read_bytes(), object_pairs_hook=unique_fields)
                require(digest(self.connection) == checksum and size > 0, 'Connection changed during inspection.')
                pid = value['pid']
                require(type(pid) is int and 1 < pid < 2 ** 31, 'Invalid installed process identity.')
                identity = self.process_identity(pid)
                descriptor = os.pidfd_open(pid)
                try:
                    require(self.process_identity(pid) == identity, 'Installed process changed during acquisition.')
                    self.pid, self.identity, self.pidfd = pid, identity, descriptor
                    self.rpc('projects', dict(action='list'))
                    return
                except BaseException:
                    os.close(descriptor)
                    self.pid = self.pidfd = self.identity = None
                    raise
            except (OSError, ValueError, KeyError, subprocess.SubprocessError):
                time.sleep(0.1)
        raise ValueError('The installed desktop entry did not establish an authenticated live GUI.')

    def projects(self, paths):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            self.authenticate()
            projects = self.rpc('projects', dict(action='list'))['projects']
            found = {Path(p['path']): p for p in projects if p.get('path') is not None}
            if (set(found) == set(paths) and sum(p.get('path') is not None for p in projects) == len(paths)
                    and all(p.get('loading') is None for p in projects)):
                return found
            time.sleep(0.1)
        raise ValueError('Installed desktop delivery did not open the four exact independent projects.')

    def state(self, project):
        return self.rpc('observe', dict(project_id=project, max_edge=32, image=False))['state']

    def guarded(self, method, project, **parameters):
        state = self.state(project)
        return self.rpc(method, dict(project_id=project, document_id=state['document']['id'],
            expected_revision=state['document']['revision'], **parameters))

    def export(self, project, path):
        self.guarded('document', project, action='export', path=str(path))
        return png_samples(path)

    def screenshot(self, path):
        # Hovered brush previews are real overlays, especially at a magnified
        # 32x24 canvas. Park in the header; never weaken the painted-region oracle.
        window = self.focused_window()
        run(['xdotool', 'mousemove', '--sync', '--window', window, '2', '2'])
        self.check_focus(window)
        require(not path.exists(), 'Preserve earlier capture evidence.')
        self.rpc('capture_ui', dict(path=str(path), panel='workspace'))
        deadline = time.monotonic() + 30
        while not path.is_file() and time.monotonic() < deadline:
            self.authenticate()
            time.sleep(0.1)
        return png_samples(path, expected_size=WINDOW_SIZE)

    def capture(self, project, path, colors, *, select_project=True):
        if select_project:
            self.rpc('projects', dict(action='select', project_id=project))
        deadline = time.monotonic() + 30
        while self.rpc('projects', dict(action='list'))['active_project_id'] != project:
            require(time.monotonic() < deadline, 'Desktop activation did not select its existing project.')
            self.authenticate()
            time.sleep(0.1)
        before = self.state(project)
        history = self.rpc('history', dict(action='list', project_id=project, feedback='request'))
        samples = self.screenshot(path)
        if duplicate_ok(samples, required=False) is not None:
            # Initial desktop delivery can also repeat an already-open file.
            # Keep the first real capture and qualify a fresh frame after OK.
            self.acknowledge_duplicate(path.stem, samples=samples)
            path = path.with_name(path.stem + '-after-duplicate.png')
            samples = self.screenshot(path)
        boxes = canvas_quadrants(samples, colors)
        require(self.state(project) == before
                and self.rpc('history', dict(action='list', project_id=project, feedback='request')) == history,
                'Window capture changed native project state, history or reservations.')
        return dict(file=path.name, sha256=digest(path), width=WINDOW_SIZE[0], height=WINDOW_SIZE[1],
                    canvas_regions=[list(box) for box in boxes])

    def focused_window(self):
        self.authenticate()
        windows = [row.split(None, 4)[0] for row in run(['wmctrl', '-lp']).splitlines()
            if len(row.split(None, 4)) == 5 and row.split(None, 4)[2] == str(self.pid)]
        require(len(windows) == 1, 'The owned installed window is missing or ambiguous.')
        window = windows[0]
        run(['wmctrl', '-ia', window])
        self.check_focus(window)
        return window

    def check_focus(self, window):
        self.authenticate()
        require(int(run(['xdotool', 'getactivewindow'])) == int(window, 16)
                and run(['xdotool', 'getwindowpid', window]) == str(self.pid),
                'Capture interaction is not in the focused owned installed window.')

    def acknowledge_duplicate(self, index, *, samples=None):
        window = self.focused_window()
        if samples is None:
            samples = self.screenshot(self.work / ('duplicate-warning-' + str(index) + '.png'))
        x, y = duplicate_ok(samples)
        self.check_focus(window)
        run(['xdotool', 'mousemove', '--sync', '--window', window, str(x), str(y)])
        self.check_focus(window)
        # XTEST follows the native input route; --window would use SendEvent,
        # which applications may ignore. Focus/owner are checked immediately above.
        run(['xdotool', 'click', '1'])

    def close(self):
        self.authenticate()
        windows = []
        for row in run(['wmctrl', '-lp']).splitlines():
            fields = row.split(None, 4)
            if len(fields) == 5 and fields[2] == str(self.pid):
                windows.append(fields[0])
        require(len(windows) == 1, 'The owned installed GUI window is missing or ambiguous.')
        self.authenticate()
        run(['wmctrl', '-ic', windows[0]])  # EWMH -> WM_DELETE_WINDOW -> real shared close/on_exit.
        require(select.select([self.pidfd], [], [], 30)[0], 'Installed GUI did not close gracefully; acceptance failed.')
        pid, status = os.waitpid(self.pid, 0)
        require(pid == self.pid and os.waitstatus_to_exitcode(status) == 0,
                'The exact adopted installed GUI did not exit successfully.')
        os.close(self.pidfd)
        self.pid = self.pidfd = self.identity = None


def display_check(fixture):
    gui_phase('environment')
    gui_environment()
    gui_phase('fixture-readback')
    require(fixture == QA_HOME / 'installed-gui-qa/fixture.json', 'Use the fixed private GUI fixture.')
    checksum, _ = bounded_file(fixture, 256 * 1024)
    facts = json.loads(fixture.read_bytes(), object_pairs_hook=unique_fields)
    require(digest(fixture) == checksum, 'GUI fixture changed during inspection.')
    validate_expected(facts['expected'])
    gui_phase('installed-identity')
    installed_identity(facts['expected'], run, deb_version(facts['expected']['version']))
    work = fixture.parent
    wm_log = (work / 'window-manager.log').open('xb')
    gui_phase('window-manager-start')
    manager = subprocess.Popen(['openbox', '--sm-disable'], stdin=subprocess.DEVNULL,
        stdout=wm_log, stderr=wm_log)
    captures, saved = [], {}
    try:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            require(manager.poll() is None, 'Isolated window manager exited before GUI acceptance.')
            if subprocess.run(['wmctrl', '-m'], stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                break
            time.sleep(0.1)
        else:
            raise ValueError('Isolated window manager did not become available.')
        gui_phase('native-artwork-copy')
        paths = []
        for index, (depth, extension) in enumerate(CELLS):
            original = QA_HOME / 'artwork' / ('Legacy ' + str(depth) + '.' + extension)
            project = work / ('Open With 作品 e\u0301 ' + str(depth) + '.' + extension)
            record = facts['projects'][original.name]
            require(bounded_file(original, 2 * 1024 * 1024, project)[0] == record['sha256'],
                    'Original native project differs from the independently captured fixture.')
            require(run(['xdg-mime', 'query', 'filetype', str(project)]) == MIME,
                    'Actual Unicode PSD/PSB fixture has no installed Photoshop MIME registration.')
            paths.append(project)
        window = Window(facts, work)
        gui_phase('desktop-launch')
        window.launch(paths)
        gui_phase('projects-and-native-oracles')
        opened = window.projects(paths)
        require(window.rpc('tool_settings', dict(action='get', tool='brush'))['profile'] == facts['profile'],
                'GUI default launch changed the migrated real tool preferences.')
        for index, ((depth, extension), path) in enumerate(zip(CELLS, paths)):
            project = opened[path]['project_id']
            original = facts['projects']['Legacy ' + str(depth) + '.' + extension]
            before = window.state(project)
            require(editable_snapshot(before) == original['definition'], 'Desktop opening changed editable native sources.')
            native = window.export(project, work / ('original-' + str(index) + '.png'))
            require(native[2] == depth and native[3].hex() == original['native_hex'], 'Installed GUI lost native original samples.')
            window.guarded('edit', project, commands=[dict(op='layer.add', name='Installed GUI oracle')], feedback='request')
            layer = window.state(project)['layers'][0]['id']
            colors = palette(index)
            rectangles = [[0, 0, 16, 12], [16, 0, 32, 12], [0, 12, 16, 24], [16, 12, 32, 24]]
            window.guarded('edit', project, commands=[dict(op='fill', layer=layer, rect=rect, color=color)
                for rect, color in zip(rectangles, colors)], feedback='request')
            for _ in range(2):
                window.guarded('history', project, action='undo', feedback='request')
            require(editable_snapshot(window.state(project)) == original['definition']
                    and window.export(project, work / ('undo-' + str(index) + '.png')) == native,
                    'Real GUI history did not restore original native sources and samples.')
            for _ in range(2):
                window.guarded('history', project, action='redo', feedback='request')
            window.guarded('edit', project, commands=[dict(op='fill', layer=layer, rect=[0, 0, 1, 1],
                color=[1, 3, 5, 255])], feedback='request')
            window.guarded('history', project, action='undo', feedback='request')
            histories = window.rpc('history', dict(action='list', project_id=project, feedback='request'))
            require(len(histories['undo']) == 2 and histories['redo_count'] == 1,
                    'Actual GUI fixture did not retain nonempty undo and redo histories.')
            captures.append(window.capture(project, work / ('canvas-' + str(index) + '.png'), colors))
            snapshot = window.state(project)
            other = opened[paths[(index + 1) % len(paths)]]['project_id']
            window.rpc('projects', dict(action='select', project_id=other))
            window.launch([path])
            window.acknowledge_duplicate(index)
            captures.append(window.capture(project, work / ('activation-' + str(index) + '.png'), colors, select_project=False))
            require(window.projects(paths)[path]['project_id'] == project and window.state(project) == snapshot
                    and window.rpc('history', dict(action='list', project_id=project, feedback='request')) == histories,
                    'Existing-instance desktop delivery replaced unsaved work or its history.')
            report = window.guarded('document', project, action='save_report')
            window.guarded('document', project, action='save', path=str(path),
                accept_editability_loss=report['has_editability_losses'])
            require(window.rpc('history', dict(action='list', project_id=project, feedback='request')) == histories,
                    'Actual installed Save changed undo or redo history.')
            result = window.export(project, work / ('saved-' + str(index) + '.png'))
            expected_pixels = b''.join(bytes(color) if depth == 8 else
                b''.join((value * 257).to_bytes(2, 'big') for value in color)
                for y in range(24) for x in range(32)
                for color in [colors[(y >= 12) * 2 + (x >= 16)]])
            require(result == (32, 24, depth, expected_pixels), 'Saved GUI raster is not the exact native quadrant result.')
            saved[path] = (editable_snapshot(window.state(project)), result)
        gui_phase('profile-and-close')
        profile = window.rpc('tool_settings', dict(action='set', tool='brush',
            settings=dict(radius=19.25, opacity=0.625)))['profile']
        captures.append(window.capture(opened[paths[-1]]['project_id'], work / 'settings-settled.png', palette(3)))
        window.close()
        stored = QA_HOME / 'config/peerbrush/settings-v1/tool-settings.json'
        bounded_file(stored, 128 * 1024)
        profile_bytes = stored.read_bytes()
        require(json.loads(profile_bytes)['tools']['brush'] == profile, 'Normal GUI on_exit did not preserve current preferences.')
        gui_phase('restart-and-native-oracles')
        window = Window(facts, work)
        window.launch(paths)
        restarted = window.projects(paths)
        require(window.rpc('tool_settings', dict(action='get', tool='brush'))['profile'] == profile,
                'Installed GUI restart did not reload current per-user preferences.')
        for index, path in enumerate(paths):
            project = restarted[path]['project_id']
            require(editable_snapshot(window.state(project)) == saved[path][0]
                    and window.export(project, work / ('restart-' + str(index) + '.png')) == saved[path][1],
                    'Installed GUI restart changed saved native/source content.')
            captures.append(window.capture(project, work / ('restart-canvas-' + str(index) + '.png'), palette(index)))
        gui_phase('restart-close')
        window.close()
        require(stored.read_bytes() == profile_bytes, 'Unchanged GUI restart rewrote persisted preferences.')
        require(digest(DESKTOP) == facts['desktop_sha256'], 'GUI check changed the installed desktop entry.')
        gui_phase('result-publication')
        with (work / 'result.json').open('x', encoding='utf-8') as stream:
            json.dump(dict(captures=captures, native_cells=4, graceful_restarts=1,
                current_profile_sha256=digest(stored), native_gui='isolated-Xvfb-only', ready=False), stream)
    finally:
        if manager.poll() is None:
            manager.terminate()
        manager.wait(timeout=10)
        wm_log.close()
    # Any incomplete app lifetime above fails; no forced app kill or acceptance fallback.


def check(package, expected, output):
    require_disposable()
    require(os.environ.get('PEERBRUSH_DISPOSABLE_REAL_UPGRADE_TEST') == '1',
            'Installed GUI acceptance must follow the actual disposable portable migration.')
    validate_expected(expected)
    output = receipt_path(output)
    selected_package(package, expected)
    runuser = installed_runuser()
    environment = dict(PATH=os.defpath, HOME=str(QA_HOME), XDG_CONFIG_HOME=str(QA_HOME / 'config'),
        XDG_DATA_HOME=str(QA_HOME / 'data'), XDG_CACHE_HOME=str(QA_HOME / 'cache'),
        XDG_RUNTIME_DIR=str(QA_HOME / 'runtime'), TMPDIR=str(QA_HOME / 'runtime'), LANG='C.UTF-8',
        PEERBRUSH_DISPOSABLE_PACKAGE_TEST='1', PEERBRUSH_DISPOSABLE_REAL_UPGRADE_TEST='1',
        WINIT_X11_SCALE_FACTOR='1', LIBGL_ALWAYS_SOFTWARE='1')
    def user(command):
        return run([str(runuser), '-u', USER, '--', *command], env=environment)
    print('Installed GUI acceptance: checking installed identity before launch.', flush=True)
    installed_identity(expected, user, deb_version(expected['version']))
    work = QA_HOME / 'installed-gui-qa'
    work.mkdir(mode=0o700)
    import pwd
    account = pwd.getpwnam(USER)
    os.chown(work, account.pw_uid, account.pw_gid)
    facts = dict(expected=expected, desktop=DESKTOP.read_text(), desktop_sha256=digest(DESKTOP), projects={})
    require('Exec=/usr/bin/peerbrush -- %F\n' in facts['desktop']
            and 'DBusActivatable=true' not in facts['desktop'], 'Installed desktop route differs from the guarded Exec contract.')
    migration_backup = QA_HOME / 'config/peerbrush/settings-v1/migration-backup/tool-settings.json'
    legacy = QA_HOME / 'state/tool-settings.json'
    originals = {path: digest(path) for path in [migration_backup, legacy]}
    defaults = [QA_HOME / 'config/mimeapps.list', QA_HOME / 'data/applications/mimeapps.list',
                QA_HOME / 'data/applications/defaults.list']
    require(all(not path.is_symlink() for path in defaults), 'Synthetic default-editor configuration is a link.')
    default_bytes = {path: bounded_file(path, 128 * 1024)[0] if path.exists() else None for path in defaults}
    print('Installed GUI acceptance: preparing native project fixtures.', flush=True)
    with tempfile.TemporaryDirectory(prefix='peerbrush-gui-observer-') as temporary:
        with Session(Path('/usr/bin/peerbrush'), environment, Path(temporary), None, runuser) as session:
            facts['profile'] = session.rpc('tool_settings', dict(action='get', tool='brush'))['profile']
            for depth, extension in CELLS:
                path = QA_HOME / 'artwork' / ('Legacy ' + str(depth) + '.' + extension)
                session.guarded('open', path=str(path))
                definition = editable_snapshot(session.rpc('observe', dict(max_edge=32, image=False))['state'])
                exported = work / ('baseline-' + str(depth) + '-' + extension + '.png')
                session.guarded('export', path=str(exported))
                samples = png_samples(exported)
                require(samples[2] == depth and (depth != 16 or b'\x27\x11\x4e\x23\x75\x37\xff\xff' in samples[3]),
                        'Installed GUI baseline lacks the intended original native low-bit samples.')
                facts['projects'][path.name] = dict(sha256=digest(path), definition=definition, native_hex=samples[3].hex())
                originals[path] = digest(path)
    fixture = work / 'fixture.json'
    fixture.write_text(json.dumps(facts), encoding='utf-8')
    fixture.chmod(0o600)
    os.chown(fixture, account.pw_uid, account.pw_gid)
    print('Installed GUI acceptance: launching the isolated desktop.', flush=True)
    user(['xvfb-run', '-a', '-s', '-screen 0 1600x1000x24 -nolisten tcp', 'dbus-run-session', '--',
        sys.executable, '-B', str(Path(__file__).resolve()), '--inside-display', str(fixture)])
    require(all(digest(path) == checksum for path, checksum in originals.items()),
            'Installed GUI acceptance changed original artwork, legacy preferences or migration backups.')
    require(all(not path.is_symlink() and
                (bounded_file(path, 128 * 1024)[0] if path.exists() else None) == checksum
                for path, checksum in default_bytes.items()),
            'Desktop launch changed the synthetic user default-editor preference or its absence.')
    print('Installed GUI acceptance: checking installed identity after desktop exit.', flush=True)
    selected_package(package, expected)
    installed_identity(expected, user, deb_version(expected['version']))
    result_hash, _ = bounded_file(work / 'result.json', 64 * 1024)
    result = json.loads((work / 'result.json').read_bytes(), object_pairs_hook=unique_fields)
    require(digest(work / 'result.json') == result_hash, 'Installed GUI result changed during inspection.')
    require(result['native_cells'] == 4 and result['graceful_restarts'] == 1 and result['ready'] is False
            and len(result['captures']) == 13, 'Installed GUI result is incomplete.')
    captures = output.parent / 'installed-gui-captures'
    # The existing private receipt parent contains these root-owned files; the
    # host owner must also be able to retain them after the container exits.
    captures.mkdir(mode=0o755)
    captures.chmod(0o755)
    require(stat.S_IMODE(captures.stat().st_mode) == 0o755, 'Private capture directory is not host-readable.')
    for item in result['captures']:
        require(item['file'] == Path(item['file']).name and item['file'].endswith('.png'), 'Invalid capture inventory.')
        require(bounded_file(work / item['file'], 8 * 1024 * 1024, captures / item['file'])[0] == item['sha256'],
                'Actual installed capture changed before private retention.')
        (captures / item['file']).chmod(0o644)
        require(stat.S_IMODE((captures / item['file']).stat().st_mode) == 0o644, 'Private capture copy is not host-readable.')
    receipt = dict(schema=1, format='PeerBrushInstalledGuiV1', commit=expected['commit'], version=expected['version'],
        package_sha256=expected['package_sha256'], executable_sha256=expected['executable_sha256'],
        source_sha256=expected['source_sha256'], target='Ubuntu 24.04 amd64', checks=result,
        limits=dict(display='private-Xvfb-software-rendering-requested', physical_desktop_consent='not-performed',
            gpu_hardware='not-qualified', default_app_choice='not-performed', unsaved_close_review='not-exercised',
            interruption_and_updates='not-performed',
            publisher_signature='not-verified', publication='not-assessed'),
        candidate_verified=False, ready=False, production_ready=False)
    write_receipt(output, receipt)
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inside-display', type=Path)
    parser.add_argument('--package', type=Path)
    parser.add_argument('--output', type=Path)
    for name in ['package-sha256', 'commit', 'version', 'executable-sha256', 'source-sha256']:
        parser.add_argument('--' + name)
    args = parser.parse_args()
    try:
        if args.inside_display is not None:
            gui_phase('imports-ready')
            display_check(args.inside_display)
        else:
            require(args.package is not None and args.output is not None, 'Select the exact installed GUI candidate and private output.')
            expected = {name: getattr(args, name) for name in ['version', 'commit', 'package_sha256', 'executable_sha256', 'source_sha256']}
            check(args.package, expected, args.output)
            print('Disposable installed GUI checks passed: four native cells, thirteen real captures and a graceful restart; release readiness is not established.')
    except Exception as error:
        parser.exit(1, 'Disposable installed GUI acceptance failed. ' + failure_diagnostic(error) + '\n')
