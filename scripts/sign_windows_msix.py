"""Explicit existing-certificate Windows EXE/MSIX signing; no install or publication.

Disabled unless the caller explicitly approves using the selected existing
CurrentUser/My certificate. No keys, enrollment, imports or trust changes.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys
from urllib.parse import urlsplit

import package_msix as msix
from package_macos_dmg import bounded_file, inspection_path, private_inspection_directory
from pe_signing import signing_only, unsigned
from production_readiness import unique_fields
import qualify_signed_msix as qualified

CERTIFICATE_SELECTION_SCRIPT = r'''
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$PSModuleAutoLoadingPreference = 'None'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
try {
    if ($PSVersionTable.PSVersion.ToString() -cne '7.6.5') { throw 'version' }
    Import-Module -Name ([IO.Path]::Combine($PSHOME, 'Modules', 'Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Utility.psd1')) -ErrorAction Stop
    $store = [Security.Cryptography.X509Certificates.X509Store]::new('My', 'CurrentUser')
    try {
        $store.Open([Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
        $found = $store.Certificates.Find([Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
            $env:PEERBRUSH_CERTIFICATE_THUMBPRINT, $false)
        if ($found.Count -ne 1 -or -not $found[0].HasPrivateKey -or
            $found[0].Thumbprint -cne $env:PEERBRUSH_CERTIFICATE_THUMBPRINT) { throw 'selection' }
        $hash = [Security.Cryptography.SHA256]::Create()
        try { $sha256 = [Convert]::ToHexString($hash.ComputeHash($found[0].RawData)).ToLowerInvariant() }
        finally { $hash.Dispose() }
        [Console]::Write((@{schema=1; powershell=$PSVersionTable.PSVersion.ToString();
            thumbprint=$found[0].Thumbprint; certificate_sha256=$sha256;
            subject=$found[0].Subject; private_key_available=$true} | ConvertTo-Json -Compress))
    } finally { $store.Close(); $store.Dispose() }
    exit 0
} catch { exit 1 }
'''


def signing_selection(thumbprint, certificate_sha256, timestamp_uri):
    if (type(thumbprint) is not str or not re.fullmatch('[0-9A-F]{40}', thumbprint)
            or not qualified._sha(certificate_sha256) or type(timestamp_uri) is not str
            or not 1 <= len(timestamp_uri) <= 512 or any(ord(c) < 33 or ord(c) > 126 for c in timestamp_uri)):
        raise ValueError('Explicit public certificate locator, SHA-256 and HTTPS timestamp selection required.')
    uri = urlsplit(timestamp_uri)
    if (uri.scheme != 'https' or not uri.hostname or uri.username is not None or uri.password is not None
            or uri.port is not None or uri.query or uri.fragment or '\\' in timestamp_uri
            or uri.netloc != uri.hostname):
        raise ValueError('Timestamp selection must be an approved canonical HTTPS service URL.')


def select_certificate(powershell, thumbprint, certificate_sha256, publisher, work):
    data = qualified._command(powershell, ['-NoLogo', '-NoProfile', '-NonInteractive',
        '-Command', CERTIFICATE_SELECTION_SCRIPT], work, 'certificate-selection', thumbprint=thumbprint)
    value = json.loads(data, object_pairs_hook=unique_fields)
    expected = dict(schema=1, powershell=qualified.POWERSHELL_VERSION, thumbprint=thumbprint,
        certificate_sha256=certificate_sha256, subject=publisher, private_key_available=True)
    if (type(value) is not dict or set(value) != set(expected)
            or any(type(value.get(key)) is not type(item) or value.get(key) != item for key, item in expected.items())):
        raise ValueError('Existing certificate does not match the approved public identity and leaf SHA-256.')


def sign_file(signtool, path, thumbprint, timestamp_uri, work, label):
    qualified._command(signtool, ['sign', '/s', 'My', '/sha1', thumbprint, '/fd', 'SHA256',
        '/tr', timestamp_uri, '/td', 'SHA256', '/q', str(path)], work, label)


def produce(root, portable, *, commit, version, identity, package_family, portable_sha256,
            certificate_sha1, certificate_sha256, timestamp_uri, signtool, signtool_sha256,
            powershell, powershell_sha256, makeappx, makeappx_sha256, revision, work, output,
            approve_existing_certificate_signing=False):
    if approve_existing_certificate_signing is not True:
        raise ValueError('Windows signing is disabled without explicit approval of the selected existing certificate.')
    if sys.platform != 'win32':
        raise ValueError('Windows signing requires an actual approved native Windows host.')
    signing_selection(certificate_sha1, certificate_sha256, timestamp_uri)
    if (type(commit) is not str or not re.fullmatch('[0-9a-f]{40}', commit)
            or type(version) is not str or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version)
            or type(revision) is not str or not re.fullmatch('[1-9][0-9]{0,5}', revision)
            or not qualified._sha(portable_sha256)):
        raise ValueError('Windows signing needs exact stable source, input hash and revision.')
    root, portable = inspection_path(root), inspection_path(portable)
    qualified._source(root, commit)
    msix.identity_values(identity)
    msix.validate_package_family(identity, package_family)
    if portable.name != f'PeerBrush-{version}-Windows-x64.zip':
        raise ValueError('Windows signing requires the selected exact-source portable archive.')
    tools = [qualified._selected_tool(path, checksum, name) for path, checksum, name in [
        (signtool, signtool_sha256, 'signtool.exe'), (powershell, powershell_sha256, 'pwsh.exe'),
        (makeappx, makeappx_sha256, 'makeappx.exe')]]
    work = private_inspection_directory(work, root, fresh=True)
    output = private_inspection_directory(output, root, fresh=True)
    if (work.is_relative_to(output) or output.is_relative_to(work)
            or any(area.is_relative_to(path.parent) for area in [work, output] for path in [portable, *tools])):
        raise ValueError('Signing work/output must be separate from each other, inputs and tools.')
    work.mkdir(mode=0o700)
    os.chmod(work, 0o700)
    inputs = work / 'inputs'
    inputs.mkdir(mode=0o700)
    original = inputs / portable.name
    if bounded_file(portable, msix.PACKAGE_LIMIT, original)[0] != portable_sha256:
        raise ValueError('Portable archive differs from the approved SHA-256.')
    original.chmod(0o400)
    expected, release = msix.expected_payload(root, original, identity, require_provenance=True)
    if release['commit'] != commit or release['version'] != version:
        raise ValueError('Portable source/build differs from selected stable release.')
    before = expected['peerbrush.exe']
    unsigned(before)
    select_certificate(tools[1], certificate_sha1, certificate_sha256, identity['publisher'], work)
    executable = work / 'peerbrush.exe'
    with executable.open('xb') as stream:
        stream.write(before)
    sign_file(tools[0], executable, certificate_sha1, timestamp_uri, work, 'sign-executable')
    bounded_file(executable, msix.PACKAGE_LIMIT, executable=True)
    after = executable.read_bytes()
    signing_only(before, after)
    qualified.verify_native_executable(after, identity['publisher'], certificate_sha256,
        tools[0], tools[1], work / 'executable-check')
    expected = msix.signed_executable_payload(expected, after)
    stage = work / 'payload'
    msix._write_payload(expected, stage)
    target_version = msix.package_version(version)
    packages = work / 'packages'
    packages.mkdir(mode=0o700)
    unsigned_package = packages / f'PeerBrush-{target_version}-Windows-x64.msix'
    qualified._selected_tool(tools[2], makeappx_sha256, 'makeappx.exe')
    qualified._command(tools[2], ['pack', '/d', str(stage), '/p', str(unsigned_package), '/o'], work, 'makeappx')
    msix.verify_payload(unsigned_package, expected)
    unsigned_sha256 = bounded_file(unsigned_package, msix.PACKAGE_LIMIT)[0]
    final = packages / (unsigned_package.stem + '-signed-r' + revision + '.msix')
    bounded_file(unsigned_package, msix.PACKAGE_LIMIT, final)
    qualified._selected_tool(tools[0], signtool_sha256, 'signtool.exe')
    sign_file(tools[0], final, certificate_sha1, timestamp_uri, work, 'sign-package')
    final_sha256 = bounded_file(final, msix.PACKAGE_LIMIT)[0]
    result = qualified.inspect_final(root, original, unsigned_package, final, commit=commit,
        version=version, identity=identity, package_family=package_family, portable_sha256=portable_sha256,
        unsigned_sha256=unsigned_sha256, package_sha256=final_sha256, certificate_sha256=certificate_sha256,
        signtool=tools[0], signtool_sha256=signtool_sha256, powershell=tools[1],
        powershell_sha256=powershell_sha256, work=work / 'final-check', require_executable_signature=True)

    def unchanged():
        qualified._source(root, commit)
        private_inspection_directory(work, root, fresh=False)
        if (bounded_file(portable, msix.PACKAGE_LIMIT)[0] != portable_sha256
                or bounded_file(original, msix.PACKAGE_LIMIT)[0] != portable_sha256
                or bounded_file(unsigned_package, msix.PACKAGE_LIMIT)[0] != unsigned_sha256
                or bounded_file(final, msix.PACKAGE_LIMIT)[0] != final_sha256
                or bounded_file(executable, msix.PACKAGE_LIMIT, executable=True)[0] != hashlib.sha256(after).hexdigest()):
            raise ValueError('Signing inputs or final bytes changed; preserve private work.')
        for tool, checksum in zip(tools, [signtool_sha256, powershell_sha256, makeappx_sha256]):
            qualified._selected_tool(tool, checksum, tool.name.casefold())

    unchanged()
    output.mkdir(mode=0o700)
    os.chmod(output, 0o700)
    private_inspection_directory(output, root, fresh=False)
    for source, checksum in [(unsigned_package, unsigned_sha256), (final, final_sha256)]:
        destination = output / source.name
        if bounded_file(source, msix.PACKAGE_LIMIT, destination)[0] != checksum or bounded_file(destination, msix.PACKAGE_LIMIT)[0] != checksum:
            raise ValueError('Copied Windows candidate differs from qualified bytes.')
    checksums = f'{unsigned_sha256}  {unsigned_package.name}\n{final_sha256}  {final.name}\n'.encode('utf-8')
    with (output / 'SHA256SUMS').open('xb') as stream:
        stream.write(checksums)
    report = (json.dumps(result, sort_keys=True, indent=2) + '\n').encode('utf-8')
    if len(report) > qualified.OUTPUT_LIMIT:
        raise ValueError('Windows candidate report exceeds its byte budget.')
    with (output / 'windows-signing-candidate.json').open('xb') as stream:
        stream.write(report)
    unchanged()
    for path, checksum in [(output / unsigned_package.name, unsigned_sha256), (output / final.name, final_sha256)]:
        if bounded_file(path, msix.PACKAGE_LIMIT)[0] != checksum:
            raise ValueError('Final output changed during candidate retention.')
    for path, data in [(output / 'SHA256SUMS', checksums), (output / 'windows-signing-candidate.json', report)]:
        if bounded_file(path, qualified.OUTPUT_LIMIT)[0] != hashlib.sha256(data).hexdigest():
            raise ValueError('Candidate checksums or report changed during retention.')
    if {path.name for path in output.iterdir()} != {unsigned_package.name, final.name, 'SHA256SUMS', 'windows-signing-candidate.json'}:
        raise ValueError('Unexpected Windows candidate output.')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'portable', 'identity', 'signtool', 'powershell', 'makeappx', 'work', 'output']:
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ['commit', 'version', 'package-family', 'portable-sha256', 'certificate-sha1',
                 'certificate-sha256', 'timestamp-uri', 'signtool-sha256', 'powershell-sha256', 'makeappx-sha256', 'revision']:
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--approve-existing-certificate-signing', action='store_true')
    arguments = vars(parser.parse_args())
    root, identity = arguments.pop('source_root'), arguments.pop('identity')
    try:
        checksum, size = bounded_file(identity, 16 * 1024)
        data = identity.read_bytes()
        if not size or len(data) != size or hashlib.sha256(data).hexdigest() != checksum:
            raise ValueError('Public signing identity changed.')
        result = produce(root, **arguments, identity=json.loads(data, object_pairs_hook=unique_fields))
        if bounded_file(identity, 16 * 1024) != (checksum, size):
            raise ValueError('Public signing identity changed during production.')
    except Exception:
        print('Windows signing failed or disabled; preserve private work. No installation or publication performed.', file=sys.stderr)
        return 1
    print('Selected EXE and MSIX signatures verified; trusted producer and installed acceptance remain unverified.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
