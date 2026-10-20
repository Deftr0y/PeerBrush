"""Real 0.26 portable to current DEB compatibility, only in a disposable Ubuntu container.

This is opt-in settings migration, not a historical DEB or automatic updater test.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import tempfile
import time
import zlib
import zipfile

from package import digest, git
from package_macos_dmg import bounded_file, inspection_path
from prepare_site_release import API, DOWNLOAD, public_get
from production_readiness import unique_fields
from test_deb_installation import (DOCUMENTATION_OPTIONS, installed_identity, require_disposable,
                                   receipt_path, run, selected_package, validate_expected, write_receipt)
from verify_release import inspect_archive

OLD_VERSION = '0.26.0'
OLD_COMMIT = 'a00cf47accde87400fad3f0fcfc5fe96fc4f5be1'
OLD_TAG = 'v0.26.0'
OLD_NAME = 'PeerBrush-0.26.0-Linux-x64.zip'
OLD_SOURCE = 'PeerBrush-0.26.0-source.zip'
OLD_BYTES = 31088822
OLD_SHA256 = '03292c5552b5a66bcb066423379dddd2661852ef9b5471350f35d278feb3efe2'
PROJECT_SIZE = (32, 24)
DIAGNOSTIC_SCRIPTS = frozenset({
    'test_deb_version_upgrade.py', 'test_deb_installation.py', 'package.py',
    'package_deb.py', 'package_macos_dmg.py', 'prepare_site_release.py',
    'production_readiness.py', 'verify_release.py',
})


def failure_diagnostic(error):
    """Fixed categories and reviewed source locations only, never exception text."""
    kinds = {ValueError: 'ValueError', KeyError: 'KeyError', TypeError: 'TypeError',
        FileNotFoundError: 'FileNotFoundError', FileExistsError: 'FileExistsError',
        PermissionError: 'PermissionError', OSError: 'OSError', RuntimeError: 'RuntimeError',
        subprocess.TimeoutExpired: 'TimeoutExpired', subprocess.CalledProcessError: 'CalledProcessError',
        json.JSONDecodeError: 'JSONDecodeError', zipfile.BadZipFile: 'BadZipFile',
        UnicodeDecodeError: 'UnicodeDecodeError'}
    scripts = Path(__file__).resolve().parent
    allowed = {os.path.normcase(str((scripts / name).resolve())): name for name in DIAGNOSTIC_SCRIPTS}
    records, seen = [], set()
    while error is not None and len(records) < 3 and id(error) not in seen:
        seen.add(id(error))
        record = kinds.get(type(error), 'OtherError')
        current, location = error.__traceback__, None
        for _ in range(64):
            if current is None:
                break
            try:
                name = allowed.get(os.path.normcase(str(Path(current.tb_frame.f_code.co_filename).resolve())))
            except (OSError, ValueError, RuntimeError):
                name = None
            if name is not None and type(current.tb_lineno) is int and 1 <= current.tb_lineno <= 1000000:
                location = name + ':' + str(current.tb_lineno)
            current = current.tb_next
        if location is not None:
            record += ' at ' + location
        if type(error) is subprocess.CalledProcessError and type(error.returncode) is int and -32768 <= error.returncode <= 32767:
            record += ' exit=' + str(error.returncode)
        records.append(record)
        cause = error.__cause__
        error = cause if cause is not None else None if error.__suppress_context__ else error.__context__
    return 'Compatibility diagnostic: ' + ' <- '.join(records)


def phase(name):
    print('Real-version compatibility phase: ' + name, flush=True)


def historical_inputs(root, work, get=public_get):
    """Read back exact official assets and committed source before any old executable runs."""
    root = inspection_path(root)
    if git(root, 'rev-parse', 'HEAD').decode().strip() != OLD_COMMIT or git(root, 'status', '--porcelain', '--untracked-files=no').strip():
        raise ValueError('Historical acceptance requires the clean exact published 0.26 source checkout.')
    release = json.loads(get(API + 'releases/tags/' + OLD_TAG, 1024 * 1024, api=True), object_pairs_hook=unique_fields)
    resolved = json.loads(get(API + 'commits/' + OLD_TAG, 1024 * 1024, api=True), object_pairs_hook=unique_fields)
    if (not isinstance(release, dict) or not isinstance(resolved, dict)
            or release.get('tag_name') != OLD_TAG or release.get('draft') is not False
            or release.get('prerelease') is not False or resolved.get('sha') != OLD_COMMIT
            or release.get('html_url') != 'https://github.com/Deftr0y/PeerBrush/releases/tag/' + OLD_TAG
            or type(release.get('published_at')) is not str or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z', release['published_at'])):
        raise ValueError('Historical public release is unavailable or differs from its pinned stable source.')
    datetime.datetime.strptime(release['published_at'], '%Y-%m-%dT%H:%M:%SZ')
    assets = release.get('assets')
    if (not isinstance(assets, list) or not 1 <= len(assets) <= 100
            or any(not isinstance(row, dict) or type(row.get('name')) is not str for row in assets)
            or len({row['name'] for row in assets}) != len(assets)):
        raise ValueError('Historical public asset metadata is ambiguous.')
    selected = {}
    for name, maximum in [(OLD_NAME, OLD_BYTES), (OLD_SOURCE, 64 * 1024 * 1024), ('SHA256SUMS', 64 * 1024)]:
        rows = [row for row in assets if row['name'] == name]
        if len(rows) != 1:
            raise ValueError('A required historical public asset is unavailable.')
        row = rows[0]
        url = DOWNLOAD + OLD_TAG + '/' + name
        if (row.get('state') != 'uploaded' or type(row.get('size')) is not int or not 0 < row['size'] <= maximum
                or row.get('browser_download_url') != url):
            raise ValueError('Historical public asset metadata differs from the expected official origin.')
        target = work / name
        get(url, maximum, destination=target)
        checksum, size = bounded_file(target, maximum)
        if size != row['size'] or (row.get('digest') is not None and row['digest'] != 'sha256:' + checksum):
            raise ValueError('Historical downloaded bytes differ from their public asset metadata.')
        selected[name] = target
    archive = selected[OLD_NAME]
    if bounded_file(archive, OLD_BYTES) != (OLD_SHA256, OLD_BYTES):
        raise ValueError('Historical portable bytes differ from the independently pinned 0.26 artifact.')
    checked = inspect_archive(archive, OLD_COMMIT, OLD_VERSION, root)
    if (checked['manifest']['platform'] != 'Linux' or checked['manifest']['architecture'] != 'x64'
            or checked['packaging_correction'] or checked['record']['binary_source_commit'] != OLD_COMMIT):
        raise ValueError('Historical portable is not the exact published Linux source candidate.')
    source = selected[OLD_SOURCE].read_bytes()
    if source != checked['source']:
        raise ValueError('Historical separate corresponding source differs from the committed archive source.')
    sums = selected['SHA256SUMS'].read_text(encoding='ascii').replace('\r\n', '\n').splitlines()
    rows = [line.split('  ') for line in sums]
    if (not 1 <= len(rows) <= 64 or any(len(row) != 2 or not re.fullmatch('[0-9a-f]{64}', row[0])
            or not row[1] or '/' in row[1] or '\\' in row[1] for row in rows)
            or len({row[1] for row in rows}) != len(rows)):
        raise ValueError('Historical checksum filenames are invalid or ambiguous.')
    checksums = dict((name, checksum) for checksum, name in rows)
    for name in [OLD_NAME, OLD_SOURCE]:
        if checksums.get(name) != digest(selected[name]):
            raise ValueError('Historical public checksums do not select the exact portable and source bytes.')
    if (git(root, 'rev-parse', 'HEAD').decode().strip() != OLD_COMMIT
            or git(root, 'status', '--porcelain', '--untracked-files=no').strip()):
        raise ValueError('Historical source changed during input verification.')
    return archive, checked


def png_samples(path):
    """Strict bounded RGBA PNG8/16 decoder for this tiny generated fixture only."""
    checksum, size = bounded_file(path, 1024 * 1024)
    with path.open('rb') as stream:
        data = stream.read(1024 * 1024 + 1)
    if len(data) != size or hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError('Fixture PNG changed during its bounded read.')
    if data[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('Expected a bounded generated PNG fixture.')
    position, header, compressed, ended = 8, None, bytearray(), False
    while position < len(data):
        if position + 12 > len(data):
            raise ValueError('Truncated fixture PNG.')
        size, kind = struct.unpack('>I4s', data[position:position + 8])
        content = data[position + 8:position + 8 + size]
        checksum = data[position + 8 + size:position + 12 + size]
        if len(content) != size or len(checksum) != 4 or zlib.crc32(kind + content) != int.from_bytes(checksum, 'big'):
            raise ValueError('Fixture PNG chunk checksum differs.')
        position += size + 12
        if kind == b'IHDR':
            if header is not None or size != 13:
                raise ValueError('Unexpected fixture PNG header.')
            header = struct.unpack('>IIBBBBB', content)
        elif kind == b'IDAT':
            if header is None:
                raise ValueError('Fixture PNG raster precedes its header.')
            compressed.extend(content)
        elif kind == b'IEND':
            ended = size == 0 and position == len(data)
            break
        elif not kind[0] & 32:
            raise ValueError('Unsupported critical fixture PNG chunk.')
    if (not ended or header is None or header[:2] != PROJECT_SIZE or header[2] not in [8, 16]
            or header[3:] != (6, 0, 0, 0)):
        raise ValueError('Expected noninterlaced native RGBA fixture PNG.')
    width, height, depth = header[:3]
    bpp, stride = 4 * (depth // 8), width * 4 * (depth // 8)
    inflater = zlib.decompressobj()
    raw = inflater.decompress(compressed, (stride + 1) * height + 1)
    if len(raw) != (stride + 1) * height or not inflater.eof or inflater.unused_data or inflater.unconsumed_tail:
        raise ValueError('Fixture PNG raster exceeds or differs from its bounded dimensions.')
    result, previous = bytearray(), bytearray(stride)
    for y in range(height):
        offset = y * (stride + 1)
        kind, row = raw[offset], bytearray(raw[offset + 1:offset + 1 + stride])
        if kind > 4:
            raise ValueError('Unknown fixture PNG filter.')
        for x in range(stride):
            left, above = (row[x - bpp] if x >= bpp else 0), previous[x]
            upper_left = previous[x - bpp] if x >= bpp else 0
            if kind == 1: predictor = left
            elif kind == 2: predictor = above
            elif kind == 3: predictor = (left + above) // 2
            elif kind == 4:
                p = left + above - upper_left
                differences = [abs(p - left), abs(p - above), abs(p - upper_left)]
                predictor = [left, above, upper_left][differences.index(min(differences))]
            else: predictor = 0
            row[x] = (row[x] + predictor) & 255
        result.extend(row)
        previous = row
    return width, height, depth, bytes(result)


def editable_snapshot(state):
    document, layers = state.get('document'), state.get('layers')
    if (not isinstance(document, dict) or document.get('read_only') is not False
            or (document.get('width'), document.get('height')) != PROJECT_SIZE
            or type(document.get('bit_depth')) is not int or document['bit_depth'] not in [8, 16]
            or not isinstance(layers, list) or not 1 <= len(layers) <= 8
            or any(not isinstance(layer, dict) or not all(key in layer for key in ['kind', 'bounds', 'source']) for layer in layers)
            or sum(isinstance(layer['source'], dict) and isinstance(layer['source'].get('content'), dict) and layer['source']['content'].get('kind') == 'shape' for layer in layers) != 1):
        raise ValueError('Fixture lost its supported native document or editable Shape definition.')
    return dict(document={name: document[name] for name in ['width', 'height', 'bit_depth']},
                layers=[{name: layer[name] for name in ['kind', 'bounds', 'source']} for layer in layers])


class Session:
    """Only synthetic container user state; never prints connection files or RPC contents."""
    def __init__(self, binary, environment, work, state):
        self.binary, self.environment, self.work, self.state = binary, environment, work, state
        self.process = None

    def command(self, *arguments):
        state = ['--state-dir', str(self.state)] if self.state is not None else []
        return ['runuser', '-u', 'peerbrush-upgrade-qa', '--', str(self.binary), *state, *arguments]

    def rpc(self, method, parameters):
        response = subprocess.run(self.command('cli', method, json.dumps(dict(actor='human', **parameters))),
            env=self.environment, stdin=subprocess.DEVNULL, capture_output=True, timeout=10)
        if response.returncode != 0:
            # Preserve only the numeric status as a diagnostic cause; do not
            # retain RPC arguments, output or connection details in this cause.
            raise ValueError('Fixture RPC failed in the disposable compatibility phase.') from subprocess.CalledProcessError(response.returncode, 'fixture-rpc')
        data = response.stdout
        if len(data) > 2 * 1024 * 1024:
            raise ValueError('Fixture RPC response exceeds its budget.')
        reply = json.loads(data, object_pairs_hook=unique_fields)
        if not isinstance(reply, dict) or reply.get('ok') is not True or not isinstance(reply.get('result'), dict):
            raise ValueError('Fixture RPC failed in the disposable compatibility phase.')
        return reply['result']

    def __enter__(self):
        self.output = (self.work / ('server-' + str(time.monotonic_ns()) + '.log')).open('xb')
        try:
            self.process = subprocess.Popen(self.command('--headless'), env=self.environment,
                                            stdin=subprocess.DEVNULL, stdout=self.output, stderr=self.output,
                                            start_new_session=True)
            deadline = time.monotonic() + 30
            last_error = None
            while time.monotonic() < deadline:
                status = self.process.poll()
                if status is not None:
                    raise ValueError('Historical/current headless fixture could not start.') from subprocess.CalledProcessError(status, 'fixture-headless')
                try:
                    self.rpc('tool_settings', dict(action='get', tool='brush'))
                    return self
                except (ValueError, OSError, subprocess.TimeoutExpired) as error:
                    last_error = error
                    time.sleep(0.1)
            raise ValueError('Headless fixture did not become ready.') from last_error
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, *unused):
        try:
            if self.process is not None:
                if self.process.poll() is None:
                    try:
                        os.killpg(self.process.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    try:
                        self.process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        try:
                            os.killpg(self.process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        self.process.wait(timeout=10)
                else:
                    self.process.wait(timeout=10)
        finally:
            if hasattr(self, 'output'):
                self.output.close()
        return False

    def guarded(self, action, **parameters):
        state = self.rpc('observe', dict(max_edge=32))
        return self.rpc('document', dict(action=action, document_id=state['document']['id'],
            expected_revision=state['document']['revision'], **parameters))


def runtime_dependencies(package):
    # Keep the application absent while the old portable creates its fixtures.
    # Use only the selected DEB's canonical dependency contract, without changing
    # APT sources, trust settings or the host.
    from package_deb import dependencies
    selected = dependencies(run(['dpkg-deb', '--field', str(package), 'Depends']), require_runtime=True)
    run(['apt-get', 'satisfy', '-y', selected], env=dict(os.environ, DEBIAN_FRONTEND='noninteractive'))


def check(old_source_root, package, expected, output):
    require_disposable()
    if os.environ.get('PEERBRUSH_DISPOSABLE_REAL_UPGRADE_TEST') != '1':
        raise ValueError('Real-version acceptance needs its additional explicit disposable-container opt-in.')
    phase('current-package')
    validate_expected(expected)
    output = receipt_path(output)
    original_package = inspection_path(package)
    with tempfile.TemporaryDirectory(prefix='peerbrush-version-upgrade-') as temporary:
        work = Path(temporary).resolve()
        package = work / original_package.name
        if bounded_file(original_package, 512 * 1024 * 1024, package)[0] != expected['package_sha256']:
            raise ValueError('Current DEB bytes differ from their independent selected hash.')
        selected_package(package, expected)
        from package_deb import deb_version
        run(['dpkg', '--compare-versions', deb_version(expected['version']), 'gt', deb_version(OLD_VERSION)])
        phase('historical-inputs')
        archive, old = historical_inputs(old_source_root, work)
        phase('dependencies')
        runtime_dependencies(package)
        phase('old-fixture')
        run(['useradd', '--create-home', '--shell', '/bin/sh', 'peerbrush-upgrade-qa'])
        home = Path('/home/peerbrush-upgrade-qa')
        folders = {name: home / name for name in ['state', 'config', 'data', 'cache', 'runtime', 'artwork', 'old']}
        for folder in folders.values():
            folder.mkdir(mode=0o700)
        # Extract only the already inspected executable; source/notices stay in
        # the verified archive. This private fixture is never redistributed.
        binary = folders['old'] / 'peerbrush'
        with zipfile.ZipFile(archive) as source:
            member = old['prefix'] + old['manifest']['executable']
            binary.write_bytes(source.read(member))
        binary.chmod(0o755)
        if digest(binary) != old['manifest']['executable_sha256']:
            raise ValueError('Historical executable differs from its verified source archive.')
        run(['chown', '-R', 'peerbrush-upgrade-qa:peerbrush-upgrade-qa', str(home)])
        environment = dict(PATH=os.defpath, HOME=str(home), XDG_CONFIG_HOME=str(folders['config']),
                           XDG_DATA_HOME=str(folders['data']), XDG_CACHE_HOME=str(folders['cache']),
                           TMPDIR=str(folders['runtime']), LANG='C.UTF-8')
        if run(['runuser', '-u', 'peerbrush-upgrade-qa', '--', str(binary), '--version'], env=environment) != 'PeerBrush ' + OLD_VERSION:
            raise ValueError('Historical executable version differs from the selected published artifact.')
        legacy_file = folders['state'] / 'tool-settings.json'
        projects, samples, definitions = [], {}, {}
        with Session(binary, environment, work, folders['state']) as old_session:
            old_session.rpc('tool_settings', dict(action='set', tool='brush', settings=dict(radius=31.5, opacity=0.375)))
            old_profile = old_session.rpc('tool_settings', dict(action='get', tool='brush'))['profile']
            for depth in [8, 16]:
                old_session.guarded('new', width=32, height=24, bit_depth=depth)
                source = dict(width=32, height=24, content=dict(kind='shape', shape='rectangle',
                    bounds=[2., 3., 22., 18.], points=[], closed=True,
                    fill=[10001, 20003, 30007, 65535], stroke=[0, 0, 0, 0], stroke_width=0.))
                old_session.rpc('edit', dict(commands=[dict(op='source.add', source=source)], feedback='request'))
                definition = editable_snapshot(old_session.rpc('observe', dict(max_edge=32)))
                if definition['document']['bit_depth'] != depth:
                    raise ValueError('Historical fixture did not create the selected native document depth.')
                for extension in ['psd', 'psb']:
                    project = folders['artwork'] / ('Legacy ' + str(depth) + '.' + extension)
                    old_session.guarded('save_report')
                    old_session.guarded('save', path=str(project), accept_editability_loss=True)
                    exported = folders['artwork'] / ('old-' + str(depth) + '-' + extension + '.png')
                    old_session.guarded('export', path=str(exported))
                    sample = png_samples(exported)
                    if sample[2] != depth or (depth == 16 and b'\x27\x11\x4e\x23\x75\x37\xff\xff' not in sample[3]):
                        raise ValueError('Historical fixture did not preserve its intended native channel values.')
                    projects.append(project)
                    samples[project.name] = sample
                    definitions[project.name] = definition
        original_settings = legacy_file.read_bytes()
        original_projects = {path: digest(path) for path in projects}
        apt_options = [argument for option in DOCUMENTATION_OPTIONS for argument in ['-o', 'Dpkg::Options::=' + option]]
        phase('install-current')
        run(['apt-get', 'install', '-y', *apt_options, str(package)],
            env=dict(os.environ, DEBIAN_FRONTEND='noninteractive'))
        def user(command):
            return run(['runuser', '-u', 'peerbrush-upgrade-qa', '--', *command], env=environment)
        installed_identity(expected, user, deb_version(expected['version']))
        installed = Path('/usr/bin/peerbrush')
        def readback(session, phase):
            profile = session.rpc('tool_settings', dict(action='get', tool='brush'))['profile']
            if profile != old_profile:
                raise ValueError('Current application changed historical tool preferences.')
            for project in projects:
                state = session.guarded('open', path=str(project))
                if state['document']['read_only']:
                    raise ValueError('Supported historical fixture unexpectedly became protected.')
                if editable_snapshot(session.rpc('observe', dict(max_edge=32))) != definitions[project.name]:
                    raise ValueError('Current readback changed native depth, bounds, layer kind or editable Shape source.')
                exported = folders['artwork'] / (phase + '-' + project.name + '.png')
                session.guarded('export', path=str(exported))
                if png_samples(exported) != samples[project.name]:
                    raise ValueError('Current native8/16 readback differs from the historical saved project.')
        phase('explicit-readback')
        with Session(installed, environment, work, folders['state']) as current:
            readback(current, 'explicit')
        profile_root = folders['config'] / 'peerbrush/settings-v1'
        if profile_root.exists() or legacy_file.read_bytes() != original_settings:
            raise ValueError('Explicit-state compatibility must stay local and preserve legacy preferences.')
        phase('migration')
        user([str(installed), 'settings', 'migrate', '--from', str(folders['state'])])
        if ((profile_root / 'tool-settings.json').read_bytes() != original_settings
                or (profile_root / 'migration-backup/tool-settings.json').read_bytes() != original_settings):
            raise ValueError('Opt-in migration did not preserve actual historical settings and backup bytes.')
        phase('default-restart')
        with Session(installed, environment, work, None) as current:
            readback(current, 'migrated')
        with Session(installed, environment, work, None) as current:
            readback(current, 'restart')
        phase('preserved-inputs')
        if (legacy_file.read_bytes() != original_settings
                or any(digest(path) != checksum for path, checksum in original_projects.items())
                or digest(binary) != old['manifest']['executable_sha256']
                or digest(package) != expected['package_sha256']
                or bounded_file(original_package, 512 * 1024 * 1024)[0] != expected['package_sha256']):
            raise ValueError('Real-version checks changed original settings, artwork or selected executable/package bytes.')
    receipt = dict(schema=1, format='PeerBrushPortableToDebCompatibilityV1',
        old_version=OLD_VERSION, old_commit=OLD_COMMIT, old_portable_sha256=OLD_SHA256,
        old_executable_sha256=old['manifest']['executable_sha256'], old_source_sha256=old['manifest']['source_sha256'],
        version=expected['version'], commit=expected['commit'], package_sha256=expected['package_sha256'],
        checks=['exact-public-historical-assets-and-corresponding-source',
                'explicit-instance-settings-continuity', 'opt-in-per-user-migration-and-default-restart',
                'native8-and16-PSD-and-PSB-rendered-and-editable-source-readback', 'original-settings-and-artwork-retained'],
        limits={'historical-deb-upgrade': 'not-available', 'automatic-default-discovery-migration': 'not-performed',
                'native-gui': 'not-performed', 'interrupted-update-and-unsaved-work': 'not-performed',
                'publisher-signature-and-repository': 'not-assessed', 'public-readiness': 'not-assessed'},
        candidate_verified=False, ready=False, production_ready=False)
    phase('receipt')
    write_receipt(output, receipt)
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--old-source-root', required=True, type=Path)
    parser.add_argument('--package', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    for name in ['package-sha256', 'commit', 'version', 'executable-sha256', 'source-sha256']:
        parser.add_argument('--' + name, required=True)
    arguments = parser.parse_args()
    expected = {name: getattr(arguments, name) for name in ['version', 'commit', 'package_sha256', 'executable_sha256', 'source_sha256']}
    try:
        check(arguments.old_source_root, arguments.package, expected, arguments.output)
    except Exception as error:
        parser.exit(1, 'Real-version disposable compatibility failed; see the last printed phase.\n'
                    + failure_diagnostic(error) + '\n')
    print('Actual historical-portable to DEB compatibility checked; installed GUI and release readiness remain unverified.')
