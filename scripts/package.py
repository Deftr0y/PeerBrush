"""Package a committed native build, matching GPL source and license notices."""
import argparse
import gzip
import hashlib
import io
import json
import os
import pathlib
import plistlib
import re
import stat
import shutil
import struct
import subprocess
import sys
import tempfile
import tarfile
import time
import tomllib
import zipfile
import zlib



MANIFEST = "scripts/package-files.json"
MACOS_RESOURCES = 'PeerBrush.app/Contents/Resources/'
MAX_CRATE_COMPRESSED_BYTES = 64 * 1024 * 1024
MAX_CRATE_DECODED_BYTES = 128 * 1024 * 1024
MAX_CRATE_MEMBERS = 10000
MAX_CRATE_NOTICE_BYTES = 4 * 1024 * 1024
MAX_CRATE_NOTICES_BYTES = 16 * 1024 * 1024


def macos_info(version):
    numeric = r'(?:0|[1-9][0-9]*)'
    parsed = re.fullmatch(r'(' + numeric + r'\.' + numeric + r'\.' + numeric + r')(-dev\.' + numeric + r')?', version)
    if not parsed:
        raise ValueError('macOS bundles require a stable A.B.C or canonical internal A.B.C-dev.N version')
    info = {'CFBundleIdentifier': 'com.peerbrush.PeerBrush', 'CFBundleName': 'PeerBrush',
            'CFBundleDisplayName': 'PeerBrush', 'CFBundleExecutable': 'peerbrush',
            'CFBundlePackageType': 'APPL', 'CFBundleShortVersionString': parsed[1],
            'CFBundleVersion': parsed[1], 'LSMinimumSystemVersion': '11.0',
            'NSHighResolutionCapable': True, 'CFBundleIconFile': 'PeerBrush.icns',
            'CFBundleDocumentTypes': [{'CFBundleTypeName': name, 'CFBundleTypeRole': 'Editor',
                                      'LSHandlerRank': 'Alternate', 'CFBundleTypeIconFile': 'PeerBrush.icns',
                                      'LSItemContentTypes': [uti]}
                                     for name, uti in [('Photoshop PSD image', 'com.adobe.photoshop-image'),
                                                       ('Photoshop PSB image', 'com.peerbrush.photoshop-large-document')]],
            'UTImportedTypeDeclarations': [{'UTTypeIdentifier': 'com.peerbrush.photoshop-large-document',
                                            'UTTypeConformsTo': ['public.image'],
                                            'UTTypeDescription': 'Photoshop Large Document',
                                            'UTTypeTagSpecification': {'public.filename-extension': ['psb'],
                                                                       'public.mime-type': 'image/vnd.adobe.photoshop'}}]}
    if parsed[2]:
        info['PeerBrushDevelopmentVersion'] = version
    return info
# Exact upstream notice names, not a LICENSE* glob that could pick up local files.
NOTICE_NAMES = {
    "copying", "copyright", "license", "licence", "notice", "notices", "unlicense",
    "license-0bsd", "license-apache", "license-apache-2.0", "license-bsd",
    "license-libm-mit", "license-mit", "license-isc", "license-unicode", "license-zlib",
    "license.apache", "license.mit",
}
EMBEDDED_FONT_NOTICES = {"epaint_default_fonts": {"fonts/Hack-Regular.txt", "fonts/OFL.txt", "fonts/UFL.txt", "fonts/emoji-icon-font-mit-license.txt"}}
EXTRA_CRATE_NOTICES = {"ring": {
    "LICENSE-BoringSSL", "LICENSE-other-bits", "src/polyfill/once_cell/LICENSE-APACHE",
    "src/polyfill/once_cell/LICENSE-MIT", "third_party/fiat/LICENSE",
}}
NOTICE_NAMES |= {name + suffix for name in tuple(NOTICE_NAMES) for suffix in (".txt", ".md", ".rst")}



