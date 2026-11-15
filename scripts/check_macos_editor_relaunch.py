"""Exercise the exact staged app on a disposable GitHub-hosted Apple Silicon Mac."""
import json
import os
import signal
import struct
import subprocess
import sys
import time

from package import digest
from package_macos_dmg import attached_image_device
from prepare_site_release import DOWNLOAD, public_get
from sign_macos import verify_native, verify_native_image
from test_deb_version_upgrade import PROJECT_SIZE, png_samples


BUSY = frozenset({
    'Source inventory is busy',
    'Source inventory validation is busy',
    'Project is busy; source capture deferred',
    'Workspace is busy; source capture deferred',
    'Native backing is busy; retry without changing the document',
    'Export workspace is busy',
})


def exercise(app, work, executable_sha256, manual_upgrade=False, signed_team=None):
    if (sys.platform != 'darwin' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'
            or os.environ.get('RUNNER_ARCH') != 'ARM64'):
        raise ValueError('The editor interruption exercise requires a disposable hosted Apple Silicon Mac.')
    if signed_team and not manual_upgrade:
        raise ValueError('Signed editor acceptance requires the installed upgrade path.')
    exe = app / 'Contents/MacOS/peerbrush'
    if digest(exe) != executable_sha256:
        raise ValueError('Staged editor differs from the verified archive executable.')
    work.mkdir(mode=0o700)
    home, temporary = work / 'home', work / 'tmp'
    for directory in (home, temporary):
        directory.mkdir(mode=0o700)
    environment = dict(HOME=str(home), TMPDIR=str(temporary), PATH='/usr/bin:/bin')
    options = dict(env=environment, cwd=work, stdin=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    connection = temporary / 'PeerBrush/connection.json'
    artwork = work / 'Relaunch 16 bit.psd'
    width, height = PROJECT_SIZE
    rgb = (0xc301, 0x3f29, 0x15a7)
    # Three uncompressed planar channels, deliberately including non-8-bit samples.
    source = (struct.pack('>4sH6xHIIHHIIIH', b'8BPS', 1, 3, height, width, 16, 3, 0, 0, 0, 0)
              + b''.join(struct.pack('>H', value) * (width * height) for value in rgb))
    artwork.write_bytes(source)
    pixels = struct.pack('>4H', *rgb, 65535) * (width * height)

    def rpc(method, **params):
        request = json.dumps(dict(actor='human', **params))
        readback = method == 'observe' or method == 'tool_settings' and params.get('action') == 'get'
        export = method == 'document' and params.get('action') == 'export'
        for attempt in range(5):
            result = subprocess.run([exe, 'cli', method, request], stdout=subprocess.PIPE,
                                    timeout=12, **options)
            if not result.stdout:
                result.check_returncode()
            reply = json.loads(result.stdout)
            if reply.get('ok') is True:
                result.check_returncode()
                return reply['result']
            busy = reply.get('error') in BUSY
            if attempt == 4 or not busy or not (readback or export and not os.path.lexists(params['path'])):
                raise ValueError('The real editor refused the fixture operation.')
            time.sleep(0.2)

    previous_profile = None
    if manual_upgrade:
        print('Prepare the published macOS 0.30.1 app for a manual upgrade', flush=True)
        old_name = 'PeerBrush-0.30.1-macOS-arm64-apple-signed-r1-dmg-signed-r1.dmg'
        image = work / old_name
        public_get(DOWNLOAD + 'v0.30.1/' + old_name, 66042942, destination=image)
        if digest(image) != '3eb1155c30b46ca5a64e4b2f40d58e73bf022167865a33911ff0e9322d05c24c':
            raise ValueError('Published Mac installer differs from the pinned signed 0.30.1 DMG.')
        verify_native_image(image, 'KU3Q7F9Q84', notarized=True)
        mount = work.resolve() / 'published'
        mount.mkdir()
        applications = home / 'Applications'
        applications.mkdir(mode=0o700)
        installed = applications / 'PeerBrush.app'
        if attached_image_device(image.resolve()) is not None:
            raise ValueError('Published DMG must be detached before the exercise.')
        try:
            subprocess.run(['/usr/bin/hdiutil', 'attach', image, '-mountpoint', mount,
                            '-readonly', '-nobrowse', '-noautoopen'], check=True, timeout=90, **options)
            if not mount.is_mount():
                raise ValueError('Published DMG did not mount in the disposable workspace.')
            subprocess.run(['/usr/bin/ditto', mount / 'PeerBrush.app', installed], check=True,
                           timeout=90, **options)
        finally:
            device = attached_image_device(image.resolve())
            if device is not None:
                subprocess.run(['/usr/bin/hdiutil', 'detach', device], check=True, timeout=90, **options)
            if mount.is_mount():
                raise ValueError('Published DMG remains mounted.')
        verify_native(installed, 'KU3Q7F9Q84', notarized=True)
        exe = installed / 'Contents/MacOS/peerbrush'
        if subprocess.check_output([exe, '--version'], timeout=12, **options).strip() != b'PeerBrush 0.30.1':
            raise ValueError('The published editor did not identify as 0.30.1.')
        previous = subprocess.Popen([exe, '--headless'], stdout=subprocess.DEVNULL, **options)
        try:
            deadline = time.monotonic() + 45
            while previous.poll() is None and time.monotonic() < deadline:
                if connection.is_file() and json.loads(connection.read_bytes()).get('pid') == previous.pid:
                    break
                time.sleep(0.2)
            else:
                raise ValueError('The historical editor did not open the disposable workspace.')
            previous_profile = rpc('tool_settings', action='set', tool='brush',
                                   settings=dict(radius=31.5, opacity=0.375))['profile']
            profile_file = home / 'Library/Application Support/PeerBrush/Config/settings-v1/tool-settings.json'
            previous_bytes = profile_file.read_bytes()
        finally:
            if previous.poll() is None:
                previous.terminate()
                try:
                    previous.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    previous.kill()
                    previous.wait(timeout=10)
        installed.rename(work / 'previous.app')
        subprocess.run(['/usr/bin/ditto', app, installed], check=True, timeout=90, **options)
        if digest(exe) != executable_sha256:
            raise ValueError('Replacement in the disposable Applications folder changed the editor.')
        if signed_team:
            verify_native(installed, signed_team, notarized=True)

    profile = None
    for launch in ('first', 'restart'):
        print(f'Packaged editor {launch}: open PSD', flush=True)
        proc = subprocess.Popen([exe, '--open', artwork], stdout=subprocess.DEVNULL, **options)
        try:
            deadline = time.monotonic() + 60
            while proc.poll() is None and time.monotonic() < deadline:
                if connection.is_file() and json.loads(connection.read_bytes()).get('pid') == proc.pid:
                    projects = rpc('projects', action='list')['projects']
                    matches = [p for p in projects if p.get('path') == str(artwork) and p.get('loading') is None]
                    if len(matches) == 1:
                        break
                time.sleep(0.2)
            else:
                raise ValueError('The packaged editor did not open the fixture in its own window.')
            project = matches[0]['project_id']
            print(f'Packaged editor {launch}: verify native samples and settings', flush=True)
            doc = rpc('observe', project_id=project, max_edge=32, image=False)['state']['document']
            output = work / (launch + '-pixels.png')
            rpc('document', action='export', project_id=project, document_id=doc['id'],
                expected_revision=doc['revision'], path=str(output))
            if png_samples(output) != (width, height, 16, pixels) or artwork.read_bytes() != source:
                raise ValueError('The packaged editor changed the original PSD or its native samples.')
            if launch == 'first':
                if previous_profile is not None:
                    if (rpc('tool_settings', action='get', tool='brush')['profile'] != previous_profile
                            or profile_file.read_bytes() != previous_bytes):
                        raise ValueError('The manual upgrade changed the saved 0.30.1 brush profile.')
                    print('The published 0.30.1 brush profile is unchanged after the update.', flush=True)
                profile = rpc('tool_settings', action='set', tool='brush',
                              settings=dict(radius=19.25, opacity=0.625))['profile']
            elif rpc('tool_settings', action='get', tool='brush')['profile'] != profile:
                raise ValueError('The relaunched editor lost its saved per-user settings.')
            print(f'Packaged editor {launch}: wait for rendered workspace', flush=True)
            capture = work / (launch + '-workspace.png')
            rpc('capture_ui', path=str(capture), panel='workspace')
            deadline = time.monotonic() + 40
            while proc.poll() is None and time.monotonic() < deadline:
                if capture.is_file():
                    scaled = work / (launch + '-thumbnail.png')
                    decoded = subprocess.run(['/usr/bin/sips', '-Z', '128', str(capture), '--out', str(scaled)],
                                             stdout=subprocess.DEVNULL, timeout=8, **options)
                    if decoded.returncode == 0 and scaled.is_file() and scaled.stat().st_size > 100:
                        break
                time.sleep(0.2)
            else:
                raise ValueError('No decodable workspace frame arrived from the packaged editor.')
            if launch == 'first':
                proc.kill()
                if proc.wait(timeout=10) != -signal.SIGKILL:
                    raise ValueError('The fixture did not interrupt its exact child.')
            else:
                print('Packaged editor restart: request native Quit', flush=True)
                script = ('ObjC.import("AppKit"); function run(argv) { '
                          'if (!$.NSRunningApplication.runningApplicationWithProcessIdentifier('
                          'Number(argv[0])).terminate) throw Error("Quit refused"); }')
                subprocess.run(['/usr/bin/osascript', '-l', 'JavaScript', '-e', script, str(proc.pid)],
                               stdout=subprocess.DEVNULL, check=True, timeout=12, **options)
                if proc.wait(timeout=20) != 0:
                    raise ValueError('The relaunched editor did not quit normally.')
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=10)
    if artwork.read_bytes() != source or digest(exe) != executable_sha256:
        raise ValueError('The packaged editor or the saved fixture changed during relaunch.')
    if signed_team:
        verify_native(installed, signed_team, notarized=True)
    print('Packaged macOS editor rendered twice. Native 16-bit PSD and brush settings survived abrupt process exit.', flush=True)
