"""Isolated native verification of selected APT metadata and public keys only."""
import os
import re
import subprocess
import time

from package_macos_dmg import bounded_file
from production_readiness import handoff_path


OUTPUT_LIMIT = 128 * 1024
KEYRING_NAME = 'peerbrush-archive-keyring.gpg'
FINGERPRINT = r'(?:[0-9A-F]{40}|[0-9A-F]{64})'
BAD_STATUS = {'BADSIG', 'ERRSIG', 'EXPSIG', 'EXPKEYSIG', 'REVKEYSIG', 'KEYEXPIRED',
              'SIGEXPIRED', 'FAILURE', 'ERROR', 'NODATA', 'BADARMOR', 'NO_PUBKEY'}
KEY_BITS = {'1': 2048, '19': 256, '22': 255}
# shortcut: v4 RSA/ECDSA/EdDSA signatures for the Ubuntu 24.04 target;
# review target APT/GnuPG support before expanding packet versions or algorithms.


def run(tool, arguments, work, label):
    """Use the existing native-verifier polling pattern, without inherited config."""
    environment = dict(PATH=os.defpath, HOME=str(work), USERPROFILE=str(work),
        APPDATA=str(work), LOCALAPPDATA=str(work), GNUPGHOME=str(work / 'keys'),
        TEMP=str(work), TMP=str(work), LC_ALL='C')
    for name in ['SYSTEMROOT', 'WINDIR']:
        value = next((v for k, v in os.environ.items() if k.upper() == name), None)
        if value:
            environment[name] = value
    output = work / (label + '.stdout')
    with output.open('xb') as stream:
        try:
            child = subprocess.Popen([str(tool), *arguments], cwd=work, env=environment,
                stdin=subprocess.DEVNULL, stdout=stream, stderr=subprocess.DEVNULL,
                close_fds=True, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            try:
                deadline = time.monotonic() + 30
                while True:
                    if os.fstat(stream.fileno()).st_size > OUTPUT_LIMIT or time.monotonic() >= deadline:
                        raise ValueError('APT signature verification exceeded its output or time budget.')
                    code = child.poll()
                    if code is not None:
                        if code != 0:
                            raise ValueError('APT signature verification did not succeed.')
                        break
                    time.sleep(0.05)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=10)
        except (OSError, subprocess.SubprocessError):
            raise ValueError('Native APT signature verification could not complete.') from None
    if output.stat().st_size > OUTPUT_LIMIT:
        raise ValueError('APT signature verification exceeded its output budget.')
    return output.read_bytes()


def cleartext(data, expected):
    data = data.replace(b'\r\n', b'\n')
    if (b'\r' in data or data.count(b'-----BEGIN PGP SIGNATURE-----') != 1
            or data.count(b'-----END PGP SIGNATURE-----') != 1):
        raise ValueError('InRelease must contain one cleartext signature.')
    header, separator, body = data.partition(b'\n\n')
    if (not separator or header.split(b'\n') not in [
            [b'-----BEGIN PGP SIGNED MESSAGE-----', b'Hash: ' + name]
            for name in [b'SHA256', b'SHA384', b'SHA512']]):
        raise ValueError('InRelease requires a SHA-2 cleartext signature header.')
    text, _, signature = body.partition(b'-----BEGIN PGP SIGNATURE-----')
    if text != expected or not signature.endswith(b'-----END PGP SIGNATURE-----\n'):
        raise ValueError('InRelease does not enclose the exact selected Release bytes.')


def status(data, fingerprint, start, end, now, signature_class):
    rows = [line.split()[1:] for line in data.splitlines() if line.startswith(b'[GNUPG:] ')]
    if any(row and row[0].decode('ascii', errors='replace') in BAD_STATUS for row in rows):
        raise ValueError('APT signature or signing key is invalid, expired or revoked.')
    valid = [row[1:] for row in rows if row and row[0] == b'VALIDSIG']
    if len(valid) != 1 or sum(bool(row) and row[0] == b'GOODSIG' for row in rows) != 1:
        raise ValueError('APT metadata requires exactly one successful native signature result.')
    try:
        fields = [field.decode('ascii') for field in valid[0]]
        if (len(fields) not in (9, 10) or not re.fullmatch(FINGERPRINT, fields[0])
                or fields[-1 if len(fields) == 10 else 0] != fingerprint
                or fields[4] != '4' or fields[6] not in KEY_BITS
                or fields[7] not in {'8', '9', '10'} or fields[8] != signature_class
                or not re.fullmatch('[0-9]{1,12}', fields[2])
                or not re.fullmatch('[0-9]{1,12}', fields[3])):
            raise ValueError()
        created, expires = int(fields[2]), int(fields[3])
        if not start.timestamp() <= created <= now.timestamp() or (expires and expires < end.timestamp()):
            raise ValueError()
    except (UnicodeError, ValueError):
        raise ValueError('APT signature identity, algorithm or validity interval differs.') from None
    return fields[0]


