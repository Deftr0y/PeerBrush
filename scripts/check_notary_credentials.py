"""Check the configured Apple Notary Service credentials without signing or submitting."""
import errno
import os
import pty
import re
import secrets
import select
import signal
import subprocess
import tempfile
import termios
import time
from pathlib import Path


def store_credentials(keychain, account, team, password):
    command = ['/usr/bin/xcrun', 'notarytool', 'store-credentials', 'peerbrush-check',
               '--keychain', str(keychain), '--apple-id', account, '--team-id', team]
    pid, terminal = pty.fork()
    if pid == 0:
        settings = termios.tcgetattr(0)
        settings[3] &= ~termios.ECHO
        termios.tcsetattr(0, termios.TCSANOW, settings)
        os.execv(command[0], command)

    deadline = time.monotonic() + 90
    transcript = bytearray()
    submitted = False
    finished = False
    try:
        while len(transcript) < 16384 and (remaining := deadline - time.monotonic()) > 0:
            if not select.select([terminal], [], [], remaining)[0]:
                break
            try:
                block = os.read(terminal, 1024)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                block = b''
            if not block:
                finished = True
                break
            transcript.extend(block)
            if not submitted and re.search(rb'app-specific password[^\r\n]*:\s*$', transcript, re.I):
                payload = password.encode('ascii') + b'\n'
                if os.write(terminal, payload) != len(payload):
                    raise RuntimeError('Incomplete input')
                submitted = True
    finally:
        os.close(terminal)
        if not finished:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        _, status = os.waitpid(pid, 0)
    if not finished:
        return 'timeout'
    if not submitted:
        return 'password prompt unavailable'
    if status == 0:
        return 'accepted'
    if re.search(rb'HTTP(?: status code)?\s*:?\s*401\b', transcript, re.I):
        return 'HTTP 401 authentication failed'
    if re.search(rb'HTTP(?: status code)?\s*:?\s*403\b', transcript, re.I):
        if re.search(rb'\b(?:agreement|contract)\b', transcript, re.I):
            return 'HTTP 403 Apple Developer agreement requires attention'
        return 'HTTP 403 account or team lacks access'
    if re.search(rb'HTTP(?: status code)?\s*:?\s*5[0-9]{2}\b', transcript, re.I):
        return 'Apple service returned an HTTP 5xx error'
    if re.search(rb'\b(errSec|SecKeychain|User interaction is not allowed)', transcript, re.I):
        return 'runner keychain error'
    return 'unclassified Apple tool error'


def main():
    account = os.environ.pop('APPLE_ID', '')
    team = os.environ.pop('APPLE_TEAM_ID', '')
    password = os.environ.pop('APPLE_NOTARY_PASSWORD', '')
    if team != os.environ.get('APPLE_TEAM') or not re.fullmatch(r'[^\s@]+@[^\s@]+', account):
        result = 'missing or malformed Apple account or team'
    elif not re.fullmatch(r'[A-Za-z0-9-]{12,64}', password):
        result = 'missing or malformed app-specific password'
    else:
        try:
            with tempfile.TemporaryDirectory(prefix='peerbrush-notary-check-') as temp:
                keychain = Path(temp) / 'check.keychain-db'
                ephemeral = secrets.token_hex(24)
                for operation in ('create-keychain', 'unlock-keychain'):
                    subprocess.run(['/usr/bin/security', operation, '-p', ephemeral, str(keychain)],
                                   check=True, capture_output=True, timeout=20)
                try:
                    result = store_credentials(keychain, account, team, password)
                finally:
                    subprocess.run(['/usr/bin/security', 'delete-keychain', str(keychain)],
                                   capture_output=True, timeout=20)
        except (OSError, RuntimeError, subprocess.SubprocessError):
            result = 'runner could not complete the credential check'
    print('Apple Notary Service credential check: ' + result + '.')
    return 0 if result == 'accepted' else 2


if __name__ == '__main__':
    raise SystemExit(main())
