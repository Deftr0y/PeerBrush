"""Exchange a private fixture between two clean hosted Macs in the same reviewed signing run."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


def main(args):
    env = os.environ
    run = env['GITHUB_RUN_ID']
    if (not re.fullmatch('[1-9][0-9]*', run) or env['GITHUB_RUN_ATTEMPT'] != '1'
            or env['GITHUB_REF'] != 'refs/heads/main' or env['GITHUB_SHA'] != env['SELECTED_COMMIT']
            or env['GITHUB_WORKFLOW_SHA'] != env['SELECTED_COMMIT'] or env['GITHUB_REPOSITORY'] != 'Deftr0y/PeerBrush'
            or env['GITHUB_EVENT_NAME'] != 'workflow_dispatch' or env['RUNNER_ENVIRONMENT'] != 'github-hosted'
            or env['GITHUB_WORKFLOW_REF'] != 'Deftr0y/PeerBrush/.github/workflows/macos-release-signing.yml@refs/heads/main'):
        raise ValueError('Requires the protected first exact main signing run.')
    os.umask(0o077)
    root = Path(env['RUNNER_TEMP'])
    openssl = '/opt/homebrew/opt/openssl@3/bin/openssl'
    action, role, *extra = args
    if role not in ('signer', 'editor'):
        raise ValueError('Unknown private role.')
    pem = root / (role + '.pem')
    key = root / (role + '.key')
    def execute(*cmd):
        subprocess.run(cmd, check=True, timeout=600, stdin=subprocess.DEVNULL)
    if action == 'recipient' and not extra:
        if key.exists() or pem.exists():
            raise ValueError('Recipient already exists.')
        execute(openssl, 'req', '-x509', '-newkey', 'rsa:3072', '-nodes', '-days', '1',
                '-subj', f'/CN=PeerBrush-{role}-{run}', '-keyout', key, '-out', pem)
        return
    # Each role receives one certificate and one authenticated AES-GCM envelope.
    cert = action == 'fetch-cert'
    name = f'sparkle-{"recipient" if cert else "delivery"}-{role}-{run}-1'
    inbox = root / name
    if action in ('fetch-cert', 'decrypt') and (not extra if cert else len(extra) == 1):
        inbox.mkdir(mode=0o700)
        deadline = time.monotonic() + 10800
        peer = role if cert else ('editor' if role == 'signer' else 'signer')
        peer_name = 'sign' if peer == 'signer' else 'Private installed Sparkle upgrade'
        while True:
            # Observe the producer before artifacts so a final upload wins over completion.
            jobs = json.loads(subprocess.check_output(
                ['gh', 'api', f'repos/Deftr0y/PeerBrush/actions/runs/{run}/jobs?filter=latest&per_page=100'],
                timeout=45))['jobs']
            producers = [job for job in jobs if job['name'] == peer_name]
            if len(producers) != 1:
                raise ValueError('Missing or ambiguous same-run producer.')
            ids = subprocess.check_output(['gh', 'api', f'repos/Deftr0y/PeerBrush/actions/runs/{run}/artifacts?name={name}',
                                          '--jq', f'.artifacts[] | select(.name == "{name}" and (.expired | not)) | .id'],
                                         timeout=45).decode().split()
            if len(ids) == 1:
                break
            if ids or time.monotonic() >= deadline:
                raise ValueError('Missing or ambiguous same-run delivery.')
            if producers[0]['status'] == 'completed':
                raise ValueError('Same-run producer finished without the required delivery; stop and reconcile.')
            time.sleep(20)
        execute('gh', 'run', 'download', run, '--repo', 'Deftr0y/PeerBrush', '--name', name, '--dir', inbox)
        file = role + ('.pem' if cert else '.cms')
        if list(inbox.iterdir()) != [inbox / file] or (inbox / file).is_symlink() or not (inbox / file).is_file():
            raise ValueError('Unexpected same-run artifact contents.')
        if cert:
            if pem.exists():
                raise ValueError('Recipient would be replaced.')
            (inbox / file).rename(pem)
        else:
            dest = Path(extra[0])
            if dest.exists():
                raise ValueError('Refusing to replace delivery.')
            execute(openssl, 'cms', '-decrypt', '-binary', '-inform', 'DER', '-in', inbox / file,
                    '-recip', pem, '-inkey', key, '-out', dest)
    elif action == 'encrypt' and len(extra) == 1:
        output = root / (role + '.cms')
        if output.exists():
            raise ValueError('Refusing to replace envelope.')
        execute(openssl, 'cms', '-encrypt', '-aes-256-gcm', '-binary', '-outform', 'DER',
                '-in', extra[0], '-out', output, '-recip', pem, '-keyopt', 'rsa_padding_mode:oaep')
    else:
        raise ValueError('Unknown fixture exchange operation.')


if __name__ == '__main__':
    main(sys.argv[1:])