def relative_path(value):
    """Require portable relative file names and reject traversal/Windows aliases."""
    if not isinstance(value, str) or not value or any(ord(char) < 32 or ord(char) == 127 or char in '\\:<>"|?*' for char in value):
        raise ValueError("Manifest paths must be portable relative file names")
    path = pathlib.PurePosixPath(value)
    reserved = {"con", "prn", "aux", "nul", *(f"com{i}" for i in range(1, 10)), *(f"lpt{i}" for i in range(1, 10))}
    if path.is_absolute() or str(path) != value or any(
        part in (".", "..") or part.rstrip(" .") != part or part.split(".")[0].casefold() in reserved for part in path.parts
    ):
        raise ValueError("Invalid manifest path")
    return path

def regular_file(base, relative):
    """Reject symlinks, junctions/reparse points, and paths outside the input root."""
    relative = relative_path(relative)
    base = base.resolve(strict=True)
    path = base
    for part in relative.parts:
        path = path / part
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise ValueError("Package inputs must not use links or reparse points")
    if not path.resolve(strict=True).is_relative_to(base) or not path.is_file():
        raise ValueError("Package input must be a regular file within its root")
    return path

def copy_file(base, relative, destination):
    source = regular_file(base, relative)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)

def active_rust_docs():
    """Use the same active compiler as the build, never a repository fallback."""
    sysroot = subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip()
    if not sysroot or not pathlib.Path(sysroot).is_absolute():
        raise ValueError('Active Rust compiler did not return an absolute sysroot')
    return pathlib.Path(sysroot) / 'share' / 'doc' / 'rust'

def cached_crate_notices(registry, crate, package, metadata):
    """Read notices from a lock-verified Cargo cache, without extracting it."""
    checksum = package.get('checksum')
    if type(checksum) is not str or not re.fullmatch('[0-9a-f]{64}', checksum):
        raise ValueError('Missing locked dependency archive checksum')
    key = f"{package['name']}-{package['version']}"
    relative_path(key)
    meta = tomllib.loads(metadata.decode('utf-8'))['package']
    if meta.get('name') != package['name'] or meta.get('version') != package['version']:
        raise ValueError('Dependency metadata identity differs from its locked package')
    required = (EXTRA_CRATE_NOTICES.get(package['name'], set())
                | EMBEDDED_FONT_NOTICES.get(package['name'], set()))
    if meta.get('license-file'):
        required = required | {meta['license-file']}
    for name in required:
        relative_path(name)
    try:
        cache = regular_file(registry.parent / 'cache', crate.parent.name + '/' + key + '.crate')
        with cache.open('rb') as stream:
            compressed = stream.read(MAX_CRATE_COMPRESSED_BYTES + 1)
        if len(compressed) > MAX_CRATE_COMPRESSED_BYTES:
            raise ValueError('Dependency notice archive exceeds the compressed budget')
        if hashlib.sha256(compressed).hexdigest() != checksum:
            raise ValueError('Dependency archive differs from Cargo.lock checksum')
        with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as stream:
            decoded = stream.read(MAX_CRATE_DECODED_BYTES + 1)
        if len(decoded) > MAX_CRATE_DECODED_BYTES:
            raise ValueError('Dependency notice archive exceeds the decoded budget')
        seen, notices, found_metadata, total = set(), {}, False, 0
        # Bound the decoded buffer before tar/PAX header parsing can read it.
        with tarfile.open(fileobj=io.BytesIO(decoded), mode='r:') as archive:
            for count, entry in enumerate(archive, 1):
                path = relative_path(entry.name.rstrip('/') if entry.isdir() else entry.name)
                if count > MAX_CRATE_MEMBERS or path.parts[0] != key or str(path).casefold() in seen:
                    raise ValueError('Invalid dependency archive paths, duplicates or member count')
                seen.add(str(path).casefold())
                if entry.isdir():
                    continue
                if not entry.isfile() or len(path.parts) < 2 or not 0 <= entry.size <= MAX_CRATE_DECODED_BYTES:
                    raise ValueError('Dependency archive must contain bounded regular files')
                name = '/'.join(path.parts[1:])
                selected = (name in required or (len(path.parts) == 2 and path.name.casefold() in NOTICE_NAMES)
                            or (path.parts[1] == 'fonts' and path.name.casefold() in NOTICE_NAMES))
                if name == 'Cargo.toml' or selected:
                    if not 0 < entry.size <= MAX_CRATE_NOTICE_BYTES:
                        raise ValueError('Dependency notice or metadata exceeds its file budget')
                    with archive.extractfile(entry) as stream:
                        data = stream.read(entry.size + 1)
                    if len(data) != entry.size:
                        raise ValueError('Truncated dependency notice archive')
                    if name == 'Cargo.toml':
                        if data != metadata:
                            raise ValueError('Dependency metadata differs from the verified archive')
                        found_metadata = True
                    else:
                        total += len(data)
                        if total > MAX_CRATE_NOTICES_BYTES:
                            raise ValueError('Dependency notices exceed the combined budget')
                        notices[name] = data
        if not found_metadata or not required <= set(notices):
            raise ValueError('Missing required dependency notice inventory in verified archive')
        for name, data in notices.items():
            if digest(regular_file(crate, name)) != hashlib.sha256(data).hexdigest():
                raise ValueError('Dependency notice checksum mismatch')
        return notices
    except (OSError, EOFError, tarfile.TarError, zlib.error):
        raise ValueError('Could not read the required lock-verified dependency notice archive') from None


