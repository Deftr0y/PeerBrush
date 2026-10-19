"""Private, resumable Store receipts; read-only reconciliation never retries mutations."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import time

from submit_store_release import (BASELINE, FAMILY, IDENTITY, PUBLISHER, STORE_ID,
                                 RemoteError, SafeError, published_version, release_version, version)

STATUSES = {
    'None': 'draft', 'Canceled': 'canceled', 'PendingCommit': 'commit-pending',
    'CommitStarted': 'commit-pending', 'CommitFailed': 'failed',
    'PreProcessing': 'processing', 'PreProcessingFailed': 'failed',
    'Certification': 'certification-pending', 'CertificationFailed': 'rejected',
    'PendingPublication': 'publication-pending', 'Publishing': 'publishing',
    'Published': 'published', 'PublishFailed': 'failed',
    'Release': 'release-processing', 'ReleaseFailed': 'failed',
}


def regular(path):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
        raise SafeError('Store state and lock must be regular files without links or reparse points.')


@contextmanager
def locked(path):
    """OS lock releases on process failure; an interrupted receipt remains resumable."""
    path = Path(path)
    if not path.is_absolute() or not path.parent.is_dir():
        raise SafeError('Choose an absolute state file in an existing private directory.')
    lock = path.with_name(path.name + '.lock')
    if lock.exists() or lock.is_symlink():
        regular(lock)
    fd = os.open(lock, os.O_RDWR | os.O_CREAT | getattr(os, 'O_NOFOLLOW', 0), 0o600)
    with os.fdopen(fd, 'r+b') as stream:
        regular(lock)
        try:
            if os.name == 'nt':
                import msvcrt
                if not stream.read(1):
                    stream.write(b'0')
                    stream.flush()
                stream.seek(0)
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise SafeError('Store state is already in use; no operation retried.') from None
        try:
            yield Journal(path)
        finally:
            if os.name == 'nt':
                stream.seek(0)
                msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(stream, fcntl.LOCK_UN)


class Journal:
    def __init__(self, path):
        self.path = Path(path)
        self.state = None
        if self.path.exists() or self.path.is_symlink():
            regular(self.path)
            if self.path.stat().st_size > 8192:
                raise SafeError('Store state exceeds its size limit.')
            self.state = json.loads(self.path.read_text(encoding='utf-8'))
            fields = {'schema', 'tag', 'commit', 'package', 'sha256', 'version',
                      'phase', 'submission_id', 'service_status'}
            s = self.state
            if (not isinstance(s, dict) or set(s) != fields or type(s['schema']) is not int or s['schema'] != 1
                    or not re.fullmatch(r'v?[0-9]+\.[0-9]+(?:\.[0-9]+)?', str(s['tag']))
                    or not re.fullmatch('[0-9a-f]{40}', str(s['commit']))
                    or not re.fullmatch('[0-9a-f]{64}', str(s['sha256']))
                    or not re.fullmatch(r'PeerBrush-[0-9.]+-Windows-x64\.msix', str(s['package']))
                    or s['phase'] not in {'new', 'create-requested', 'created', 'commit-requested', 'committed'}
                    or (s['submission_id'] is not None and not re.fullmatch('[0-9]+', str(s['submission_id'])))
                    or s['service_status'] not in {None, *STATUSES}):
                raise SafeError('Unsupported Store state; preserve it for manual recovery.')
            version(s['version'])
            if release_version(s['tag'])[1] != s['version'] or s['package'] != f"PeerBrush-{s['version']}-Windows-x64.msix":
                raise SafeError('Store receipt tag, package and deployment version disagree.')
            if s['phase'] in {'created', 'commit-requested', 'committed'} and s['submission_id'] is None:
                raise SafeError('Incomplete Store state; preserve it for manual recovery.')
            if s['phase'] in {'new', 'create-requested'} and s['submission_id'] is not None:
                raise SafeError('Inconsistent Store creation state; preserve it for manual recovery.')

    def bind(self, tag, name, report):
        expected = dict(schema=1, tag=tag, commit=report['commit'], package=name,
                        sha256=report['sha256'], version=report['version'])
        if self.state is None:
            self.state = dict(expected, phase='new', submission_id=None, service_status=None)
            self.write()
        elif any(self.state[key] != value for key, value in expected.items()):
            raise SafeError('Store state is bound to different immutable release bytes.')

    def write(self):
        name = None
        try:
            with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=self.path.parent,
                                             prefix='.store-state-', delete=False) as stream:
                name = stream.name
                json.dump(self.state, stream, sort_keys=True)
                stream.write('\n')
                stream.flush()
                os.fsync(stream.fileno())
            if os.name == 'nt':
                import ctypes
                from ctypes import wintypes
                move = ctypes.WinDLL('kernel32', use_last_error=True).MoveFileExW
                move.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD]
                move.restype = wintypes.BOOL
                if not move(name, str(self.path), 0x1 | 0x8):  # ReplaceExisting | WriteThrough
                    raise SafeError('Could not durably replace the private Store receipt; no mutation retried.')
            else:
                os.replace(name, self.path)
            if os.name != 'nt':
                fd = os.open(self.path.parent, os.O_RDONLY)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
        finally:
            if name is not None:
                Path(name).unlink(missing_ok=True)

    def phase(self, phase, submission_id=None):
        if phase == 'create-requested' and self.state['phase'] != 'new':
            raise SafeError('A Store create was already attempted; reconcile its outcome instead of resubmitting.')
        self.state['phase'] = phase
        if submission_id is not None:
            self.state['submission_id'] = submission_id
        self.write()

    def observe(self, status):
        if status not in STATUSES:
            raise SafeError('Unknown Store service status; availability remains unverified.')
        self.state['service_status'] = status
        self.write()


def get(store, path='', attempts=3, sleep=time.sleep):
    if not 1 <= attempts <= 3:
        raise SafeError('Store observation permits one to three read attempts.')
    for attempt in range(attempts):
        try:
            return store.call(path)
        except RemoteError as error:
            if error.status not in {None, 408, 429, 500, 502, 503, 504} or attempt + 1 == attempts:
                raise
            sleep(2 ** attempt)


def reconcile(store, journal, attempts=3, sleep=time.sleep):
    """Only service GETs. Published is never proof of public package availability."""
    if journal.state is None:
        raise SafeError('Choose an existing release-bound Store state file.')
    read = lambda path='': get(store, path, attempts, sleep)
    app = read()
    if (app.get('id') != STORE_ID or app.get('packageIdentityName') != IDENTITY
            or app.get('publisherName') != PUBLISHER or app.get('packageFamilyName') != FAMILY):
        raise SafeError('Partner Center product identity mismatch.')
    s = journal.state
    result = dict(schema=1, channel='microsoft-store', version=s['version'],
                  commit=s['commit'], package_sha256=s['sha256'],
                  submission_id=s['submission_id'], public_availability='unverified',
                  can_submit=False, service_state='not-started')
    sid = s['submission_id']
    if sid is not None:
        submission = read('/submissions/' + sid)
        if str(submission.get('id')) != sid:
            raise SafeError('Store submission identity differs from its receipt.')
        matches = [p for p in submission.get('applicationPackages', []) if p.get('fileName') == s['package']]
        if (len(matches) != 1 or matches[0].get('fileStatus') not in {'PendingUpload', 'Uploaded'}
                or (matches[0].get('version') is not None and version(matches[0]['version']) != version(s['version']))
                or matches[0].get('architecture', 'x64').lower() != 'x64'):
            raise SafeError('Store submission packages do not match the release receipt.')
        status = read('/submissions/' + sid + '/status').get('status')
        journal.observe(status)
        result['service_state'] = STATUSES[status]
        result['service_status'] = status
        return result
    if s['phase'] != 'new':
        # A disconnected create can succeed remotely without returning its ID.
        result['service_state'] = 'create-outcome-uncertain'
        return result
    if app.get('pendingApplicationSubmission'):
        result['service_state'] = 'another-submission-pending'
        return result
    published = str(app.get('lastPublishedApplicationSubmission', {}).get('id', ''))
    if not re.fullmatch('[0-9]+', published):
        raise SafeError('A published baseline submission is required.')
    current = published_version(read('/submissions/' + published))
    if version(s['version']) <= max(BASELINE, current):
        result['service_state'] = 'version-already-present-or-superseded'
    else:
        result['can_submit'] = True
    return result


def main():
    import argparse
    import sys
    from submit_store_release import Store
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state', type=Path, required=True)
    args = parser.parse_args()
    try:
        with locked(args.state) as journal:
            if journal.state is None:
                raise SafeError('Choose an existing release-bound Store state file.')
            result = reconcile(Store(), journal)
        print(json.dumps(result, sort_keys=True))
        return 0
    except Exception as error:
        print('Store reconciliation stopped: ' + (str(error) if isinstance(error, SafeError) else type(error).__name__), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
