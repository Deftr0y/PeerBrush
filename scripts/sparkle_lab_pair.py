"""Select only the two fixed local pairs for the protected private upgrade exercise."""
import io
import os
from pathlib import Path
import sys
from types import SimpleNamespace

from package import digest, git
from prepare_sparkle_feed import prepare
from verify_release import inspect_archive


def main(action):
    root = Path(__file__).resolve().parents[1]
    temp = Path(os.environ['RUNNER_TEMP']).resolve()
    parent = os.environ['SELECTED_COMMIT']
    commit = git(root, 'rev-parse', 'HEAD').decode().strip()
    baseline = root == temp / 'peerbrush-sparkle-baseline'
    if (os.environ.get('GITHUB_WORKFLOW_SHA') != parent or os.environ.get('GITHUB_SHA') != parent
            or (git(root, 'rev-parse', 'HEAD^').decode().strip() if baseline else commit) != parent):
        raise ValueError('Wrong reviewed source for private exercise.')
    version = '0.29.99' if baseline else '0.30.0'
    if action == 'unsigned' and baseline:
        archive = temp / 'PeerBrush-0.29.99-macOS-arm64.zip'
        checked = inspect_archive(archive, commit, version, root)
        if (checked['record']['native_publisher_signing'] != 'not-configured' or checked['packaging_correction']
                or checked['record']['binary_source_commit'] != commit):
            raise ValueError('Unexpected unsigned baseline.')
        return
    if action != 'feed':
        raise ValueError('Unexpected private fixture operation.')
    with os.fdopen(os.dup(0), 'rb') as stream, open(os.devnull, 'rb') as empty:
        os.dup2(empty.fileno(), 0)
        key = stream.read(48)
    folder = temp / ('baseline-signed' if baseline else 'macos-signed')
    name = f'PeerBrush-{version}-macOS-arm64-apple-signed-r' + ('1' if baseline else os.environ['ZIP_REVISION'])
    archive = folder / (name + '.zip')
    image = folder / (name + '-dmg-signed-r' + ('1' if baseline else os.environ['DMG_REVISION']) + '.dmg')
    notes = temp / ('private-notes-' + version)
    notes.write_text('Private installed update exercise.', encoding='utf-8')
    args = SimpleNamespace(version=version, previous_version='0.29.98' if baseline else '0.29.99',
                           commit=commit, archive=archive, image=image, archive_sha256=digest(archive),
                           image_sha256=digest(image), work=temp / ('lab-inspect-' + version),
                           output=temp / ('lab-feed-' + version), signer=temp / 'sign_update', notes=notes)
    prepare(args, io.BytesIO(key), private_loopback=True)


if __name__ == '__main__':
    main(sys.argv[1])
