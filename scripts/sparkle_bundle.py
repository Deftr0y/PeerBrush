"""Exact dormant Sparkle payload contract; no updater or publication operations.

Upstream bytes and native normalization policy are distinct authorities. A native
audit emits review candidates only; it never edits the committed policy.
"""
import hashlib
import base64
import json
import os
import pathlib
import plistlib
import re
import stat
import sys
import tempfile

PREFIX = 'PeerBrush.app/Contents/Frameworks/Sparkle.framework/'
PIN_NAME = 'packaging/sparkle/sparkle-2.10.0.json'
POLICY_NAME = 'packaging/sparkle/native-policy.json'
CHECK_CONFIGURATION = 'packaging/sparkle/check-configuration.json'
CHECK_MARKER = b'pub const CHECK_LAYOUT: u32 = 1;'
CHECK_SOURCES = {CHECK_CONFIGURATION, 'src/sparkle/check.rs', 'src/sparkle/macos/driver.rs',
                 'src/sparkle/macos/installed.rs'}
NOTICE = 'licenses/sparkle-2.10.0/LICENSE'
OBJECTS = (
    ('Versions/B/XPCServices/Installer.xpc', 'Versions/B/XPCServices/Installer.xpc/Contents/MacOS/Installer'),
    ('Versions/B/XPCServices/Downloader.xpc', 'Versions/B/XPCServices/Downloader.xpc/Contents/MacOS/Downloader'),
    ('Versions/B/Autoupdate', 'Versions/B/Autoupdate'),
    ('Versions/B/Updater.app', 'Versions/B/Updater.app/Contents/MacOS/Updater'),
    ('.', 'Versions/B/Sparkle'),
)
SEALS = frozenset((
    'Versions/B/XPCServices/Installer.xpc/Contents/_CodeSignature/CodeResources',
    'Versions/B/XPCServices/Downloader.xpc/Contents/_CodeSignature/CodeResources',
    'Versions/B/Updater.app/Contents/_CodeSignature/CodeResources',
    'Versions/B/_CodeSignature/CodeResources',
))
CODE = frozenset(code for _, code in OBJECTS)
DISABLED_INFO = dict(SUEnableAutomaticChecks=False, SUAutomaticallyUpdate=False,
    SUAllowsAutomaticUpdates=False, SUEnableSystemProfiling=False,
    SUVerifyUpdateBeforeExtraction=True, SURequireSignedFeed=True,
    SUSignedFeedFailureExpirationInterval=0)


def pin(sources=None):
    if sources is None:
        data = (pathlib.Path(__file__).resolve().parents[1] / PIN_NAME).read_bytes()
    else:
        data = sources[PIN_NAME]
    result = json.loads(data)
    if (result['version'] != '2.10.0' or len(result['framework_entries']) != 151
            or sum(item['kind'] == 'symlink' for item in result['framework_entries'].values()) != 9):
        raise ValueError('Unexpected Sparkle exact inventory revision')
    return result


def reviewed_policy(root=None, sources=None):
    root = root or pathlib.Path(__file__).resolve().parents[1]
    policy = json.loads(sources[POLICY_NAME] if sources is not None else (root / POLICY_NAME).read_bytes())
    if (not isinstance(policy, dict) or set(policy) != {'schema', 'archive_sha256', 'objects'} or policy['schema'] != 1
            or policy['archive_sha256'] != pin(sources)['archive_sha256'] or policy['objects'] is None):
        raise ValueError('Sparkle native signing/load policy awaits independent review; updater remains disabled')
    objects = policy['objects']
    if not isinstance(objects, dict) or set(objects) != {name for name, _ in OBJECTS}:
        raise ValueError('Sparkle native policy must name exactly five reviewed objects')
    for name, value in objects.items():
        if (not isinstance(value, dict) or set(value) != {'identifier', 'unsigned_sha256', 'entitlements'}
                or not isinstance(value['identifier'], str)
                or not re.fullmatch(r'[A-Za-z0-9.-]{1,160}', value['identifier'])
                or not isinstance(value['unsigned_sha256'], str)
                or not re.fullmatch('[0-9a-f]{64}', value['unsigned_sha256'])
                or not isinstance(value['entitlements'], dict)
                or value['entitlements'].get('com.apple.security.get-task-allow') is not None
                or (name != 'Versions/B/XPCServices/Downloader.xpc' and value['entitlements'] != {})):
            raise ValueError('Unreviewed Sparkle identifier, normalization or entitlement policy')
    return objects


