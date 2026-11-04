"""Derive the single approved Mac upgrade starting build in a separate local checkout."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
PATCH = 'scripts/sparkle-baseline.patch'
SHA256 = '870c21e49b18c947133a5aca058da25ff2578bbc0519b118c9cc52acae2f4248'
FILES = {'Cargo.toml', 'Cargo.lock', 'src/sparkle/discovery.rs', 'src/sparkle/macos.rs', 'scripts/package.py'}


def git(root, *args, data=None, env=None):
    return subprocess.check_output(['git', '-C', root, *args], input=data, env=env)


def derive(root, parent, dest):
    if (not re.fullmatch('[0-9a-f]{40}', parent) or git(root, 'rev-parse', 'HEAD').decode().strip() != parent
            or dest.exists() or dest.is_symlink() or dest.resolve().is_relative_to(root.resolve())):
        raise ValueError('Expected the exact reviewed parent and a fresh external checkout.')
    patch = git(root, 'show', f'{parent}:{PATCH}')
    if hashlib.sha256(patch).hexdigest() != SHA256:
        raise ValueError('Starting-build patch differs from the reviewed selection.')
    dest.mkdir(mode=0o700)
    git(root, 'clone', '--no-checkout', '--no-hardlinks', '--quiet', str(root), str(dest))
    git(dest, 'remote', 'remove', 'origin')
    git(dest, 'config', 'core.autocrlf', 'false')
    git(dest, 'checkout', '--quiet', '--detach', parent)
    git(dest, 'apply', '--index', '--whitespace=error', '-', data=patch)
    files = git(dest, 'diff', '--cached', '--name-only', '-z').decode().split('\0')[:-1]
    if set(files) != FILES or len(files) != len(FILES):
        raise ValueError('Unexpected change in test source.')
    def read(path):
        return tomllib.loads(git(dest, 'show', ':' + path).decode())
    if (read('Cargo.toml')['package']['version'] != '0.29.99' or
            [p['version'] for p in read('Cargo.lock')['package'] if p['name'] == 'peerbrush'] != ['0.29.99']):
        raise ValueError('Unexpected starting build version.')
    humans = json.loads(git(root, 'show', parent + ':.github/human-contributors.json'))['humans']
    name, email = git(root, 'show', '-s', '--format=%an%x00%ae', parent).decode().strip().split('\0')
    if hashlib.sha256(f'{name} <{email}>'.lower().encode()).hexdigest() not in {h for group in humans.values() for h in group}:
        raise ValueError('The selected main source must have a verified human author.')
    date = git(root, 'show', '-s', '--format=%cI', parent).decode().strip()
    identity = dict(os.environ)
    for role in ['AUTHOR', 'COMMITTER']:
        for key, value in [('NAME', name), ('EMAIL', email), ('DATE', date)]:
            identity[f'GIT_{role}_{key}'] = value
    tree = git(dest, 'write-tree').decode().strip()
    commit = git(dest, 'commit-tree', tree, '-p', parent, '-m', 'Private Sparkle upgrade starting fixture 0.29.99',
                 env=identity).decode().strip()
    git(dest, 'checkout', '--quiet', '--detach', commit)
    if git(dest, 'status', '--porcelain') or git(dest, 'remote'):
        raise ValueError('Expected a clean isolated source without a push destination.')
    return commit


if __name__ == '__main__':
    print(derive(ROOT, sys.argv[1], Path(os.environ['RUNNER_TEMP']).resolve() / 'peerbrush-sparkle-baseline'))