def dependency_notices(root, bundle, registry, manifest, rust_docs=None):
    notices = bundle / "licenses"
    rows = ["# Third-party notices", "", "Dependencies retain their original licenses. Sources and checksums are pinned by Cargo.lock in the included project source archive. This list covers downloaded dependencies; not every crate is linked into this platform binary.", ""]
    packages = tomllib.loads(regular_file(root, "Cargo.lock").read_text(encoding="utf-8"))["package"]
    for package in packages:
        if "source" not in package:
            continue
        key = f"{package['name']}-{package['version']}"
        matches = sorted(registry.glob("*/" + key))
        if not matches:
            continue  # Cargo.lock also lists dependencies for other platforms.
        if len(matches) != 1:
            raise ValueError("Ambiguous registry source; supply --registry for the build's registry")
        crate = matches[0]
        regular_file(registry, (crate / "Cargo.toml").relative_to(registry).as_posix())
        metadata = regular_file(crate, "Cargo.toml").read_bytes()
        meta = tomllib.loads(metadata.decode('utf-8'))["package"]
        rows.append(f"- {key}: {meta.get('license', meta.get('license-file', 'See source license'))}")
        selected = {p.name for p in crate.iterdir() if p.name.casefold() in NOTICE_NAMES}
        if meta.get("license-file"):
            selected.add(meta["license-file"])
        # Embedded egui fonts have their own licenses; select checksum-listed notices.
        checksum_file = crate / ".cargo-checksum.json"
        extra_notices = EXTRA_CRATE_NOTICES.get(package["name"], set())
        try:
            checksum_file.lstat()
            checksum_present = True
        except FileNotFoundError:
            checksum_present = False
        if not checksum_present and (extra_notices or EMBEDDED_FONT_NOTICES.get(package['name'])):
            for name, data in cached_crate_notices(registry, crate, package, metadata).items():
                target = notices / key / relative_path(name)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            continue
        if checksum_present:
            checksums = json.loads(regular_file(crate, ".cargo-checksum.json").read_text(encoding="utf-8"))["files"]
            if not extra_notices <= set(checksums):
                raise ValueError("Missing required dependency notice inventory")
            for name in extra_notices:
                if digest(regular_file(crate, name)) != checksums[name]:
                    raise ValueError("Dependency notice checksum mismatch")
            selected.update(extra_notices)
            font_notices = EMBEDDED_FONT_NOTICES.get(package["name"], set())
            if not font_notices <= set(checksums):
                raise ValueError("Missing embedded font notice inventory")
            selected.update(font_notices)
            for name in checksums:
                path = relative_path(name)
                if path.parts[0] == "fonts" and path.name.casefold() in NOTICE_NAMES:
                    selected.add(name)
        for name in sorted(selected):
            copy_file(crate, name, notices / key / relative_path(name))
    for directory in manifest["vendored"]:
        meta = tomllib.loads(regular_file(root, directory + "/Cargo.toml").read_text(encoding="utf-8"))["package"]
        key = f"{meta['name']}-{meta['version']}"
        rows.append(f"- {key} (vendored input patch): {meta.get('license', 'See source license')}")
        for name in manifest["source"]:
            path = relative_path(name)
            if str(path.parent) == directory and path.name.casefold() in NOTICE_NAMES:
                copy_file(root, name, notices / key / path.name)
        if meta['name'] == 'jpegxl-src':
            # Exact compiled native components; all bytes must be committed source.
            native_notices = ['libjxl/LICENSE', 'libjxl/third_party/highway/LICENSE',
                              'libjxl/third_party/highway/LICENSE-BSD3',
                              'libjxl/third_party/brotli/LICENSE',
                              'libjxl/third_party/skcms/LICENSE']
            for native in native_notices:
                selected = directory + '/' + native
                if selected not in manifest['source']:
                    raise ValueError('Missing committed JPEG XL native notice')
                copy_file(root, selected, notices / key / native)
            copy_file(root, 'LICENSE', notices / key / 'GPL-3.0.txt')
    copy_file(root, "assets/fonts/LICENCE.txt", notices / "ubuntu-sans" / "LICENCE.txt")
    # Include named compiler notices only, never all local compiler documentation.
    rust_docs = rust_docs if rust_docs is not None else active_rust_docs()
    for name in manifest["rust_notices"]:
        try:
            source = regular_file(rust_docs, name)
        except FileNotFoundError:
            raise ValueError('Missing required Rust runtime notice: ' + name + '; install rust-docs for the active compiler') from None
        if source.stat().st_size == 0:
            raise ValueError('Required Rust runtime notice is empty: ' + name)
        copy_file(rust_docs, name, notices / "rust-runtime" / relative_path(name))
    (bundle / "THIRD_PARTY_NOTICES.md").write_text("\n".join(rows) + "\n", encoding="utf-8")


