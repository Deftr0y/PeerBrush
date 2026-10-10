"""Require the release workflow and checkout to refer to one exact commit."""
import argparse
import os
import pathlib
import re
import subprocess
import tomllib

parser = argparse.ArgumentParser()
parser.add_argument('--commit', required=True)
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parents[1]
if not re.fullmatch(r'[0-9a-f]{40}', args.commit):
    raise SystemExit('Expected a full source commit SHA')
actual = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
if actual != args.commit or os.environ.get('GITHUB_SHA', actual) != args.commit:
    raise SystemExit('Workflow and source checkout must match the selected commit')
version = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
lock = tomllib.loads((root / 'Cargo.lock').read_text(encoding='utf-8'))
if not any(p['name'] == 'peerbrush' and p['version'] == version for p in lock['package']):
    raise SystemExit('Cargo manifest and lockfile versions disagree')
print(f'Preparing PeerBrush {version} from {actual}')
