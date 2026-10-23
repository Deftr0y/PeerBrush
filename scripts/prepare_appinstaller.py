"""Prepare App Installer candidate files from explicitly selected media.

No signing, installation, upload, automatic-update settings or trust changes.
This accepts no qualification report and makes no public-readiness claim.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import stat
import sys
from urllib.parse import urlsplit, urlunsplit
import xml.etree.ElementTree as ET
import zipfile

import package_msix as msix
from package_macos_dmg import bounded_file, inspection_path, private_inspection_directory
from production_readiness import unique_fields
import qualify_signed_msix as signed

INSTALLER_NS = 'http://schemas.microsoft.com/appx/appinstaller/2017'
MANIFEST_LIMIT = 64 * 1024


def canonical_uri(value, filename):
    """Only explicit canonical HTTPS selections; no transport or availability claim."""
    if type(value) is not str or not value.isascii() or not 1 <= len(value) <= 2084:
        raise ValueError('App Installer requires explicit canonical HTTPS locations.')
    uri = urlsplit(value)
    if (uri.scheme != 'https' or not 1 <= len(uri.netloc) <= 253
            or not any(character.isalpha() for character in uri.netloc)
            or not re.fullmatch(r'[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?', uri.netloc)
            or '.' not in uri.netloc or uri.hostname != uri.netloc
            or any(not re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', label) for label in uri.netloc.split('.'))
            or uri.query or uri.fragment or urlunsplit(uri) != value
            or not re.fullmatch(r'/[A-Za-z0-9._~/-]+', uri.path)
            or any(part in ['', '.', '..'] for part in uri.path[1:].split('/'))
            or uri.path.rsplit('/', 1)[-1] != filename):
        raise ValueError('App Installer requires exact canonical HTTPS filenames without credentials or path aliases.')
    return value


def package_version(value):
    if type(value) is not str or not re.fullmatch(r'(0|[1-9][0-9]*)(?:\.(0|[1-9][0-9]*)){3}', value):
        raise ValueError('Framework versions must be canonical four-part Windows versions.')
    parts = tuple(map(int, value.split('.')))
    if any(part > 65535 for part in parts):
        raise ValueError('Framework version exceeds the Windows package bounds.')
    return parts


def manifest(path):
    """Read only a bounded canonical package manifest; native trust is separate."""
    with zipfile.ZipFile(path) as archive:
        entries = archive.infolist()
        if (not 1 <= len(entries) <= 16384 or len(entries) != len({entry.filename.casefold() for entry in entries})
                or sum(entry.file_size for entry in entries) > msix.PACKAGE_LIMIT):
            raise ValueError('App Installer input exceeds its package inventory budget.')
        for entry in entries:
            msix.portable.relative_path(entry.filename.rstrip('/'))
            if (entry.orig_filename != entry.filename or entry.flag_bits & 1
                    or entry.compress_type not in [zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED]
                    or stat.S_IFMT(entry.external_attr >> 16) not in [0, stat.S_IFREG, stat.S_IFDIR]):
                raise ValueError('App Installer input contains an unsupported package entry.')
        entry = archive.getinfo('AppxManifest.xml')
        if entry.is_dir() or not 0 < entry.file_size <= MANIFEST_LIMIT:
            raise ValueError('App Installer needs a bounded package manifest.')
        with archive.open(entry) as stream:
            data = stream.read(MANIFEST_LIMIT + 1)
            if len(data) != entry.file_size or stream.read(1):
                raise ValueError('Package manifest changed or exceeded its byte budget.')
    text = data.decode('utf-8-sig')
    if '<!DOCTYPE' in text.upper() or '<!ENTITY' in text.upper():
        raise ValueError('Package manifests cannot define external or internal entities.')
    root = ET.fromstring(text)
    if root.tag != '{' + msix.NS['f'] + '}Package':
        raise ValueError('App Installer requires a Windows package manifest.')
    return root


def framework_identity(package, runtime):
    required = manifest(package).findall('f:Dependencies/f:PackageDependency', msix.NS)
    if (len(required) != 1 or set(required[0].attrib) != {'Name', 'Publisher', 'MinVersion'}
            or required[0].get('Name') != 'Microsoft.VCLibs.140.00.UWPDesktop'):
        raise ValueError('Only the source-selected Desktop VCLibs framework is supported.')
    expected = required[0].attrib
    root = manifest(runtime)
    identities = root.findall('f:Identity', msix.NS)
    frameworks = root.findall('f:Properties/f:Framework', msix.NS)
    if (len(identities) != 1 or len(frameworks) != 1 or frameworks[0].text != 'true'
            or len(frameworks[0]) or frameworks[0].attrib
            or root.findall('f:Applications', msix.NS)
            or root.findall('f:Dependencies/f:PackageDependency', msix.NS)):
        raise ValueError('Runtime must be the selected standalone retail framework package.')
    identity = identities[0].attrib
    if (not {'Name', 'Publisher', 'Version', 'ProcessorArchitecture'} <= set(identity)
            or set(identity) - {'Name', 'Publisher', 'Version', 'ProcessorArchitecture', 'ResourceId'}
            or identity.get('Name') != expected['Name'] or identity.get('Publisher') != expected['Publisher']
            or identity.get('ProcessorArchitecture') != 'x64' or identity.get('ResourceId', '')
            or package_version(identity.get('Version')) < package_version(expected['MinVersion'])):
        raise ValueError('Runtime identity, architecture or version differs from the source dependency.')
    return {key: identity[key] for key in ['Name', 'Publisher', 'Version', 'ProcessorArchitecture']}


def descriptor_bytes_for_selection(identity, dependency, version, descriptor_uri, package_uri, runtime_uri):
    """Canonical descriptor serialization shared by native preparation and byte binding.

    The caller validates identities/media; this function establishes no native trust.
    """
    root_attributes = dict(xmlns=INSTALLER_NS, Version=version, Uri=descriptor_uri)
    main_attributes = dict(Name=identity['name'], Publisher=identity['publisher'],
        Version=version, ProcessorArchitecture='x64', Uri=package_uri)
    dependency_attributes = dict(dependency, Uri=runtime_uri)
    # App Installer's native text contract is narrower than signed MSIX identity:
    # https://learn.microsoft.com/en-us/uwp/schemas/appinstallerschema/element-appinstaller#remarks
    for attributes in [root_attributes, main_attributes, dependency_attributes]:
        for field, value in attributes.items():
            if (type(value) is not str or not value.isascii()
                    or any(ord(character) < 32 or ord(character) == 127 or character in '&<>"' for character in value)):
                raise ValueError(f'App Installer {field} requires literal printable ASCII without XML escaping; the signed MSIX may remain valid.')
    installer = ET.Element('AppInstaller', **root_attributes)
    ET.SubElement(installer, 'MainPackage', **main_attributes)
    ET.SubElement(ET.SubElement(installer, 'Dependencies'), 'Package', **dependency_attributes)
    descriptor_bytes = ET.tostring(installer, encoding='UTF-8', xml_declaration=True) + b'\n'
    if len(descriptor_bytes) > 16 * 1024 or not descriptor_bytes.isascii() or b'&' in descriptor_bytes:
        raise ValueError('App Installer descriptor exceeds its bound or requires unsupported XML text.')
    return descriptor_bytes


def prepare(root, portable, unsigned, package, runtime, *, identity, runtime_license, commit, version, package_family,
            portable_sha256, unsigned_sha256, package_sha256, certificate_sha256,
            runtime_sha256, runtime_certificate_sha256, runtime_license_sha256, descriptor_uri, package_uri, runtime_uri,
            signtool, signtool_sha256, powershell, powershell_sha256, work, output,
             require_executable_signature=False, return_qualification=False):
    if type(return_qualification) is not bool or (return_qualification and require_executable_signature is not True):
        raise ValueError('Native installer qualification requires strict signed-executable verification.')
    if sys.platform != 'win32':
        raise ValueError('App Installer preparation requires an actual Windows verification host.')
    if any(not signed._sha(value) for value in [runtime_sha256, runtime_certificate_sha256, runtime_license_sha256]):
        raise ValueError('Runtime media, public leaf certificate and reviewed license SHA-256 selections are required.')
    root = inspection_path(root)
    signed._source(root, commit)
    paths = [inspection_path(path) for path in [portable, unsigned, package, runtime, identity, runtime_license]]
    portable, unsigned, package, runtime, identity, runtime_license = paths
    if (not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,126}\.(?:appx|msix)', runtime.name)
            or len(set(paths)) != len(paths)):
        raise ValueError('Select separate exact package, framework and public identity files.')
    tools = [signed._selected_tool(signtool, signtool_sha256, 'signtool.exe'),
             signed._selected_tool(powershell, powershell_sha256, 'pwsh.exe')]
    work = private_inspection_directory(work, root, fresh=True)
    output = private_inspection_directory(output, root, fresh=True)
    if work == output or any(directory.is_relative_to(path.parent) for directory in [work, output] for path in [*paths, *tools]):
        raise ValueError('Use separate fresh private work/output directories outside inputs and source.')
    work.mkdir(mode=0o700)
    work.chmod(0o700)
    private_inspection_directory(work, root, fresh=False)
    identity_copy = work / 'public-identity.json'
    identity_hash, identity_size = bounded_file(identity, 16 * 1024, identity_copy)
    identity_copy.chmod(0o400)
    if not identity_size:
        raise ValueError('Missing selected public direct-distribution identity.')
    selected_identity = json.loads(identity_copy.read_bytes(), object_pairs_hook=unique_fields)
    license_copy = work / 'runtime-license.txt'
    license_hash, license_size = bounded_file(runtime_license, 256 * 1024, license_copy)
    license_copy.chmod(0o400)
    if (not license_size or license_hash != runtime_license_sha256
            or not license_copy.read_bytes().decode('utf-8-sig').strip()):
        raise ValueError('Runtime license differs from its independently reviewed selection.')
    result = signed.inspect_final(root, portable, unsigned, package,
        commit=commit, version=version, identity=selected_identity, package_family=package_family,
        portable_sha256=portable_sha256, unsigned_sha256=unsigned_sha256, package_sha256=package_sha256,
        certificate_sha256=certificate_sha256, signtool=tools[0], signtool_sha256=signtool_sha256,
        powershell=tools[1], powershell_sha256=powershell_sha256, work=work / 'peerbrush',
        require_executable_signature=require_executable_signature)
    descriptor = 'PeerBrush-' + result['package_version'] + '-Windows-x64.appinstaller'
    canonical_uri(descriptor_uri, descriptor)
    canonical_uri(package_uri, package.name)
    canonical_uri(runtime_uri, runtime.name)
    msix.validate_casefold_paths([descriptor, package.name, runtime.name, 'SHA256SUMS'])
    runtime_work = work / 'framework'
    runtime_work.mkdir(mode=0o700)
    runtime_work.chmod(0o700)
    private_inspection_directory(runtime_work, root, fresh=False)
    owned_runtime = runtime_work / runtime.name
    actual, size = bounded_file(runtime, msix.PACKAGE_LIMIT, owned_runtime)
    if not size or actual != runtime_sha256:
        raise ValueError('Runtime differs from its independently selected SHA-256.')
    owned_runtime.chmod(0o400)
    owned_package = work / 'peerbrush' / package.name
    dependency = framework_identity(owned_package, owned_runtime)
    for tool, checksum in zip(tools, [signtool_sha256, powershell_sha256]):
        signed._selected_tool(tool, checksum, tool.name.casefold())
    runtime_metadata = signed.verify_native_signature(owned_runtime, dependency['Publisher'], runtime_certificate_sha256,
        tools[0], tools[1], runtime_work)
    # Emit no automatic-update configuration. Existing OS/provider policies can
    # still apply; association does not establish shared unsaved-work protection.
    descriptor_bytes = descriptor_bytes_for_selection(selected_identity, dependency, result['package_version'],
        descriptor_uri, package_uri, runtime_uri)
    def unchanged():
        signed._source(root, commit)
        private_inspection_directory(work, root, fresh=False)
        private_inspection_directory(work / 'peerbrush', root, fresh=False)
        private_inspection_directory(runtime_work, root, fresh=False)
        for original, owned, checksum, limit in [
            (portable, work / 'peerbrush' / portable.name, portable_sha256, msix.PACKAGE_LIMIT),
            (unsigned, work / 'peerbrush' / unsigned.name, unsigned_sha256, msix.PACKAGE_LIMIT),
            (package, owned_package, package_sha256, msix.PACKAGE_LIMIT),
            (runtime, owned_runtime, runtime_sha256, msix.PACKAGE_LIMIT),
            (identity, identity_copy, identity_hash, 16 * 1024),
            (runtime_license, license_copy, runtime_license_sha256, 256 * 1024)]:
            if bounded_file(original, limit)[0] != checksum or bounded_file(owned, limit)[0] != checksum:
                raise ValueError('Candidate inputs changed; preserve private work without publication.')
        for tool, checksum in zip(tools, [signtool_sha256, powershell_sha256]):
            signed._selected_tool(tool, checksum, tool.name.casefold())
    unchanged()
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    private_inspection_directory(output, root, fresh=False)
    expected = {descriptor: hashlib.sha256(descriptor_bytes).hexdigest(),
                package.name: package_sha256, runtime.name: runtime_sha256}
    for original in [owned_package, owned_runtime]:
        copied = output / original.name
        if bounded_file(original, msix.PACKAGE_LIMIT, copied)[0] != expected[original.name]:
            raise ValueError('Candidate copy differs from its native-verified media.')
        copied.chmod(0o400)
    with (output / descriptor).open('xb') as stream:
        stream.write(descriptor_bytes)
    checksums = ''.join(expected[name] + '  ' + name + '\n' for name in sorted(expected)).encode('ascii')
    with (output / 'SHA256SUMS').open('xb') as stream:
        stream.write(checksums)
    unchanged()
    private_inspection_directory(output, root, fresh=False)
    if (set(path.name for path in output.iterdir()) != set(expected) | {'SHA256SUMS'}
            or any(bounded_file(output / name, msix.PACKAGE_LIMIT)[0] != checksum for name, checksum in expected.items())
            or bounded_file(output / 'SHA256SUMS', 4096)[0] != hashlib.sha256(checksums).hexdigest()):
        raise ValueError('Exclusive candidate files changed before preparation completed.')
    if return_qualification:
        qualification = dict(schema=1, format='PeerBrushWindowsInstallerQualificationV1', commit=commit, version=version,
            signed_package=result, identity_sha256=identity_hash,
            runtime=dict(file=runtime.name, sha256=runtime_sha256, identity=dependency,
                certificate_sha256=runtime_certificate_sha256,
                timestamp_certificate_sha256=runtime_metadata['timestamp_certificate_sha256'],
                license_sha256=runtime_license_sha256),
            descriptor=dict(file=descriptor, sha256=expected[descriptor], uri=descriptor_uri,
                package_uri=package_uri, runtime_uri=runtime_uri),
            native_producer_authentication='unverified-requires-trusted-native-job',
            installed_acceptance='not-performed', runtime_redistribution_approval='not-authenticated',
            public_availability='unverified', ready=False, production_ready=False)
        return output / descriptor, qualification
    return output / descriptor


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source-root', 'portable', 'unsigned', 'package', 'runtime', 'identity', 'runtime-license', 'signtool', 'powershell', 'work', 'output']:
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ['commit', 'version', 'package-family', 'portable-sha256', 'unsigned-sha256', 'package-sha256',
                 'certificate-sha256', 'runtime-sha256', 'runtime-certificate-sha256', 'runtime-license-sha256',
                 'descriptor-uri', 'package-uri', 'runtime-uri', 'signtool-sha256', 'powershell-sha256']:
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--require-executable-signature', action='store_true',
                        help='Require separate source-bound executable Authenticode verification')
    arguments = vars(parser.parse_args())
    root = arguments.pop('source_root')
    try:
        prepare(root, **arguments)
    except Exception:
        print('App Installer candidate preparation failed; no signing, installation or publication performed.', file=sys.stderr)
        return 1
    print('App Installer candidate files prepared; no automatic-update configuration emitted. Installation and public availability remain unverified.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