def git(root, *args, input=None):
    environment = {key: value for key, value in os.environ.items() if not key.upper().startswith('GIT_')}
    return subprocess.run(['git', '-c', 'core.fsmonitor=false', '-C', str(root), *args], env=environment, input=input, check=True, capture_output=True).stdout


def hidden_index_inputs(root):
    return any(row[:1].islower() or row[:1] == b'S' for row in git(root, 'ls-files', '-v', '-z').split(b'\0'))


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def binary_architecture(path, platform):
    with path.open('rb') as stream:
        return architecture_from_stream(stream, platform)


def architecture_from_stream(stream, platform):
    header = stream.read(64)
    if platform == 'Windows' and len(header) >= 64 and header[:2] == b'MZ':
        stream.seek(struct.unpack_from('<I', header, 60)[0])
        pe = stream.read(6)
        if pe[:4] == b'PE\0\0' and pe[4:] == b'\x64\x86':
            return 'x64'
    elif platform == 'Linux' and len(header) >= 20 and header[:6] == b'\x7fELF\x02\x01':
        if struct.unpack_from('<H', header, 18)[0] == 62:
            return 'x64'
    elif platform == 'macOS' and len(header) >= 8 and header[:4] == b'\xcf\xfa\xed\xfe':
        cpu = struct.unpack_from('<I', header, 4)[0]
        if cpu in [0x1000007, 0x100000c]:
            return 'x64' if cpu == 0x1000007 else 'arm64'
    raise ValueError('Unsupported or mismatched native executable architecture')


