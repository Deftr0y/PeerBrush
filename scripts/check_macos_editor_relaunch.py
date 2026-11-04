"""Exercise the exact staged app on a disposable GitHub-hosted Apple Silicon Mac."""
import json
import os
import signal
import struct
import subprocess
import sys
import time

from package import digest
from prepare_site_release import DOWNLOAD, public_get
from sign_macos import verify_native
from test_deb_version_upgrade import PROJECT_SIZE, png_samples


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
        result = subprocess.run([exe, 'cli', method, json.dumps(dict(actor='human', **params))],
                                stdout=subprocess.PIPE, timeout=12, check=True, **options)
        reply = json.loads(result.stdout)
        if reply.get('ok') is not True:
            raise ValueError('The real editor refused the fixture operation.')
        return reply['result']

    legacy = None
    if manual_upgrade:
        print('Prepare published macOS 0.26 for the first manual upgrade', flush=True)
        old_name = 'PeerBrush-0.26.0-macOS-arm64-apple-signed-r1.zip'
        archive = work / old_name
        public_get(DOWNLOAD + 'v0.26.0/' + old_name, 27402539, destination=archive)
        if digest(archive) != 'db59d281163e6c7223d924ee134bf149d3551d41f2fdcb2438d2f1332762f17c':
            raise ValueError('Published Mac app differs from the pinned signed 0.26 archive.')
        old = work / 'published'
        subprocess.run(['/usr/bin/ditto', '-x', '-k', archive, old], check=True, timeout=90, **options)
        applications = home / 'Applications'
        applications.mkdir(mode=0o700)
        installed = applications / 'PeerBrush.app'
        (old / 'PeerBrush-0.26.0-macOS-arm64/PeerBrush.app').rename(installed)
        verify_native(installed, 'KU3Q7F9Q84', notarized=True)
        exe = installed / 'Contents/MacOS/peerbrush'
        if subprocess.check_output([exe, '--version'], timeout=12, **options).strip() != b'PeerBrush 0.26.0':
            raise ValueError('The historical editor did not identify as 0.26.0.')
        previous = subprocess.Popen([exe, '--headless'], stdout=subprocess.DEVNULL, **options)
        try:
            deadline = time.monotonic() + 45
            while previous.poll() is None and time.monotonic() < deadline:
                if connection.is_file() and json.loads(connection.read_bytes()).get('pid') == previous.pid:
                    break
                time.sleep(0.2)
            else:
                raise ValueError('The historical editor did not open the disposable workspace.')
            legacy = rpc('tool_settings', action='set', tool='brush',
                         settings=dict(radius=31.5, opacity=0.375))['profile']
            legacy_file = connection.parent / 'tool-settings.json'
            legacy_bytes = legacy_file.read_bytes()
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
                if legacy is not None:
                    preferences = home / 'Library/Application Support/PeerBrush/Config/settings-v1'
                    if (rpc('tool_settings', action='get', tool='brush')['profile'] != legacy
                            or (preferences / 'migration-backup/tool-settings.json').read_bytes() != legacy_bytes
                            or legacy_file.read_bytes() != legacy_bytes):
                        raise ValueError('The manual upgrade lost the 0.26 preferences or their backup.')
                    print('0.26 brush settings migrated and the original and backup match byte-for-byte.', flush=True)
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
