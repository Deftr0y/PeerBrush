"""Provision pinned public attestation authority; never read signing credentials."""
import argparse
import hashlib
import io
from pathlib import Path
import tarfile
import urllib.request

from final_release import read
from package_macos_dmg import private_inspection_directory
from verify_macos_qualification import CLI_VERSION, LIMITS

ARCHIVE = 'gh_2.102.0_linux_amd64.tar.gz'
ARCHIVE_SHA256 = 'bb766f710eef8ede859c18578c72c327597cd4c8a85b06001b1f3843c6019386'
CLI_SHA256 = '7469124f706944133d6a169691dd1c6c3511b12e85878d255e044e2948df4c9b'
ROOT_SHA256 = '65ca537f6ed8a47fd0e560c421baa1f6c1efb8b25fc200d8c5c02c0e92eb2b9c'
ARCHIVE_LIMIT = 32 * 1024 * 1024


def verifier(archive):
    if CLI_VERSION != '2.102.0' or hashlib.sha256(archive).hexdigest() != ARCHIVE_SHA256:
        raise ValueError('Official verifier archive differs from the reviewed checksum.')
    with tarfile.open(fileobj=io.BytesIO(archive), mode='r:gz') as package:
        member = package.getmember('gh_2.102.0_linux_amd64/bin/gh')
        if not member.isfile() or not 0 < member.size <= LIMITS['gh']:
            raise ValueError('Expected bounded regular verifier executable.')
        with package.extractfile(member) as stream:
            binary = stream.read(LIMITS['gh'] + 1)
    if hashlib.sha256(binary).hexdigest() != CLI_SHA256:
        raise ValueError('Verifier executable differs from its independent checksum.')
    return binary


def provision(root, output):
    output = private_inspection_directory(output, root, fresh=True)
    public_root = read(root / 'packaging/attestation/trusted_root.jsonl', LIMITS['trusted-root'], ROOT_SHA256)
    url = 'https://github.com/cli/cli/releases/download/v2.102.0/' + ARCHIVE
    with urllib.request.urlopen(url, timeout=60) as response:
        archive = response.read(ARCHIVE_LIMIT + 1)
    if len(archive) > ARCHIVE_LIMIT:
        raise ValueError('Verifier archive exceeds its size limit.')
    binary = verifier(archive)
    output.mkdir(mode=0o700)
    for platform in ['macos', 'windows', 'linux', 'store']:
        area = output / platform
        area.mkdir(mode=0o700)
        for name, data in [('gh', binary), ('trusted_root.jsonl', public_root)]:
            with (area / name).open('xb') as stream:
                stream.write(data)
            (area / name).chmod(0o500 if name == 'gh' else 0o400)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    provision(Path(__file__).resolve().parents[1], args.output)