def write_zip(path, files, epoch, compression=zipfile.ZIP_DEFLATED):
    files = list(files)
    for name, _, _ in files:
        relative_path(name)
    if len({name.casefold() for name, _, _ in files}) != len(files):
        raise ValueError("Ambiguous duplicate archive paths")
    stamp = time.gmtime(max(epoch, 315532800))[:6]
    with zipfile.ZipFile(path, 'w', compression, compresslevel=9) as archive:
        for name, data, mode in sorted(files):
            info = zipfile.ZipInfo(name, stamp)
            info.create_system = 3
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = compression
            archive.writestr(info, data)


def source_files(root, commit=None):
    commit = commit or git(root, 'rev-parse', 'HEAD').decode().strip()
    tree = {}
    for row in git(root, 'ls-tree', '-rz', commit).split(b'\0'):
        if row:
            meta, name = row.split(b'\t', 1)
            mode, kind, object_id = meta.split()
            tree[name.decode('utf-8')] = (mode, kind, object_id)
    if MANIFEST not in tree:
        raise ValueError('Missing committed archive policy')
    policy = json.loads(git(root, 'cat-file', 'blob', tree[MANIFEST][2].decode()))
    validate_policy(policy, tree)
    entries = []
    for name in policy['source']:
        mode, kind, object_id = tree[name]
        if kind != b'blob' or mode not in [b'100644', b'100755']:
            raise ValueError('Package source must contain regular committed files')
        entries.append((name, object_id, int(mode, 8) & 0o777))
    output = git(root, 'cat-file', '--batch', input=b'\n'.join(obj for _, obj, _ in entries) + b'\n')
    stream = io.BytesIO(output)
    files = []
    for name, object_id, mode in entries:
        actual, kind, size = stream.readline().split()
        if actual != object_id or kind != b'blob':
            raise ValueError('Source object mismatch')
        data = stream.read(int(size))
        if len(data) != int(size) or stream.read(1) != b'\n':
            raise ValueError('Truncated source object')
        files.append((name, data, mode))
    values = {name: data for name, data, _ in files}
    version = tomllib.loads(values['Cargo.toml'].decode('utf-8'))['package']['version']
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?', version):
        raise ValueError('Unsupported release version')
    readme = values[policy['readme']].replace(b'{{SOURCE_ARCHIVE}}', f'PeerBrush-{version}-source.zip'.encode())
    return files + [('README.md', readme, 0o644)]


def validate_policy(policy, tree):
    if not isinstance(policy, dict) or set(policy) != {'version', 'source', 'portable', 'readme', 'vendored', 'rust_notices'} or type(policy['version']) is not int or policy['version'] not in (1, 2):
        raise ValueError('Unsupported package manifest')
    for group in ['source', 'portable', 'vendored', 'rust_notices']:
        values = policy[group]
        if not isinstance(values, list) or not all(isinstance(v, str) for v in values):
            raise ValueError('Manifest paths must be strings')
        if len({v.casefold() for v in values}) != len(values):
            raise ValueError('Manifest lists must not contain duplicate paths')
        for value in values:
            relative_path(value)
    relative_path(policy['readme'])
    selected = set(policy['source'])
    required = {MANIFEST, policy['readme'], 'Cargo.toml', 'Cargo.lock', 'LICENSE', 'assets/fonts/LICENCE.txt', 'scripts/start-windows.cmd'}
    provenance_inputs = {'build.rs', 'src/build_info.rs'}
    if provenance_inputs & set(tree) and policy['version'] != 2:
        raise ValueError('Build provenance source requires package policy version 2')
    if policy['version'] == 2:
        required |= provenance_inputs
    if not required <= selected or not set(policy['portable']) <= selected:
        raise ValueError('Required inputs and portable files must be in corresponding source')
    generated = {'readme.md', 'release.json', 'third_party_notices.md', 'peerbrush.exe', 'peerbrush', 'start peerbrush.cmd'}
    if any(relative_path(n).parts[0].casefold() in generated | {'licenses', 'peerbrush.app'} or n.casefold().endswith('.zip') for n in selected):
        raise ValueError('Manifest inputs collide with generated package files')
    if not selected <= set(tree):
        raise ValueError('Missing committed package input; review scripts/package-files.json')
    compiler_roots = {'src', 'tests', 'examples', 'integrations', 'vendor'}
    # All files under application/build roots need review, including non-Rust
    # includes, provider requirements and nested Cargo build configuration.
    actual = {n for n in tree if relative_path(n).parts[0] in compiler_roots}
    vendored = {str(relative_path(n).parent) for n in tree if n.startswith('vendor/') and relative_path(n).name == 'Cargo.toml'}
    if vendored != set(policy['vendored']):
        raise ValueError('Vendored notice inventory changed; review the package manifest')
    if not actual <= selected:
        missing = ', '.join(sorted(actual - selected))
        raise ValueError('Compiler input inventory changed; review scripts/package-files.json: ' + missing)
    if any(n in tree and n not in selected for n in ['build.rs', 'rust-toolchain.toml', '.cargo/config', '.cargo/config.toml']):
        raise ValueError('Unlisted build configuration; review the package manifest')


