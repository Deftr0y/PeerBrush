"""Read-only cross-run uncertainty fence; receipts never authorize Store mutations."""
import hashlib
import io
import re
import stat
import struct
import time
import zipfile

import acquire_macos_qualification_inputs as transport
import store_release_state as state
import submit_store_release as store

WORKFLOW = '.github/workflows/production.yml'
WRITER = 'Publish only through actual composition and immutable reconciliation'
STEPS = ['Require exact reviewed production invocation', 'Checkout reviewed production source',
    'Require exact source', 'Acquire separate exact final-input producer data', WRITER,
    'Retain final manifest only after exact public release readback',
    'Retain sanitized Store intent and submission receipt for reconciliation',
    'Retain private Store submission input for future reconciliation']
GENERATED_STEPS = {'Set up job', 'Post Checkout reviewed production source', 'Complete job'}
MAX_ZIP = 64 * 1024
MAX_PACKAGE_ZIP = store.msix.PACKAGE_LIMIT + 1024
CONCLUSIONS = {'success', 'failure', 'neutral', 'cancelled', 'skipped', 'timed_out', 'action_required', 'stale'}


def run_identity(run):
    if (not isinstance(run, dict) or type(run.get('id')) is not int or not 0 < run['id'] < 10 ** 20
            or type(run.get('run_number')) is not int or not 0 < run['run_number'] <= 1000
            or type(run.get('run_attempt')) is not int or run['run_attempt'] != 1
            or run.get('path') not in {WORKFLOW, WORKFLOW + '@main'} or run.get('head_branch') != 'main'
            or not transport.hexadecimal(run.get('head_sha'), 40) or run.get('pull_requests') != []
            or not isinstance(run.get('head_commit'), dict) or run['head_commit'].get('id') != run['head_sha']
            or run.get('event') not in {'push', 'workflow_dispatch'}
            or run.get('status') not in {'completed', 'in_progress', 'queued', 'pending', 'waiting', 'requested'}
            or (run['status'] == 'completed' and run.get('conclusion') not in CONCLUSIONS)
            or (run['status'] != 'completed' and run.get('conclusion') is not None)):
        raise store.SafeError('Store publisher history has an unknown invocation; reconcile without retry.')
    for key in ['repository', 'head_repository']:
        repo = run.get(key)
        if (not isinstance(repo, dict) or type(repo.get('id')) is not int or repo['id'] != transport.REPOSITORY_ID
                or repo.get('full_name') != transport.REPOSITORY or repo.get('fork') is not False
                or repo.get('private') is not False or not isinstance(repo.get('owner'), dict)
                or type(repo['owner'].get('id')) is not int or repo['owner']['id'] != transport.OWNER_ID):
            raise store.SafeError('Store publisher history has a foreign source repository.')
    return {key: run.get(key) for key in ['id', 'run_number', 'run_attempt', 'head_sha', 'head_branch', 'path', 'event', 'status', 'conclusion']}


def artifact_member(raw, name, limit, archive_limit):
    if not isinstance(raw, bytes) or not 22 <= len(raw) <= archive_limit:
        raise store.SafeError('Store history artifact exceeds its bounded contract.')
    signature, disk, directory_disk, count_disk, count, size, offset, comment = struct.unpack('<4s4H2IH', raw[-22:])
    if (signature != b'PK\x05\x06' or disk or directory_disk or count_disk != 1 or count != 1
            or not 0 < size <= 512 or comment or offset + size != len(raw) - 22):
        raise store.SafeError('Store history requires one ordinary bounded artifact member.')
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        members = archive.infolist()
        if len(members) != 1:
            raise store.SafeError('Store history artifact inventory differs.')
        member = members[0]
        if (member.filename != name or member.is_dir() or member.external_attr & 0x10
                or stat.S_IFMT(member.external_attr >> 16) not in {0, stat.S_IFREG}
                or member.flag_bits & 1 or member.compress_type not in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED}
                or not 0 < member.file_size <= limit or not 0 < member.compress_size <= archive_limit):
            raise store.SafeError('Store history artifact is not its exact bounded regular member.')
        with archive.open(member) as stream:
            value = stream.read(limit + 1)
        if len(value) != member.file_size:
            raise store.SafeError('Store history artifact changed during extraction.')
    return value


def receipt_bytes(raw):
    return state.receipt(artifact_member(raw, 'submission.json', 8192, MAX_ZIP))