def check_pins(sources, version):
    """Exact committed public check-only configuration; no service availability claim."""
    if CHECK_MARKER not in sources.get('src/sparkle.rs', b''):
        return None  # Historical dormant bundle sources retain their original plist.
    if not CHECK_SOURCES <= set(sources):
        raise ValueError('Manual Sparkle check source contract is incomplete')
    pins = json.loads(sources[CHECK_CONFIGURATION])
    if (not isinstance(pins, dict) or set(pins) != {'schema', 'feed_url', 'public_ed_key', 'publisher_team_id'}
            or type(pins['schema']) is not int or pins['schema'] != 1):
        raise ValueError('Invalid public Sparkle check configuration')
    fields = [pins[name] for name in ('feed_url', 'public_ed_key', 'publisher_team_id')]
    if fields == [None, None, None]:
        return None
    if (not all(isinstance(value, str) for value in fields)
            or pins['feed_url'] != 'https://peerbrush.com/updates/macos/stable/appcast.xml'
            or not re.fullmatch(r'[A-Z0-9]{10}', pins['publisher_team_id'])
            or not re.fullmatch(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', version)):
        raise ValueError('Manual Sparkle checks require complete reviewed public pins and a stable version')
    try:
        key = base64.b64decode(pins['public_ed_key'], validate=True)
    except ValueError as error:
        raise ValueError('Expected a canonical public Ed25519 key') from error
    if (len(key) != 32 or not any(key)
            or base64.b64encode(key).decode('ascii') != pins['public_ed_key']):
        raise ValueError('Expected a canonical public Ed25519 key')
    reviewed_policy(sources=sources)
    return pins


def check_publisher(root, commit, version, team):
    from package import source_files
    pins = check_pins({name: data for name, data, _ in source_files(root, commit)}, version)
    if pins is not None and pins['publisher_team_id'] != team:
        raise ValueError('Selected publisher differs from the compiled manual-check Team ID')


def links(sources=None):
    return {PREFIX + name: value['target'] for name, value in pin(sources)['framework_entries'].items()
            if value['kind'] == 'symlink'}


def link_target(name, sources=None):
    # Signing journals prepend exactly one selected release-directory component.
    if name.startswith('PeerBrush-') and '/' in name:
        name = name.split('/', 1)[1]
    return links(sources).get(name)


def validate_link(name, data, sources=None):
    target = link_target(name, sources)
    if target is None or data != target.encode('utf-8'):
        raise ValueError('Unexpected release symlink; only nine exact Sparkle links are permitted')
    return target


def entries(archive, sources):
    """Read only exact checksum-pinned upstream bytes after full inspection."""
    from prepare_sparkle import inspect
    import zipfile
    selected = pin(sources)
    inspect(archive, selected)
    values = []
    with zipfile.ZipFile(archive) as source:
        for name, item in selected['framework_entries'].items():
            if item['kind'] == 'directory':
                continue
            data = source.read(selected['framework_prefix'] + name)
            mode = 0o120777 if item['kind'] == 'symlink' else item['mode']
            values.append((PREFIX + name, data, mode))
    return values


def inspect_members(archive, prefix, sources, signed):
    """Portable structure/hash checks do not qualify native transformed code."""
    selected = pin(sources)
    found = set()
    for name, item in selected['framework_entries'].items():
        if item['kind'] == 'directory':
            continue
        relative = PREFIX + name
        entry = archive.getinfo(prefix + relative)
        data = archive.read(entry)
        mode = entry.external_attr >> 16
        if item['kind'] == 'symlink':
            if not stat.S_ISLNK(mode) or stat.S_IMODE(mode) != 0o777:
                raise ValueError('Sparkle link type or mode changed')
            validate_link(relative, data, sources)
        else:
            if not stat.S_ISREG(mode) or stat.S_IMODE(mode) != item['mode']:
                raise ValueError('Sparkle regular file type or mode changed')
            if not signed or name not in CODE | SEALS:
                if len(data) != item['bytes'] or hashlib.sha256(data).hexdigest() != item['sha256']:
                    raise ValueError('Sparkle unmodified code/resource differs from exact pin')
            elif not 0 < len(data) <= 16 * 1024 * 1024:
                raise ValueError('Sparkle transformed native object exceeds its budget')
        found.add(relative)
    return found


def disk_entries(root):
    """Metadata-only walk; retain only the nine bounded link targets in memory."""
    values = {}
    framework_seen = set()
    selected_entries = pin()['framework_entries']
    def visit(path):
        name = path.relative_to(root).as_posix()
        info = path.lstat()
        selected = None
        if name.endswith(PREFIX.rstrip('/')):
            relative = '.'
        elif PREFIX in name:
            relative = name.split(PREFIX, 1)[1]
        else:
            relative = None
        if relative is not None:
            selected = selected_entries.get(relative)
            if selected is None:
                raise ValueError('Unexpected Sparkle journal file/directory/link')
            framework_seen.add(relative)
        if getattr(info, 'st_file_attributes', 0) & 0x400:
            raise ValueError('Signing payload must not contain reparse points')
        if stat.S_ISLNK(info.st_mode):
            if info.st_nlink != 1 or stat.S_IMODE(info.st_mode) != 0o777:
                raise ValueError('Sparkle links must retain exact modes and a single inode link')
            target = validate_link(name, os.readlink(path).encode('utf-8'))
            if not path.resolve(strict=True).is_relative_to(root.resolve(strict=True)):
                raise ValueError('Sparkle link escapes payload')
            values[name] = (path, 0o120777, target.encode('utf-8'))
        elif stat.S_ISDIR(info.st_mode):
            if selected is not None:
                if selected['kind'] != 'directory' or stat.S_IMODE(info.st_mode) != selected['mode']:
                    raise ValueError('Sparkle directory mode differs from exact pin')
            for child in sorted(path.iterdir()):
                visit(child)
        elif stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
            if selected is not None:
                if selected['kind'] != 'file' or stat.S_IMODE(info.st_mode) != selected['mode']:
                    raise ValueError('Sparkle journal file inventory or mode differs from the exact pin')
            values[name] = (path, stat.S_IMODE(info.st_mode), None)
        else:
            raise ValueError('Signing payload has a hard link or nonregular entry')
    for child in sorted(root.iterdir()):
        visit(child)
    if framework_seen and framework_seen != set(selected_entries):
        raise ValueError('Incomplete Sparkle journal file/directory/link inventory')
    return values


def normalized_hash(path, run, directory):
    """Only owned copies are changed. Exact equality is mandatory, not assumed."""
    candidate = directory / 'code'
    with candidate.open('xb') as output:
        output.write(path.read_bytes())
    candidate.chmod(0o700)
    run(['/usr/bin/codesign', '--remove-signature', str(candidate)])
    return hashlib.sha256(candidate.read_bytes()).hexdigest()


def native_facts(path, run):
    detail = run(['/usr/bin/codesign', '--display', '--verbose=4', str(path)])
    text = detail.stdout + '\n' + detail.stderr
    identifiers = re.findall(r'^Identifier=([^\r\n]+)$', text, re.M)
    if len(identifiers) != 1 or not re.fullmatch(r'[A-Za-z0-9.-]{1,160}', identifiers[0]):
        raise ValueError('Sparkle native identifier is missing or ambiguous')
    value = run(['/usr/bin/codesign', '--display', '--entitlements', '-', '--xml', str(path)]).stdout.strip()
    entitlements = plistlib.loads(value.encode()) if value else {}
    if not isinstance(entitlements, dict):
        raise ValueError('Sparkle native entitlements must be an exact dictionary')
    return identifiers[0], entitlements, text


def verify_native(app, team_id, run):
    framework = app / 'Contents/Frameworks/Sparkle.framework'
    if not framework.exists():
        return
    policy = reviewed_policy()
    requirement = ('=anchor apple generic and certificate leaf[subject.OU] = '
                   f'"{team_id}" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists')
    for name, code in OBJECTS:
        object_path = framework / name
        run(['/usr/bin/codesign', '--verify', '--strict', '--test-requirement', requirement, str(object_path)])
        identifier, entitlements, text = native_facts(object_path, run)
        expected = policy[name]
        if (identifier != expected['identifier'] or entitlements != expected['entitlements']
                or f'TeamIdentifier={team_id}' not in text.splitlines()
                or not re.search(r'^Authority=Developer ID Application: .+ \(' + re.escape(team_id) + r'\)$', text, re.M)
                or not re.search(r'^CodeDirectory .*flags=0x[0-9a-fA-F]+\([^\n)]*\bruntime\b[^\n)]*\)', text, re.M)
                or not re.search(r'^Timestamp=\S.+$', text, re.M)):
            raise ValueError('Sparkle native publisher/runtime/timestamp/identifier/entitlements differ from reviewed policy')
        # Mounted DMGs are read-only. Normalization always uses a separate owned
        # system-temporary directory, never a resource inside the inspected app.
        with tempfile.TemporaryDirectory(prefix='.sparkle-normalize-') as temporary:
            if normalized_hash(framework / code, run, pathlib.Path(temporary)) != expected['unsigned_sha256']:
                raise ValueError('Sparkle code differs after native signature removal; signing-only equivalence not established')


def sign_nested(app, identity, keychain, run):
    framework = app / 'Contents/Frameworks/Sparkle.framework'
    if not framework.exists():
        return
    policy = reviewed_policy()  # Before the first signing mutation/provider call.
    for name, _ in OBJECTS:
        command = ['/usr/bin/codesign', '--force', '--sign', identity, '--options', 'runtime',
                   '--timestamp', '--identifier', policy[name]['identifier']]
        if name == 'Versions/B/XPCServices/Downloader.xpc':
            command += ['--preserve-metadata=entitlements']
        command += (['--keychain', str(keychain)] if keychain is not None else [])
        run(command + [str(framework / name)])


def audit_native(archive, work, run):
    """Credential-free facts for review, never accepted as signing authority."""
    from prepare_sparkle import inspect, load_pin, external_output, prepare, verify_stage, ROOT
    if sys.platform != 'darwin':
        raise ValueError('Sparkle native audit requires macOS 12+')
    import platform
    if int(platform.mac_ver()[0].split('.')[0] or 0) < 12:
        raise ValueError('Sparkle native audit requires macOS 12+')
    selected = load_pin()
    archive = inspect(archive, selected)
    work = external_output(work, ROOT)
    if work.exists() or work.is_symlink():
        raise ValueError('Native audit requires fresh private work')
    for parent in work.absolute().parents:
        if parent.is_symlink():
            raise ValueError('Native audit work must not use linked ancestors')
    work.mkdir(mode=0o700, parents=True)
    os.chmod(work, 0o700)
    # Native verification needs the original bundle/resource context, not a
    # renamed standalone Mach-O. Reuse the exact pinned framework stager.
    prepared = prepare(archive, work / 'upstream')
    framework = prepared / 'Sparkle.framework'
    facts = {}
    # Neither the staged bundle nor its separate normalization copies are loaded,
    # executed, signed or given trust. Signature removal touches owned copies only.
    for index, (name, code) in enumerate(OBJECTS):
        verify_stage(prepared, selected)
        original = framework / code
        data = original.read_bytes()
        if hashlib.sha256(data).hexdigest() != selected['framework_entries'][code]['sha256']:
            raise ValueError('Native audit original code differs from exact pin')
        directory = work / str(index)
        directory.mkdir(mode=0o700)
        operation = 'strict signature verification'
        try:
            run(['/usr/bin/codesign', '--verify', '--strict', str(framework / name)])
            operation = 'identifier/entitlement inspection'
            identifier, entitlements, _ = native_facts(framework / name, run)
            operation = 'owned-copy signature removal'
            facts[name] = dict(identifier=identifier, entitlements=entitlements,
                               unsigned_sha256=normalized_hash(original, run, directory))
        except ValueError as error:
            # Fixed public labels only; no native stderr, private paths or accounts.
            raise ValueError(f'Sparkle native audit {operation} failed for pinned object {name}') from error
        if original.read_bytes() != data:
            raise ValueError('Native audit modified its original snapshot')
    verify_stage(prepared, selected)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != selected['archive_sha256']:
        raise ValueError('Native audit upstream archive changed')
    result = dict(schema=1, format='PeerBrushSparkleNativePolicyCandidateV1',
        archive_sha256=selected['archive_sha256'], objects=facts,
        authority='review-candidate-only', updater_started=False,
        signing_performed=False, production_ready=False)
    if len(json.dumps(result).encode()) > 16 * 1024:
        raise ValueError('Native audit public facts exceed the bounded contract')
    return result