def key_lifetimes(data, primary, signers, start, end, now):
    records, pending = {}, None
    for line in data.splitlines():
        fields = line.split(b':')
        if fields[0] in (b'pub', b'sub'):
            pending = fields
        elif fields[0] == b'fpr' and pending is not None:
            if len(fields) < 10 or len(pending) < 12:
                raise ValueError('Native public-key metadata is incomplete.')
            fingerprint = fields[9].decode('ascii', errors='replace')
            if fingerprint in records or not re.fullmatch(FINGERPRINT, fingerprint):
                raise ValueError('Native public-key fingerprint metadata is ambiguous.')
            records[fingerprint] = pending
            pending = None
    if primary not in records or records[primary][0] != b'pub' or sum(row[0] == b'pub' for row in records.values()) != 1:
        raise ValueError('The approved primary public key is missing.')
    for fingerprint in {primary, *signers}:
        row = records.get(fingerprint)
        try:
            if (row is None or row[1] not in (b'o', b'-', b'q', b'm', b'f', b'u')
                    or b'D' in row[11] or (fingerprint in signers and b's' not in row[11])
                    or row[3].decode('ascii') not in KEY_BITS
                    or not row[2].isdigit() or int(row[2]) < KEY_BITS[row[3].decode('ascii')]
                    or not row[5].isdigit() or int(row[5]) > start.timestamp()
                    or (row[6] not in (b'', b'0') and (not row[6].isdigit() or int(row[6]) < end.timestamp()))):
                raise ValueError()
        except (UnicodeError, ValueError):
            raise ValueError('APT primary/signing key is unusable or expires before repository metadata.') from None
    if not start <= now < end:
        raise ValueError('Signed repository metadata is not currently valid.')


def verify(release, inrelease, detached, public_key, *, tool, tool_sha256, fingerprint, work, start, end, now):
    if not isinstance(fingerprint, str) or not re.fullmatch(FINGERPRINT, fingerprint):
        raise ValueError('Select the full approved uppercase public-key fingerprint.')
    cleartext(inrelease, release)
    if (not public_key.startswith(b'-----BEGIN PGP PUBLIC KEY BLOCK-----')
            or not public_key.rstrip().endswith(b'-----END PGP PUBLIC KEY BLOCK-----')
            or public_key.count(b'-----BEGIN PGP PUBLIC KEY BLOCK-----') != 1):
        raise ValueError('Select one ASCII-armored public certificate, never a private key or keybox.')
    tool = handoff_path(tool)
    if tool.name.lower() not in {'gpg', 'gpg2', 'gpg.exe'} or not re.fullmatch('[0-9a-f]{64}', tool_sha256 or ''):
        raise ValueError('Select the reviewed GnuPG executable and its independent SHA-256.')
    actual, size = bounded_file(tool, 64 * 1024 * 1024, executable=True)
    if not size or actual != tool_sha256:
        raise ValueError('The native GnuPG tool differs from its selected bytes.')
    if not start <= now < end:
        raise ValueError('Signed repository metadata is not currently valid.')
    (work / 'keys').mkdir(mode=0o700)
    for name, data in [('Release', release), ('InRelease', inrelease), ('Release.gpg', detached), ('public-key', public_key)]:
        with (work / name).open('xb') as stream:
            stream.write(data)
    isolated = ['--batch', '--no-tty',
        '--no-autostart', '--disable-dirmngr', '--no-auto-key-retrieve', '--no-auto-key-import',
        '--auto-key-locate', 'clear', '--no-auto-check-trustdb']
    options = ['--no-options', '--homedir', str(work / 'keys'), *isolated]
    preview = run(tool, [*options, '--with-colons', '--fixed-list-mode', '--fingerprint', '--fingerprint',
        '--show-keys', str(work / 'public-key')], work, 'inspect-public')
    if any(line.split(b':')[0] in (b'sec', b'ssb') for line in preview.splitlines()):
        raise ValueError('Private key material cannot enter APT signature verification.')
    key_lifetimes(preview, fingerprint, [], start, end, now)
    run(tool, [*options, '--import', str(work / 'public-key')], work, 'import-public')
    keyring = run(tool, [*options, '--export-options', 'export-minimal', '--export', fingerprint],
                  work, 'export-keyring')
    if not keyring:
        raise ValueError('GnuPG did not export the selected APT public keyring.')
    exported = work / KEYRING_NAME
    with exported.open('xb') as stream:
        stream.write(keyring)
    # Verify both signatures using the exact exported bytes in a separate empty home.
    client = work / 'client'
    (client / 'keys').mkdir(mode=0o700, parents=True)
    options = ['--no-options', '--homedir', str(client / 'keys'), *isolated]
    run(tool, [*options, '--import', str(exported)], client, 'import-keyring')
    listing = run(tool, [*options, '--with-colons', '--fixed-list-mode', '--fingerprint', '--fingerprint',
        '--list-keys'], client, 'public-keys')
    signers = []
    for name, arguments, kind in [('inrelease', [str(work / 'InRelease')], '01'),
                                 ('detached', [str(work / 'Release.gpg'), str(work / 'Release')], '00')]:
        data = run(tool, [*options, '--status-fd', '1', '--verify', *arguments], client, name)
        signers.append(status(data, fingerprint, start, end, now, kind))
    key_lifetimes(listing, fingerprint, signers, start, end, now)
    if bounded_file(tool, 64 * 1024 * 1024, executable=True)[0] != tool_sha256:
        raise ValueError('The native GnuPG tool changed during verification.')
    return dict(primary_fingerprint=fingerprint, signer_fingerprints=sorted(set(signers)),
        signature_hashes='sha256-sha384-sha512-only', public_key_lifetime='selected-key-valid-through-release-expiry',
        tool_sha256=tool_sha256), keyring