def policy_from_sources(sources):
    return json.loads(next(data for name, data, _ in sources if name == MANIFEST))


def portable_files(sources):
    policy = policy_from_sources(sources)
    return [(name, data, mode) for name, data, mode in sources if name in set(policy['portable']) | {'README.md'}]


def validate_binary(data):
    markers = (
        rb'-----BEGIN (?:RSA |EC |OPENSSH |ENCRYPTED )?PRIVATE KEY-----',
        rb'gh[pousr]_[A-Za-z0-9]{30,}', rb'github_pat_[A-Za-z0-9_]{40,}',
        rb'[A-Za-z]:[/\\]+Users[/\\]+[^/\\\x00\r\n]{1,100}[/\\]',
        rb'/(?:Users|home)/[^/\x00\r\n]{1,100}/',
    )
    # Null removal also recognizes ASCII tokens/paths embedded as UTF-16.
    if any(re.search(marker, payload) for payload in [data, data.replace(b'\0', b'')] for marker in markers):
        raise ValueError('Executable contains a credential marker or personal build path; rebuild with path remapping and review before packaging')


def binary_build_info(data, version, commit):
    """Check a public embedded label, not cryptographic proof of compilation."""
    prefix = b'{"format":"PeerBrushBuildInfoV1",'
    suffix = b'"end":"PeerBrushBuildInfoEnd"}'
    if data.count(prefix) != 1:
        raise ValueError('Missing or ambiguous embedded build provenance')
    start = data.index(prefix)
    record = data[start:start + 512]
    end = record.find(suffix)
    if end < 0:
        raise ValueError('Malformed or oversized embedded build provenance')
    record = record[:end + len(suffix)]
    try:
        info = json.loads(record)
    except (UnicodeDecodeError, ValueError):
        raise ValueError('Malformed embedded build provenance') from None
    expected = {'format': 'PeerBrushBuildInfoV1', 'schema': 1, 'product': 'PeerBrush',
                'version': version, 'commit': commit, 'tree': 'clean', 'end': 'PeerBrushBuildInfoEnd'}
    if info != expected or type(info.get('schema')) is not int or record != json.dumps(info, separators=(',', ':')).encode():
        raise ValueError('Executable build provenance differs from the clean expected version/commit')
    return info


def add_notices(source_root, bundle, registry, policy, rust_docs=None):
    dependency_notices(source_root, bundle, registry, policy, rust_docs)
    return {p.relative_to(bundle).as_posix(): digest(p) for p in sorted((bundle / 'licenses').rglob('*')) if p.is_file()}


