"""Exercise the exact staged app on a disposable GitHub-hosted Apple Silicon Mac."""
import json
import os
import signal
import struct
import subprocess
import sys
import time

from package import digest
from test_deb_version_upgrade import PROJECT_SIZE, png_samples


def exercise(app, work, executable_sha256):
    if (sys.platform != 'darwin' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'
            or os.environ.get('RUNNER_ARCH') != 'ARM64'):
        raise ValueError('The editor interruption exercise requires a disposable hosted Apple Silicon Mac.')
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
    print('Packaged macOS editor rendered twice. Native 16-bit PSD and brush settings survived abrupt process exit.', flush=True)
