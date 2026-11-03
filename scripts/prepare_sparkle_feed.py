"""Prepare one private stable Sparkle appcast for an exact, notarized PeerBrush DMG."""
import argparse
import base64
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from xml.etree import ElementTree as ET

from package import git, source_files
from package_macos_dmg import MAX_IMAGE_BYTES, bounded_file, inspect_final, private_inspection_directory
from prepare_site_release import DOWNLOAD, STABLE
from sparkle_bundle import check_pins

# bin/sign_update in the official Sparkle 2.10.0 release (not the SPM framework ZIP).
SIGNER = ('43c249771bafc3aa581228abae00731a012d324691b8292860896635050be76b', 1358624)
NS = 'http://www.andymatuschak.org/xml-namespaces/sparkle'
VERIFY = """import CryptoKit
import Foundation
let a = CommandLine.arguments
guard let publicBytes = Data(base64Encoded:a[1]), let sig = Data(base64Encoded:a[2]) else { exit(1) }
let key = try Curve25519.Signing.PublicKey(rawRepresentation:publicBytes)
let data = try Data(contentsOf:URL(fileURLWithPath:a[3]),options:.mappedIfSafe)
exit(key.isValidSignature(sig, for:data) ? 0 : 1)
"""


def feed_bytes(version, record, notes, signature):
    ET.register_namespace('sparkle', NS)
    root = ET.Element('rss', version='2.0')
    channel = ET.SubElement(root, 'channel')
    ET.SubElement(channel, 'title').text = 'PeerBrush for macOS'
    item = ET.SubElement(channel, 'item')
    ET.SubElement(item, 'title').text = 'PeerBrush ' + version
    ET.SubElement(item, f'{{{NS}}}version').text = version
    ET.SubElement(item, f'{{{NS}}}minimumSystemVersion').text = '12.0.0'
    ET.SubElement(item, f'{{{NS}}}hardwareRequirements').text = 'arm64'
    ET.SubElement(item, 'description', {f'{{{NS}}}format': 'plain-text'}).text = notes
    ET.SubElement(item, 'enclosure', {'url': DOWNLOAD + 'v' + version + '/' + record['file'],
                  'length': str(record['bytes']), 'type': 'application/octet-stream', f'{{{NS}}}edSignature': signature})
    ET.indent(root)
    return ET.tostring(root, encoding='utf-8', xml_declaration=True) + b'\n'