def equivalent_build_inputs(root, sources, original_commit):
    if not re.fullmatch(r'[0-9a-f]{40}', original_commit):
        raise ValueError('Expected full executable source commit')
    compiler_roots = {'src', 'tests', 'examples', 'integrations', 'vendor'}
    selected = {name: data for name, data, _ in sources if relative_path(name).parts[0] in compiler_roots or name.startswith('assets/') or name in ['Cargo.toml', 'Cargo.lock', 'build.rs', 'rust-toolchain.toml', '.cargo/config', '.cargo/config.toml']}
    original_names = set(git(root, 'ls-tree', '-r', '--name-only', original_commit).decode().splitlines())
    original_compiler = {name for name in original_names if relative_path(name).parts[0] in compiler_roots or name in ['Cargo.toml', 'Cargo.lock', 'build.rs', 'rust-toolchain.toml', '.cargo/config', '.cargo/config.toml']}
    if original_compiler != {name for name in selected if not name.startswith('assets/')}:
        raise ValueError('Executable source compiler inventory differs from package source')
    for name, data in selected.items():
        if name not in original_names or git(root, 'show', original_commit + ':' + name) != data:
            raise ValueError('Executable source inputs differ from package source')


def package(root, binary, output, registry, platform, verify_binary=True, rust_docs=None, binary_source_commit=None):
    root = root.resolve(strict=True)
    if git(root, 'status', '--porcelain', '--untracked-files=no').strip():
        raise ValueError('Commit tracked changes before packaging a release')
    commit = git(root, 'rev-parse', 'HEAD').decode().strip()
    epoch = int(git(root, 'show', '-s', '--format=%ct', commit))
    sources = source_files(root, commit)
    policy = policy_from_sources(sources)
    if policy['version'] == 2 and git(root, 'status', '--porcelain', '--untracked-files=normal').strip():
        raise ValueError('Commit or remove untracked source inputs before release packaging')
    if policy['version'] == 2 and hidden_index_inputs(root):
        raise ValueError('Hidden index inputs cannot establish clean release provenance')
    binary_source_commit = binary_source_commit or commit
    if binary_source_commit != commit:
        equivalent_build_inputs(root, sources, binary_source_commit)
    version = tomllib.loads(next(data for name, data, _ in sources if name == 'Cargo.toml').decode('utf-8'))['package']['version']
    binary = regular_file(binary.absolute().parent, binary.name)
    executable = binary.read_bytes()
    validate_binary(executable)
    architecture = architecture_from_stream(io.BytesIO(executable), platform)
    build_info = binary_build_info(executable, version, binary_source_commit) if policy['version'] == 2 else None
    output.mkdir(parents=True, exist_ok=True)
    name = f'PeerBrush-{version}-{platform}-{architecture}'
    # Stage all outputs before replacing final files. Never reuse an old
    # extracted bundle or touch unrelated output contents.
    with tempfile.TemporaryDirectory(prefix='.peerbrush-package-', dir=output) as temporary:
        stage = pathlib.Path(temporary)
        source_root = stage / 'source'
        source_root.mkdir()
        for filename, data, _ in sources:
            destination = source_root / relative_path(filename)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
        source = stage / f'PeerBrush-{version}-source.zip'
        write_zip(source, sources, epoch, zipfile.ZIP_STORED)
        bundle = stage / name
        bundle.mkdir()
        relative = 'peerbrush.exe' if platform == 'Windows' else 'peerbrush'
        if platform == 'macOS':
            relative = 'PeerBrush.app/Contents/MacOS/peerbrush'
            contents = bundle / 'PeerBrush.app' / 'Contents'
            (contents / 'MacOS').mkdir(parents=True)
            with (contents / 'Info.plist').open('wb') as stream:
                plistlib.dump(macos_info(version), stream)
        staged_binary = bundle / relative
        staged_binary.write_bytes(executable)
        staged_binary.chmod(0o755)
        if verify_binary:
            actual = subprocess.check_output([str(staged_binary.resolve()), '--version'], text=True, timeout=30).strip()
            if actual != f'PeerBrush {version}':
                raise ValueError('Executable version does not match the committed source')
            if build_info is not None:
                actual = subprocess.check_output([str(staged_binary.resolve()), '--build-info'], text=True, timeout=30).strip()
                if actual != json.dumps(build_info, separators=(',', ':')):
                    raise ValueError('Executable --build-info differs from its embedded provenance')
        for filename, data, _ in portable_files(sources):
            destination = bundle / relative_path(filename)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
        if platform == 'Windows':
            launcher = next(data for filename, data, _ in sources if filename == 'scripts/start-windows.cmd')
            (bundle / 'Start PeerBrush.cmd').write_bytes(launcher)
        shutil.copy2(source, bundle / source.name)
        notices = add_notices(source_root, bundle, registry, policy, rust_docs)
        manifest = {'schema': 1, 'product': 'PeerBrush', 'version': version, 'commit': commit, 'binary_source_commit': binary_source_commit, 'platform': platform, 'architecture': architecture, 'executable': relative, 'executable_sha256': digest(staged_binary), 'source': source.name, 'source_sha256': digest(source), 'notice_files': notices, 'license': 'GPL-3.0-only', 'native_publisher_signing': 'not-configured', 'package_signature': 'See the matching external Sigstore bundle when supplied; this manifest alone is not a signature.'}
        if platform == 'macOS':
            resources = bundle / MACOS_RESOURCES
            resources.mkdir()
            icon = next(data for name, data, _ in sources if name == 'assets/peerbrush.icns')
            if icon[:4] != b'icns' or len(icon) < 16 or struct.unpack('>I', icon[4:8])[0] != len(icon):
                raise ValueError('A valid committed macOS ICNS resource is required')
            (resources / 'PeerBrush.icns').write_bytes(icon)
            for filename in ['LICENSE', 'THIRD_PARTY_NOTICES.md', *notices]:
                copy_file(bundle, filename, resources / filename)
            manifest['macos_bundle_resources'] = 1
        if build_info is not None:
            manifest['build_info'] = build_info
        (bundle / 'release.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
        files = [(f'{name}/{p.relative_to(bundle).as_posix()}', p.read_bytes(), 0o755 if p == staged_binary else 0o644) for p in bundle.rglob('*') if p.is_file()]
        archive = stage / (name + '.zip')
        write_zip(archive, files, epoch)
        sums = stage / f'{name}.sha256'
        sums.write_text(f'{digest(archive)}  {archive.name}\n{digest(source)}  {source.name}\n', encoding='utf-8')
        for path in [source, archive, sums]:
            target = output / path.name
            if target.is_symlink() or (target.exists() and getattr(target.lstat(), 'st_file_attributes', 0) & 0x400):
                raise ValueError('Output archives must not be links or reparse points')
        for path in [source, archive, sums]:
            os.replace(path, output / path.name)
    archive = output / (name + '.zip')
    print(json.dumps({'package': archive.name, 'version': version, 'commit': commit, 'architecture': architecture, 'sha256': digest(archive)}))
    return archive


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--check-source', action='store_true', help='Validate the committed source archive policy without building or packaging')
    parser.add_argument('--output', type=pathlib.Path)
    parser.add_argument('--registry', type=pathlib.Path)
    parser.add_argument('--binary', type=pathlib.Path)
    parser.add_argument('--binary-source-commit', help='Explicit packaging-only correction: require identical application/build inputs to this original binary commit')
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    platform = {'win32': 'Windows', 'darwin': 'macOS'}.get(sys.platform, 'Linux')
    exe = 'peerbrush.exe' if platform == 'Windows' else 'peerbrush'
    cargo_home = pathlib.Path(os.environ.get('CARGO_HOME', str(pathlib.Path.home() / '.cargo')))
    try:
        if args.check_source:
            sources = source_files(root)
            print(f'Package source policy valid: {len(sources)} files')
            return
        package(root, args.binary or root / 'target' / 'release' / exe, (args.output or root / 'dist').resolve(), args.registry or cargo_home / 'registry' / 'src', platform, binary_source_commit=args.binary_source_commit)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        message = str(error) if isinstance(error, ValueError) else 'Input/output validation failed; check required files and permissions'
        parser.exit(1, 'Packaging failed: ' + message + '\n')


if __name__ == '__main__':
    main()