class History:
    def __init__(self, client, provider, environment):
        self.client, self.provider, self.environment = client, provider, environment
        self.observed = None
        self.packages = {}
        self.package_bytes = 0
        self.current = environment.get('GITHUB_RUN_ID')
        required = dict(GITHUB_REPOSITORY=transport.REPOSITORY, GITHUB_REPOSITORY_ID=str(transport.REPOSITORY_ID),
            GITHUB_REPOSITORY_OWNER_ID=str(transport.OWNER_ID), GITHUB_REF='refs/heads/main',
            GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_ATTEMPT='1', RUNNER_ENVIRONMENT='github-hosted',
            RUNNER_OS='Linux', RUNNER_ARCH='X64', GITHUB_WORKFLOW_REF=transport.REPOSITORY + '/' + WORKFLOW + '@refs/heads/main')
        if (not transport.identifier(self.current) or not transport.hexadecimal(environment.get('GITHUB_SHA'), 40)
                or environment.get('GITHUB_SHA') != environment.get('GITHUB_WORKFLOW_SHA')
                or any(environment.get(key) != value for key, value in required.items())):
            raise store.SafeError('Store history requires the exact first protected hosted publisher invocation.')

    def read(self, route):
        if time.monotonic() >= self.deadline:
            raise store.SafeError('Store history exceeded its read-only reconciliation deadline.')
        result = self.client.request('GET', '/repos/' + transport.REPOSITORY + '/actions/' + route)
        if time.monotonic() >= self.deadline or not isinstance(result, dict):
            raise store.SafeError('Store history readback is incomplete; no mutation retried.')
        return result

    def runs(self):
        result, total = [], None
        for page in range(1, 11):
            value = self.read('workflows/production.yml/runs?per_page=100&page=' + str(page))
            rows = value.get('workflow_runs')
            if (type(value.get('total_count')) is not int or not 1 <= value['total_count'] <= 1000
                    or (total is not None and value['total_count'] != total)
                    or not isinstance(rows, list) or not 1 <= len(rows) <= 100):
                raise store.SafeError('Store workflow history is missing or exceeds its complete bounded window.')
            total = value['total_count']
            result += [run_identity(row) for row in rows]
            if len(result) >= total:
                if len(result) != total: raise store.SafeError('Store workflow history count differs.')
                break
        if (len(result) != total or len({r['id'] for r in result}) != total
                or {r['run_number'] for r in result} != set(range(1, total + 1))):
            raise store.SafeError('Store workflow history has a deletion, duplicate or incomplete page; reconcile manually.')
        current = [row for row in result if str(row['id']) == self.current]
        if (len(current) != 1 or current[0]['head_sha'] != self.environment['GITHUB_SHA']
                or current[0]['event'] != 'workflow_dispatch' or current[0]['status'] != 'in_progress'
                or current[0]['conclusion'] is not None):
            raise store.SafeError('Store current publisher run differs from exact service readback.')
        number = current[0]['run_number']
        for row in result:
            if row['run_number'] > number and row['status'] not in {'queued', 'pending', 'waiting', 'requested'}:
                raise store.SafeError('Another Store publisher may be executing; no mutation retried.')
        return sorted((row for row in result if row['run_number'] < number), key=lambda row: row['run_number'])

    def writer_started(self, run):
        if run['status'] != 'completed':
            raise store.SafeError('An earlier publisher is incomplete; no Store mutation retried.')
        value = self.read('runs/' + str(run['id']) + '/attempts/1/jobs?per_page=100&page=1')
        jobs = value.get('jobs')
        if (type(value.get('total_count')) is not int or value['total_count'] != 4
                or not isinstance(jobs, list) or len(jobs) != 4 or any(not isinstance(j, dict) for j in jobs)
                or {j.get('name') for j in jobs} != {'gate', 'compose', 'publish', 'site'}
                or len({j.get('id') for j in jobs if type(j.get('id')) is int}) != 4):
            raise store.SafeError('Store publisher job history is incomplete or has an unknown policy.')
        for job in jobs:
            if (not 0 < job['id'] < 10 ** 20 or type(job.get('run_id')) is not int or job['run_id'] != run['id']
                    or job.get('head_sha') != run['head_sha'] or job.get('head_branch') != 'main'
                    or job.get('status') != 'completed' or job.get('conclusion') not in CONCLUSIONS
                    or ('run_attempt' in job and (type(job['run_attempt']) is not int or job['run_attempt'] != 1))):
                raise store.SafeError('Store historical job differs from its exact workflow source.')
        job = next(j for j in jobs if j['name'] == 'publish')
        if job['conclusion'] == 'skipped':
            if job.get('steps') not in [None, []]: raise store.SafeError('Skipped Store publisher has unexpected steps.')
            return False
        if any(j['conclusion'] != 'success' for j in jobs if j['name'] in {'gate', 'compose'}):
            raise store.SafeError('Store historical publisher lacks successful exact-source prerequisites.')
        labels, steps = job.get('labels'), job.get('steps')
        if (run['event'] != 'workflow_dispatch' or not isinstance(labels, list) or not 1 <= len(labels) <= 8
                or any(type(v) is not str for v in labels) or len(set(labels)) != len(labels)
                or 'ubuntu-24.04' not in labels or 'self-hosted' in labels
                or not isinstance(steps, list) or not 1 <= len(steps) <= 16):
            raise store.SafeError('Store historical publisher host or steps are unknown.')
        names, numbers = [], []
        for step in steps:
            if (not isinstance(step, dict) or type(step.get('number')) is not int or not 0 < step['number'] <= 1000
                    or type(step.get('name')) is not str or step['name'] not in set(STEPS) | GENERATED_STEPS
                    or step.get('status') != 'completed' or step.get('conclusion') not in
                    {'success', 'failure', 'cancelled', 'skipped', 'timed_out'}):
                raise store.SafeError('Store historical step policy is unknown; preserve the uncertainty hold.')
            names.append(step['name'])
            numbers.append(step['number'])
        if (numbers != sorted(set(numbers)) or len(set(names)) != len(names)
                or [name for name in names if name in STEPS] != STEPS):
            raise store.SafeError('Store historical publisher steps differ from the fixed order.')
        return steps[names.index(WRITER)]['conclusion'] != 'skipped'

    def selected_artifact(self, run, name, limit):
        run_id = str(run['id'])
        value = self.read('runs/' + run_id + '/artifacts?per_page=100&page=1')
        artifacts = value.get('artifacts')
        if (type(value.get('total_count')) is not int or not 1 <= value['total_count'] <= 16
                or not isinstance(artifacts, list) or len(artifacts) != value['total_count']
                or any(not isinstance(a, dict) for a in artifacts)):
            raise store.SafeError('Store prior receipt listing is missing or incomplete.')
        selected = [a for a in artifacts if a.get('name') == name]
        if len(selected) != 1: raise store.SafeError('Store prior receipt is missing or ambiguous; reconcile manually.')
        artifact = selected[0]
        identifier = artifact.get('id')
        origin = artifact.get('workflow_run')
        if (type(identifier) is not int or not 0 < identifier < 10 ** 20 or artifact.get('expired') is not False
                or type(artifact.get('size_in_bytes')) is not int or not 22 <= artifact['size_in_bytes'] <= limit
                or type(artifact.get('digest')) is not str or not re.fullmatch('sha256:[0-9a-f]{64}', artifact['digest'])
                or artifact.get('url') != 'https://api.github.com' + transport.API + 'artifacts/' + str(identifier)
                or artifact.get('archive_download_url') != artifact['url'] + '/zip'
                or not isinstance(origin, dict) or any(type(origin.get(k)) is not type(v) or origin.get(k) != v
                    for k, v in dict(id=run['id'], repository_id=transport.REPOSITORY_ID,
                        head_repository_id=transport.REPOSITORY_ID, head_branch='main', head_sha=run['head_sha']).items())):
            raise store.SafeError('Store receipt artifact origin, expiry or bounded byte identity differs.')
        return artifact

    def selected_receipt(self, run):
        artifact = self.selected_artifact(run, 'store-submission-reconciliation-' + str(run['id']) + '-1', MAX_ZIP)
        identifier = artifact['id']
        raw = self.client.receipt_bytes(identifier, artifact['size_in_bytes'])
        if (len(raw) != artifact['size_in_bytes'] or hashlib.sha256(raw).hexdigest() != artifact['digest'][7:]
                or self.read('artifacts/' + str(identifier)) != artifact):
            raise store.SafeError('Store prior receipt bytes or metadata changed.')
        receipt = receipt_bytes(raw)
        if receipt['commit'] != run['head_sha']:
            raise store.SafeError('Store receipt source differs from the historical workflow source.')
        return receipt, artifact['digest']

    def verify_package(self, run, receipt):
        artifact = self.selected_artifact(run, 'store-submission-input-' + str(run['id']) + '-1', MAX_PACKAGE_ZIP)
        if receipt['tag'] in self.packages:
            if self.packages[receipt['tag']] != artifact:
                raise store.SafeError('Store prior private package identity changed before mutation.')
            return
        self.package_bytes += artifact['size_in_bytes']
        if self.package_bytes > 2 * MAX_PACKAGE_ZIP:
            raise store.SafeError('Store history exceeds its bounded package readback window; reconcile manually.')
        raw = self.client.package_bytes(artifact['id'], artifact['size_in_bytes'])
        if (len(raw) != artifact['size_in_bytes'] or hashlib.sha256(raw).hexdigest() != artifact['digest'][7:]
                or self.read('artifacts/' + str(artifact['id'])) != artifact):
            raise store.SafeError('Store prior private package bytes or metadata changed.')
        package = artifact_member(raw, receipt['package'], store.msix.PACKAGE_LIMIT, MAX_PACKAGE_ZIP)
        if hashlib.sha256(package).hexdigest() != receipt['sha256']:
            raise store.SafeError('Store prior actual package bytes differ from its receipt.')
        self.packages[receipt['tag']] = artifact

    def check(self, pending_id=None):
        self.deadline = time.monotonic() + 180
        runs, observed, receipts = self.runs(), [], []
        for run in runs:
            if self.writer_started(run):
                receipt, digest = self.selected_receipt(run)
                receipts.append((run, receipt))
                observed.append((run, digest))
            else: observed.append((run, None))
        if self.runs() != runs or (self.observed is not None and self.observed != observed):
            raise store.SafeError('Store prior history changed before mutation; no automatic retry.')
        app = self.provider.call()
        if (app.get('id') != store.STORE_ID or app.get('packageIdentityName') != store.IDENTITY
                or app.get('publisherName') != store.PUBLISHER or app.get('packageFamilyName') != store.FAMILY):
            raise store.SafeError('Store history product identity differs.')
        pending = app.get('pendingApplicationSubmission')
        if pending and (pending_id is None or not isinstance(pending, dict) or str(pending.get('id')) != pending_id):
            raise store.SafeError('Store history has a competing pending submission; no mutation retried.')
        published = str(app.get('lastPublishedApplicationSubmission', {}).get('id', ''))
        if not re.fullmatch('[1-9][0-9]{0,31}', published): raise store.SafeError('Store history needs its actual published baseline.')
        current = store.published_version(self.provider.call('/submissions/' + published))
        identifiers = set()
        for run, receipt in receipts:
            if receipt['phase'] == 'new': continue
            sid = receipt['submission_id']
            if sid is None or sid in identifiers:
                raise store.SafeError('An earlier Store create outcome is uncertain; receipt replay cannot authorize retry.')
            identifiers.add(sid)
            release = self.client.release(receipt['tag'])
            commit = self.client.commit(receipt['tag'])
            if (not isinstance(release, dict) or release.get('draft') is not False or release.get('prerelease') is not False
                    or not release.get('published_at') or release.get('tag_name') != receipt['tag']
                    or release.get('target_commitish') != receipt['commit']
                    or not isinstance(commit, dict) or commit.get('sha') != receipt['commit']
                    or not isinstance(release.get('assets'), list) or not 1 <= len(release['assets']) <= 64):
                raise store.SafeError('Store prior attempt no longer has its exact published release source.')
            self.verify_package(run, receipt)
            submission = self.provider.call('/submissions/' + sid)
            packages = submission.get('applicationPackages')
            if (str(submission.get('id')) != sid or not isinstance(packages, list)
                    or len(packages) != 1 or packages[0].get('fileName') != receipt['package']
                    or packages[0].get('fileStatus') != 'Uploaded' or packages[0].get('architecture', '').lower() != 'x64'
                    or store.version(packages[0].get('version', '')) != store.version(receipt['version'])
                    or current < store.version(receipt['version'])
                    or self.provider.call('/submissions/' + sid + '/status').get('status') != 'Published'):
                raise store.SafeError('An earlier Store attempt is not service-reconciled published; no automatic retry.')
        if time.monotonic() >= self.deadline:
            raise store.SafeError('Store history exceeded its read-only reconciliation deadline.')
        self.observed = observed
        return dict(prior_attempts=len(receipts), mutation_authority=False, public_availability='unverified')