def prepare(args, key_input, *, private_loopback=False):
    root = Path(__file__).resolve().parents[1]
    if (sys.platform != 'darwin' or not re.fullmatch('[0-9a-f]{40}', args.commit)
            or git(root, 'rev-parse', 'HEAD').decode().strip() != args.commit
            or git(root, 'status', '--porcelain', '--untracked-files=normal')
            or not all(re.fullmatch(STABLE, v) for v in (args.version, args.previous_version))
            or tuple(map(int, args.version.split('.'))) <= tuple(map(int, args.previous_version.split('.')))):
        raise ValueError('Requires a clean reviewed macOS checkout and a strictly newer stable version.')
    pins = check_pins({p: b for p, b, _ in source_files(root, args.commit)}, args.version)
    if pins is None:
        raise ValueError('Approved public feed configuration is missing.')
    output = private_inspection_directory(args.output, root, fresh=True)
    work = private_inspection_directory(args.work, root, fresh=True)
    # Notes must be explicitly selected for public display. No HTML or remote notes.
    output.mkdir(mode=0o700)
    with tempfile.TemporaryDirectory(dir=output) as temp:
        stage = Path(temp)
        bounded_file(args.notes, 65536, stage / 'notes')
        notes = (stage / 'notes').read_text(encoding='utf-8')
        if not notes.strip() or any(ord(c) < 32 and c not in '\t\r\n' for c in notes):
            raise ValueError('Invalid public notes.')
        signer = stage / 'sign_update'
        if bounded_file(args.signer, SIGNER[1], signer, executable=True) != SIGNER:
            raise ValueError('Incorrect Sparkle 2.10 signer.')
        signer.chmod(0o500)
        record = inspect_final(root, args.archive, args.image, commit=args.commit, version=args.version,
                               archive_sha256=args.archive_sha256, image_sha256=args.image_sha256,
                               team_id=pins['publisher_team_id'], work=work)['record']
        image = stage / record['file']
        if bounded_file(args.image, MAX_IMAGE_BYTES, image) != (record['sha256'], record['bytes']):
            raise ValueError('Qualified DMG changed.')
        # Only this tool gets the bounded seed, via a pipe. Upstream error output
        # can echo malformed input, so it must never be forwarded to a log.
        key = key_input.read(47).removesuffix(b'\n')
        decoded = base64.b64decode(key, validate=True)
        if len(decoded) != 32 or base64.b64encode(decoded) != key:
            raise ValueError('Invalid signing input.')
        options = dict(cwd=stage, env={'HOME': temp, 'TMPDIR': temp, 'PATH': '/usr/bin:/bin'},
                       stderr=subprocess.DEVNULL, check=True, timeout=120)
        def sign(path):
            return subprocess.run([signer, '-p', '--disable-signing-warning', '--ed-key-file', '-', path],
                                  input=key+b'\n', stdout=subprocess.PIPE, **options).stdout.strip()
        def verify(path, signature):
            if len(base64.b64decode(signature, validate=True)) != 64:
                raise ValueError('Invalid signature length.')
            subprocess.run(['/usr/bin/swift', '-e', VERIFY, pins['public_ed_key'], signature, path],
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, **options)
        signature = sign(image).decode('ascii')
        verify(image, signature)
        content = feed_bytes(args.version, record, notes, signature)
        if private_loopback:
            if (args.version not in ('0.29.99', '0.30.0') or os.environ.get('GITHUB_JOB') != 'sign'
                    or os.environ.get('GITHUB_REF') != 'refs/heads/main'
                    or os.environ.get('GITHUB_WORKFLOW_REF') != 'Deftr0y/PeerBrush/.github/workflows/macos-release-signing.yml@refs/heads/main'
                    or os.environ.get('SELECTED_COMMIT') != os.environ.get('GITHUB_WORKFLOW_SHA')
                    or os.environ.get('GITHUB_RUN_ATTEMPT') != '1'
                    or content.count(DOWNLOAD.encode()) != 1):
                raise ValueError('Private appcasts require the fixed reviewed signing workflow.')
            content = content.replace(DOWNLOAD.encode(), b'http://localhost:38573/downloads/')
        ET.fromstring(content)  # Reject text containing XML-forbidden Unicode code points.
        xml = stage / 'appcast.xml'
        plain = stage / 'content'
        plain.write_bytes(content)
        xml.write_bytes(content)
        sign(xml)
        signed = xml.read_bytes()
        trailer = re.fullmatch(rb'<!-- sparkle-signatures:\nedSignature: ([A-Za-z0-9+/]{86}==)\nlength: ([0-9]+)\n-->\n', signed[len(content):])
        if not signed.startswith(content) or not trailer or int(trailer[2]) != len(content):
            raise ValueError('Unexpected signed feed bytes.')
        verify(plain, trailer[1].decode('ascii'))
        if bounded_file(image, MAX_IMAGE_BYTES) != (record['sha256'], record['bytes']):
            raise ValueError('Signed enclosure differs from the qualified DMG.')
        # Publish only to the new private review directory, never to the website.
        os.link(xml, output / 'appcast.xml')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('signer', 'archive', 'image', 'notes', 'work', 'output'):
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ('commit', 'version', 'previous-version', 'archive-sha256', 'image-sha256'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    try:
        if sys.stdin.isatty():
            raise ValueError('Use a protected pipe.')
        # Never let a Git, native inspection or Swift subprocess inherit the secret pipe.
        with os.fdopen(os.dup(0), 'rb') as key_input, open(os.devnull, 'rb') as empty:
            os.dup2(empty.fileno(), 0)
            prepare(args, key_input)
    except Exception:
        parser.exit(1, 'Private appcast preparation failed; no feed was published.\n')
    print('Verified private appcast prepared; installed acceptance and public deployment are separate.')


if __name__ == '__main__':
    main()
