"""Read-only final signed MSIX verification; no signing, installation or publishing.

All publisher/tool/input selections are explicit. The unsigned Store candidate
path is unchanged; this consumer never reads or trusts a signer's JSON receipt.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
import zipfile

import package_msix as msix
from package_macos_dmg import bounded_file, inspection_path, private_inspection_directory
from production_readiness import unique_fields

TOOL_LIMIT = 128 * 1024 * 1024
OUTPUT_LIMIT = 16 * 1024
POWERSHELL_VERSION = '7.6.5'
SIGNATURE = 'AppxSignature.p7x'
TYPES = '[Content_Types].xml'
TYPES_NS = '{http://schemas.openxmlformats.org/package/2006/content-types}'

# Microsoft's native cmdlet exposes verified provider state. Catalog results are
# expressly rejected. SignTool, not SignatureStatus.Valid alone, supplies trust.
# https://github.com/PowerShell/PowerShell/blob/v7.6.5/src/System.Management.Automation/security/Authenticode.cs
CERTIFICATE_SCRIPT = r'''
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$PSModuleAutoLoadingPreference = 'None'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
try {
    if ($PSVersionTable.PSVersion.ToString() -cne '7.6.5') { throw 'version' }
    Import-Module -Name ([IO.Path]::Combine($PSHOME, 'Modules', 'Microsoft.PowerShell.Security', 'Microsoft.PowerShell.Security.psd1')) -ErrorAction Stop
    Import-Module -Name ([IO.Path]::Combine($PSHOME, 'Modules', 'Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Utility.psd1')) -ErrorAction Stop
    $signature = Microsoft.PowerShell.Security\Get-AuthenticodeSignature -LiteralPath $env:PEERBRUSH_MSIX_INPUT -ErrorAction Stop
    if ($null -eq $signature -or $signature -is [array] -or
        [int]$signature.Status -ne 0 -or [int]$signature.SignatureType -ne 1 -or
        $null -eq $signature.SignerCertificate -or $null -eq $signature.TimeStamperCertificate) { throw 'signature' }
    [void][Reflection.Assembly]::Load('System.Security.Cryptography.Pkcs')
    [void][Reflection.Assembly]::Load('System.IO.Compression.ZipFile')
    $archive = [IO.Compression.ZipFile]::OpenRead($env:PEERBRUSH_MSIX_INPUT)
    try {
        $entry = $archive.GetEntry('AppxSignature.p7x')
        if ($null -eq $entry -or $entry.Length -le 4 -or $entry.Length -gt 4194304) { throw 'size' }
        $bytes = [byte[]]::new([int]$entry.Length)
        $stream = $entry.Open()
        try { $stream.ReadExactly($bytes, 0, $bytes.Length); if ($stream.ReadByte() -ne -1) { throw 'length' } }
        finally { $stream.Dispose() }
    } finally { $archive.Dispose() }
    if ([BitConverter]::ToUInt32($bytes, 0) -ne 0x58434b50) { throw 'header' }
    $cms = [Security.Cryptography.Pkcs.SignedCms]::new()
    $cms.Decode([byte[]]($bytes[4..($bytes.Length-1)]))
    if ($cms.SignerInfos.Count -ne 1 -or $cms.ContentInfo.ContentType.Value -cne '1.3.6.1.4.1.311.2.1.4') { throw 'signers' }
    $signerInfo = $cms.SignerInfos[0]
    if ($null -eq $signerInfo.Certificate -or $signerInfo.DigestAlgorithm.Value -cne '2.16.840.1.101.3.4.2.1' -or
        $signerInfo.UnsignedAttributes.Count -ne 1) { throw 'attributes' }
    $attribute = $signerInfo.UnsignedAttributes[0]
    if ($attribute.Oid.Value -cnotin @('1.2.840.113549.1.9.6', '1.3.6.1.4.1.311.3.3.1') -or
        $attribute.Values.Count -ne 1) { throw 'timestamp' }
    if ($attribute.Oid.Value -ceq '1.2.840.113549.1.9.6') {
        if ($signerInfo.CounterSignerInfos.Count -ne 1) { throw 'countersigners' }
        $timestampSigner = $signerInfo.CounterSignerInfos[0]
    } else {
        $token = [Security.Cryptography.Pkcs.SignedCms]::new()
        $token.Decode($attribute.Values[0].RawData)
        if ($token.SignerInfos.Count -ne 1) { throw 'timestamps' }
        $timestampSigner = $token.SignerInfos[0]
    }
    if ($null -eq $timestampSigner.Certificate -or $timestampSigner.CounterSignerInfos.Count -ne 0 -or
        $timestampSigner.UnsignedAttributes.Count -ne 0) { throw 'timestamp-signer' }
    $hash = [Security.Cryptography.SHA256]::Create()
    try {
        $signer = [Convert]::ToHexString($hash.ComputeHash($signature.SignerCertificate.RawData)).ToLowerInvariant()
        $cmsSigner = [Convert]::ToHexString($hash.ComputeHash($signerInfo.Certificate.RawData)).ToLowerInvariant()
        $timestamp = [Convert]::ToHexString($hash.ComputeHash($signature.TimeStamperCertificate.RawData)).ToLowerInvariant()
        $cmsTimestamp = [Convert]::ToHexString($hash.ComputeHash($timestampSigner.Certificate.RawData)).ToLowerInvariant()
        if ($cmsSigner -cne $signer) { throw 'ambiguous' }
        if ($cmsTimestamp -cne $timestamp) { throw 'ambiguous-timestamp' }
    } finally { $hash.Dispose() }
    [Console]::Write((@{schema=1; powershell=$PSVersionTable.PSVersion.ToString(); status=[int]$signature.Status;
        kind=[int]$signature.SignatureType; subject=$signature.SignerCertificate.Subject;
        certificate_sha256=$signer; timestamp_certificate_sha256=$timestamp;
        signers=1; timestamp_attributes=1; digest='sha256'} | ConvertTo-Json -Compress))
    exit 0
} catch { exit 1 }
'''


def _sha(value):
    return type(value) is str and re.fullmatch('[0-9a-f]{64}', value) is not None


def _source(root, commit):
    if (msix.portable.git(root, 'rev-parse', 'HEAD').decode().strip() != commit
            or msix.portable.git(root, 'status', '--porcelain', '--untracked-files=no').strip()):
        raise ValueError('Final MSIX requires unchanged exact committed source.')


def _selected_tool(path, checksum, name):
    path = inspection_path(path)
    if path.name.casefold() != name or not _sha(checksum):
        raise ValueError('Final MSIX requires explicitly selected native tool bytes.')
    actual, size = bounded_file(path, TOOL_LIMIT, executable=True)
    if not size or actual != checksum:
        raise ValueError('Native verification tool differs from its selected SHA-256.')
    return path


def _command(executable, arguments, work, label, *, package=None):
    """Fixed commands, redacted failures, output polling guards and owned child cleanup."""
    environment = dict(PATH=os.defpath, HOME=str(work), USERPROFILE=str(work),
                       APPDATA=str(work), LOCALAPPDATA=str(work), TEMP=str(work), TMP=str(work),
                       PSModulePath='', POWERSHELL_TELEMETRY_OPTOUT='1', POWERSHELL_UPDATECHECK='Off')
    for name in ['SYSTEMROOT', 'WINDIR']:
        value = next((value for key, value in os.environ.items() if key.upper() == name), None)
        if value:
            environment[name] = value
    if package is not None:
        environment['PEERBRUSH_MSIX_INPUT'] = str(package)
    output = work / (label + '.stdout')
    with output.open('xb') as stream:
        try:
            child = subprocess.Popen([str(executable), *arguments], cwd=work, env=environment,
                stdin=subprocess.DEVNULL, stdout=stream, stderr=subprocess.DEVNULL,
                close_fds=True, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            try:
                deadline = time.monotonic() + 120
                while True:
                    if os.fstat(stream.fileno()).st_size > OUTPUT_LIMIT or time.monotonic() >= deadline:
                        raise ValueError('Native MSIX verification exceeded its output or time budget.')
                    code = child.poll()
                    if code is not None:
                        # SignTool exit 2 means warnings, including missing timestamp
                        # under /tw. No localized output or success text is parsed.
                        if code != 0:
                            raise ValueError('Native MSIX verification did not complete without warnings.')
                        break
                    time.sleep(0.05)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=10)
        except (OSError, subprocess.SubprocessError):
            raise ValueError('Native MSIX verification could not complete.') from None
    if output.stat().st_size > OUTPUT_LIMIT:
        raise ValueError('Native MSIX verification exceeded its output budget.')
    return output.read_bytes()


def _certificate(data, publisher, certificate_sha256):
    """Only accepts output produced directly by our fixed native metadata command."""
    if not isinstance(data, bytes) or not 0 < len(data) <= OUTPUT_LIMIT:
        raise ValueError('Native MSIX certificate metadata is unavailable.')
    try:
        value = json.loads(data, object_pairs_hook=unique_fields)
    except (ValueError, UnicodeError):
        raise ValueError('Native MSIX certificate metadata is invalid.') from None
    expected = dict(schema=1, powershell=POWERSHELL_VERSION, status=0, kind=1,
                    subject=publisher, certificate_sha256=certificate_sha256,
                    signers=1, timestamp_attributes=1, digest='sha256')
    if (type(value) is not dict or set(value) != set(expected) | {'timestamp_certificate_sha256'}
            or any(type(value.get(key)) is not type(item) or value.get(key) != item for key, item in expected.items())
            or not _sha(value.get('timestamp_certificate_sha256'))):
        raise ValueError('Native MSIX signer, timestamp or embedded-signature policy differs from selection.')
    return value


def _unsigned_binding(unsigned, signed):
    """Only signature addition and its one content-type declaration may differ."""
    with zipfile.ZipFile(unsigned) as before, zipfile.ZipFile(signed) as after:
        if set(after.namelist()) != set(before.namelist()) | {SIGNATURE}:
            raise ValueError('Signing changed the unsigned package inventory.')
        # Both complete payloads were already byte-checked against the same
        # reconstructed expected map. Compare the bounded raw block map as well.
        if before.read('AppxBlockMap.xml') != after.read('AppxBlockMap.xml'):
            raise ValueError('Signing changed the original block map.')
        old, new = ET.fromstring(before.read(TYPES)), ET.fromstring(after.read(TYPES))
        added = [node for node in new if node.tag == TYPES_NS + 'Override'
                 and node.attrib == {'PartName': '/' + SIGNATURE, 'ContentType': 'application/vnd.ms-appx.signature'}]
        if len(added) != 1:
            raise ValueError('Signing must add exactly the signature content type.')
        new.remove(added[0])
        canonical = lambda root: (root.tag, tuple(sorted(root.attrib.items())),
            tuple(sorted((node.tag, tuple(sorted(node.attrib.items()))) for node in root)))
        if canonical(old) != canonical(new):
            raise ValueError('Signing changed an unsigned content type.')


def inspect_final(root, portable, unsigned, package, *, commit, version, identity,
                  package_family, portable_sha256, unsigned_sha256, package_sha256,
                  certificate_sha256, signtool, signtool_sha256, powershell, powershell_sha256, work):
    """Reconstruct source-derived content, then authenticate actual selected final bytes.

    This is Windows-native acceptance of these bytes on this host, not native-job
    provenance authentication, installation, public availability or release readiness.
    """
    if sys.platform != 'win32':
        raise ValueError('Final signed MSIX verification requires an actual Windows host.')
    if (type(commit) is not str or not re.fullmatch('[0-9a-f]{40}', commit)
            or type(version) is not str or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version)
            or any(not _sha(value) for value in [portable_sha256, unsigned_sha256, package_sha256, certificate_sha256])):
        raise ValueError('Final MSIX requires exact stable source, inputs and public signer certificate SHA-256.')
    msix.identity_values(identity)
    family = msix.validate_package_family(identity, package_family)
    target = msix.package_version(version)
    root = inspection_path(root)
    _source(root, commit)
    paths = [inspection_path(path) for path in [portable, unsigned, package]]
    if (paths[0].name != f'PeerBrush-{version}-Windows-x64.zip'
            or paths[1].name != f'PeerBrush-{target}-Windows-x64.msix'
            or not re.fullmatch(re.escape(paths[1].stem) + r'-signed-r[1-9][0-9]*\.msix', paths[2].name)
            or len(set(paths)) != 3):
        raise ValueError('Final MSIX inputs must select the exact same release and a signed revision.')
    tools = [_selected_tool(signtool, signtool_sha256, 'signtool.exe'),
             _selected_tool(powershell, powershell_sha256, 'pwsh.exe')]
    work = private_inspection_directory(work, root, fresh=True)
    if any(work.is_relative_to(path.parent) for path in [*paths, *tools]):
        raise ValueError('Final MSIX work must be separate from input and native tool directories.')
    work.mkdir(mode=0o700)
    os.chmod(work, 0o700)
    private_inspection_directory(work, root, fresh=False)
    copies = []
    for source, checksum in zip(paths, [portable_sha256, unsigned_sha256, package_sha256]):
        destination = work / source.name
        actual, size = bounded_file(source, msix.PACKAGE_LIMIT, destination)
        if not size or actual != checksum:
            raise ValueError('Final MSIX input differs from its independent SHA-256.')
        destination.chmod(0o400)
        copies.append(destination)
    expected, release = msix.expected_payload(root, copies[0], identity, require_provenance=True)
    if release['commit'] != commit or release['version'] != version:
        raise ValueError('Final MSIX source or embedded build differs from selection.')
    msix.verify_payload(copies[1], expected)
    msix.verify_payload(copies[2], expected, signed=True)
    _unsigned_binding(copies[1], copies[2])
    # Embedded package verification, all signatures, Default Authentication policy,
    # timestamp required. Deliberately no catalog search or policy override options.
    # https://learn.microsoft.com/windows/win32/seccrypto/signtool
    _command(tools[0], ['verify', '/pa', '/all', '/tw', '/q', str(copies[2])], work, 'signtool')
    metadata = _certificate(_command(tools[1], ['-NoLogo', '-NoProfile', '-NonInteractive',
        '-Command', CERTIFICATE_SCRIPT], work, 'certificate', package=copies[2]),
        identity['publisher'], certificate_sha256)
    _source(root, commit)
    private_inspection_directory(work, root, fresh=False)
    for source, copied, checksum in zip(paths, copies, [portable_sha256, unsigned_sha256, package_sha256]):
        if bounded_file(source, msix.PACKAGE_LIMIT)[0] != checksum or bounded_file(copied, msix.PACKAGE_LIMIT)[0] != checksum:
            raise ValueError('Final MSIX input changed during native verification; preserve private work.')
    for tool, checksum in zip(tools, [signtool_sha256, powershell_sha256]):
        _selected_tool(tool, checksum, tool.name.casefold())
    return dict(schema=1, format='PeerBrushSignedMsixQualificationV1', commit=commit, version=version,
        package_version=target, package=dict(file=paths[2].name, sha256=package_sha256),
        unsigned_sha256=unsigned_sha256, portable_sha256=portable_sha256,
        identity=dict(identity), package_family_name=family, certificate_sha256=certificate_sha256,
        timestamp_certificate_sha256=metadata['timestamp_certificate_sha256'],
        verifier=dict(signtool_sha256=signtool_sha256, powershell_sha256=powershell_sha256,
                      powershell_version=POWERSHELL_VERSION),
        content_verified=True, native_signature='verified-embedded-default-authentication-policy',
        timestamp='required-by-native-verifier',
        native_producer_authentication='unverified-requires-trusted-native-job',
        executable_signing='not-established', installed_acceptance='not-performed',
        appinstaller='not-configured', public_availability='unverified', ready=False, production_ready=False)


def write_report(result, output, root):
    output = private_inspection_directory(output, root, fresh=True)
    data = (json.dumps(result, sort_keys=True, indent=2) + '\n').encode('utf-8')
    if len(data) > OUTPUT_LIMIT:
        raise ValueError('Final signed MSIX report exceeds its byte budget.')
    output.mkdir(mode=0o700)
    private_inspection_directory(output, root, fresh=False)
    with (output / 'signed-msix-qualification.json').open('xb') as stream:
        stream.write(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'portable', 'unsigned', 'package', 'identity', 'signtool', 'powershell', 'work', 'output']:
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ['commit', 'version', 'package-family', 'portable-sha256', 'unsigned-sha256', 'package-sha256',
                 'certificate-sha256', 'signtool-sha256', 'powershell-sha256']:
        parser.add_argument('--' + name, required=True)
    arguments = vars(parser.parse_args())
    root, output, identity = arguments.pop('source_root'), arguments.pop('output'), arguments.pop('identity')
    try:
        private_inspection_directory(output, root, fresh=True)
        identity_sha256, size = bounded_file(identity, 16 * 1024)
        if not size:
            raise ValueError('Missing public direct-distribution identity.')
        with identity.open('rb') as stream:
            identity_data = stream.read(16 * 1024 + 1)
        if len(identity_data) != size or hashlib.sha256(identity_data).hexdigest() != identity_sha256:
            raise ValueError('Public direct-distribution identity changed during inspection.')
        arguments['identity'] = json.loads(identity_data, object_pairs_hook=unique_fields)
        result = inspect_final(root, **arguments)
        if bounded_file(identity, 16 * 1024) != (identity_sha256, size):
            raise ValueError('Public direct-distribution identity changed during native verification.')
        write_report(result, output, root)
    except Exception:
        print('Final signed MSIX verification failed; no signing, installation or publication performed.', file=sys.stderr)
        return 1
    print('Selected signed MSIX bytes verified; installed acceptance and public availability remain unverified.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
