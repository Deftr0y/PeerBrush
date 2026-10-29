"""Authenticate a native qualification's origin using an explicitly reviewed offline verifier.

No root, CLI, workflow permission or producer is configured by this consumer.
"""
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time

from package_macos_dmg import bounded_file, private_inspection_directory
from production_readiness import handoff_path, unique_fields

REPOSITORY = 'Deftr0y/PeerBrush'
REPOSITORY_URI = 'https://github.com/' + REPOSITORY
REPOSITORY_ID = '1407648785'
OWNER_ID = '21202494'
ISSUER = 'https://token.actions.githubusercontent.com'
WORKFLOW = '.github/workflows/macos-qualification.yml'
IDENTITY = REPOSITORY_URI + '/' + WORKFLOW + '@refs/heads/main'
CLI_VERSION = '2.102.0'
PREDICATE = 'https://slsa.dev/provenance/v1'
RESULT_MEDIA_TYPE = 'application/vnd.dev.sigstore.verificationresult+json;version=0.1'
LIMITS = {'qualification': 8192, 'bundle': 2 * 1024 * 1024,
          'trusted-root': 1024 * 1024, 'gh': 128 * 1024 * 1024}
OUTPUT_LIMIT = 4 * 1024 * 1024
API_LIMIT = 256 * 1024
REQUIRED_JOB_STEPS = (
    'Require reviewed main invocation',
    'Checkout reviewed main source',
    'Require exact main source',
    'Acquire selected signed assets',
    'Qualify selected final DMG',
    'Attest generated native qualification',
    'Retain qualification and bundle',
    'Upload qualification evidence',
)


def _policy(windows, linux=False):
    if type(windows) is not bool or type(linux) is not bool or (windows and linux):
        raise ValueError('Native platform policy must be selected by its fixed adapter.')
    if linux:
        workflow = '.github/workflows/linux-qualification.yml'
        return (workflow, REPOSITORY_URI + '/' + workflow + '@refs/heads/main',
                'linux-deb-qualification.json', 'PeerBrushLinuxDebQualificationV1',
                'PeerBrushLinuxNativeOriginV1', 'ubuntu-24.04', 'Qualify selected final Linux DEB', 16 * 1024)
    if windows:
        workflow = '.github/workflows/windows-qualification.yml'
        return (workflow, REPOSITORY_URI + '/' + workflow + '@refs/heads/main',
                'windows-installer-qualification.json', 'PeerBrushWindowsInstallerQualificationV1',
                'PeerBrushWindowsNativeOriginV1', 'windows-2025', 'Qualify selected final Windows installer', 16 * 1024)
    return (WORKFLOW, IDENTITY, 'dmg-qualification.json', 'PeerBrushDmgQualificationV1',
            'PeerBrushMacosNativeOriginV1', 'macos-26', 'Qualify selected final DMG', LIMITS['qualification'])


def _command(executable, arguments, work, label, *, _windows_host=False):
    """Only fixed built-in CLI commands; capture private output with a size/deadline guard."""
    output, errors = work / (label + '.stdout'), work / (label + '.stderr')
    environment = dict(PATH=os.defpath, HOME=str(work / 'home'), XDG_CACHE_HOME=str(work / 'cache'),
                       GH_CONFIG_DIR=str(work / 'config'), GH_HOST='github.com',
                       GH_PROMPT_DISABLED='1', GH_NO_UPDATE_NOTIFIER='1',
                       GH_NO_EXTENSION_UPDATE_NOTIFIER='1', NO_COLOR='1', LANG='C.UTF-8', TZ='UTC')
    if _windows_host:
        environment.update(USERPROFILE=str(work / 'home'), APPDATA=str(work / 'config'),
                           LOCALAPPDATA=str(work / 'cache'), TEMP=str(work / 'cache'), TMP=str(work / 'cache'))
        for name in ['SYSTEMROOT', 'WINDIR']:
            value = next((value for key, value in os.environ.items() if key.upper() == name), None)
            if value: environment[name] = value
    with output.open('xb') as stdout, errors.open('xb') as stderr:
        try:
            child = subprocess.Popen([str(executable), *arguments], cwd=work, env=environment,
                                     stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, close_fds=True,
                                     creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0) if _windows_host else 0)
            try:
                deadline = time.monotonic() + 60
                while True:
                    if (os.fstat(stdout.fileno()).st_size > OUTPUT_LIMIT
                            or os.fstat(stderr.fileno()).st_size > OUTPUT_LIMIT
                            or time.monotonic() >= deadline):
                        raise ValueError('Native attestation verifier exceeded its output or time budget.')
                    code = child.poll()
                    if code is not None:
                        if code != 0:
                            raise ValueError('Native attestation cryptographic verification failed.')
                        break
                    time.sleep(0.05)
            finally:
                # An interruption must not enter Popen's unbounded context-manager wait.
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=10)
        except (OSError, subprocess.SubprocessError):
            raise ValueError('Native attestation verifier could not complete.') from None
    if output.stat().st_size > OUTPUT_LIMIT or errors.stat().st_size > OUTPUT_LIMIT:
        raise ValueError('Native attestation verifier exceeded its output budget.')
    return output.read_bytes()


def _verified_claims(data, *, commit, qualification_sha256, run_id, run_attempt, _windows=False, _linux=False):
    """Only inspect output from the successful, pinned cryptographic verifier process."""
    workflow, identity, filename, *_ = _policy(_windows, _linux)
    if not isinstance(data, bytes) or not 0 < len(data) <= OUTPUT_LIMIT:
        raise ValueError('Native attestation verifier returned unsupported output.')
    try:
        values = json.loads(data, object_pairs_hook=unique_fields)
        if not isinstance(values, list) or len(values) != 1 or not isinstance(values[0], dict):
            raise ValueError('shape')
        result = values[0]['verificationResult']
        if (not isinstance(result, dict) or result.get('mediaType') != RESULT_MEDIA_TYPE
                or set(result) - {'mediaType', 'statement', 'signature', 'verifiedTimestamps', 'verifiedIdentity'}):
            raise ValueError('result')
        signature = result['signature']
        if not isinstance(signature, dict) or set(signature) != {'certificate'}:
            raise ValueError('signature')
        certificate = signature['certificate']
        expected = dict(subjectAlternativeName=identity, issuer=ISSUER,
                        buildSignerURI=identity, buildSignerDigest=commit,
                        runnerEnvironment='github-hosted', sourceRepositoryURI=REPOSITORY_URI,
                        sourceRepositoryDigest=commit, sourceRepositoryRef='refs/heads/main',
                        sourceRepositoryIdentifier=REPOSITORY_ID,
                        sourceRepositoryOwnerURI='https://github.com/Deftr0y',
                        sourceRepositoryOwnerIdentifier=OWNER_ID,
                        buildConfigURI=identity, buildConfigDigest=commit,
                        runInvocationURI=REPOSITORY_URI + '/actions/runs/' + run_id + '/attempts/' + run_attempt,
                        sourceRepositoryVisibilityAtSigning='public')
        allowed = set(expected) | {'certificateIssuer', 'buildTrigger', 'githubWorkflowTrigger',
                                   'githubWorkflowSHA', 'githubWorkflowName',
                                   'githubWorkflowRepository', 'githubWorkflowRef'}
        if (not isinstance(certificate, dict) or set(certificate) - allowed
                or any(type(value) is not str or len(value) > 2048 for value in certificate.values())
                or any(certificate.get(key) != value for key, value in expected.items())
                or certificate.get('buildTrigger') not in {'push', 'workflow_dispatch'}
                or not certificate.get('certificateIssuer')):
            raise ValueError('certificate')
        for key, expected_value in dict(githubWorkflowTrigger=certificate['buildTrigger'],
                                       githubWorkflowSHA=commit, githubWorkflowRepository=REPOSITORY,
                                       githubWorkflowRef='refs/heads/main').items():
            if key in certificate and certificate[key] != expected_value:
                raise ValueError('legacy extension')
        timestamps = result['verifiedTimestamps']
        if not isinstance(timestamps, list) or not 1 <= len(timestamps) <= 16:
            raise ValueError('timestamps')
        for stamp in timestamps:
            if (not isinstance(stamp, dict) or set(stamp) != {'type', 'uri', 'timestamp'}
                    or stamp['type'] not in {'Tlog', 'TimestampAuthority'}
                    or type(stamp['uri']) is not str or not 0 < len(stamp['uri']) <= 2048
                    or type(stamp['timestamp']) is not str or len(stamp['timestamp']) > 40):
                raise ValueError('timestamp')
            if datetime.datetime.fromisoformat(stamp['timestamp']).tzinfo is None:
                raise ValueError('timestamp zone')
        statement = result['statement']
        if (not isinstance(statement, dict) or set(statement) - {'_type', 'subject', 'predicateType', 'predicate'}
                or statement.get('_type') != 'https://in-toto.io/Statement/v1'
                or statement.get('predicateType') != PREDICATE
                or statement.get('subject') != [{'name': filename,
                                               'digest': {'sha256': qualification_sha256}}]):
            raise ValueError('subject')
        # Predicate fields are workflow-controlled; no OS, trigger, repository
        # identity, success or signature authority is obtained from them.
    except (KeyError, TypeError, ValueError, UnicodeError, RecursionError):
        raise ValueError('Verified native attestation does not match the exact reviewed origin and report.') from None
    return dict(repository_id=REPOSITORY_ID, owner_id=OWNER_ID, workflow=workflow,
                workflow_commit=commit, source_ref='refs/heads/main', runner='github-hosted',
                trigger=certificate['buildTrigger'], run_id=run_id, run_attempt=run_attempt,
                certificate_and_observer_time='cryptographically-verified')

def _reconcile_job(claims, *, _windows=False, _linux=False):
    """Only called with this invocation's cryptographically verified claims."""
    from prepare_site_release import public_get
    workflow, _, _, _, _, runner, qualify_step, _ = _policy(_windows, _linux)
    required_steps = tuple('Acquire selected Linux assets' if _linux and name == 'Acquire selected signed assets'
                           else qualify_step if name == 'Qualify selected final DMG' else name for name in REQUIRED_JOB_STEPS)

    try:
        # This producer is deliberately dispatch-only and refuses reruns.
        if claims['trigger'] != 'workflow_dispatch' or claims['run_attempt'] != '1':
            raise ValueError('producer')
        base = ('https://api.github.com/repos/' + REPOSITORY + '/actions/runs/'
                + claims['run_id'] + '/attempts/' + claims['run_attempt'])

        def response(suffix):
            data = public_get(base + suffix, API_LIMIT, api=True)
            if not isinstance(data, bytes) or not 0 < len(data) <= API_LIMIT:
                raise ValueError('budget')
            value = json.loads(data, object_pairs_hook=unique_fields)
            if not isinstance(value, dict):
                raise ValueError('shape')
            return value

        run = response('')
        if (type(run.get('id')) is not int or run['id'] != int(claims['run_id'])
                or type(run.get('run_attempt')) is not int or run['run_attempt'] != int(claims['run_attempt'])
                or run.get('head_sha') != claims['workflow_commit'] or run.get('head_branch') != 'main'
                # GitHub documents the @main suffix and also returns bare paths.
                or run.get('path') not in {workflow, workflow + '@main'}
                or run.get('event') != claims['trigger']
                or run.get('status') != 'completed' or run.get('conclusion') != 'success'
                or run.get('pull_requests') != [] or not isinstance(run.get('head_commit'), dict)
                or run['head_commit'].get('id') != claims['workflow_commit']):
            raise ValueError('run')
        for field in ['repository', 'head_repository']:
            repository = run.get(field)
            if (not isinstance(repository, dict) or type(repository.get('id')) is not int
                    or repository['id'] != int(REPOSITORY_ID) or repository.get('full_name') != REPOSITORY
                    or repository.get('private') is not False or repository.get('fork') is not False
                    or not isinstance(repository.get('owner'), dict)
                    or type(repository['owner'].get('id')) is not int
                    or repository['owner']['id'] != int(OWNER_ID)):
                raise ValueError('repository')
        listing = response('/jobs')
        jobs = listing.get('jobs')
        if (type(listing.get('total_count')) is not int or listing['total_count'] != 2
                or not isinstance(jobs, list) or len(jobs) != 2
                or any(not isinstance(job, dict) for job in jobs)
                or {job.get('name') for job in jobs} != {'gate', 'qualify'}):
            raise ValueError('jobs')
        identifiers = []
        for job in jobs:
            labels = job.get('labels')
            if (type(job.get('id')) is not int or not 0 < job['id'] < 10 ** 20
                    or type(job.get('run_id')) is not int or job['run_id'] != run['id']
                    or ('run_attempt' in job and (type(job['run_attempt']) is not int
                                                 or job['run_attempt'] != run['run_attempt']))
                    or job.get('head_sha') != claims['workflow_commit'] or job.get('head_branch') != 'main'
                    or job.get('status') != 'completed' or job.get('conclusion') != 'success'
                    or not isinstance(labels, list) or not 1 <= len(labels) <= 8
                    or any(type(label) is not str or not 0 < len(label) <= 64 for label in labels)
                    or len(set(labels)) != len(labels) or runner not in labels or 'self-hosted' in labels):
                raise ValueError('job')
            identifiers.append(job['id'])
        if len(set(identifiers)) != 2:
            raise ValueError('job identity')
        qualifier = next(job for job in jobs if job['name'] == 'qualify')
        steps = qualifier.get('steps')
        if not isinstance(steps, list) or not 1 <= len(steps) <= 64:
            raise ValueError('steps')
        numbers, names = [], []
        for step in steps:
            if (not isinstance(step, dict) or type(step.get('number')) is not int
                    or not 0 < step['number'] <= 1000 or type(step.get('name')) is not str
                    or not 0 < len(step['name']) <= 256):
                raise ValueError('step')
            numbers.append(step['number'])
            names.append(step['name'])
        if numbers != sorted(set(numbers)) or len(set(names)) != len(names):
            raise ValueError('step order')
        selected = [steps[names.index(name)] for name in required_steps]
        if ([step['number'] for step in selected] != sorted(step['number'] for step in selected)
                or any(step.get('status') != 'completed' or step.get('conclusion') != 'success' for step in selected)
                or steps[names.index('Attest generated native qualification')]['number']
                != steps[names.index(qualify_step)]['number'] + 1):
            raise ValueError('required steps')
    except Exception:
        raise ValueError('Authenticated native qualification does not match a successful exact-attempt service readback.') from None
    return dict(api_host='api.github.com', run_id=claims['run_id'], run_attempt=claims['run_attempt'],
                job_id=str(qualifier['id']), job_name='qualify', conclusion='success',
                workflow=workflow, workflow_commit=claims['workflow_commit'], runner_label=runner,
                required_steps=list(required_steps))


def authenticate(qualification, *, qualification_sha256, commit, version, bundle, bundle_sha256,
                 trusted_root, trusted_root_sha256, cli, cli_sha256, cli_version,
                 workflow_commit, run_id, run_attempt, work, source_root, readback_job=False, _windows=False, _linux=False, _setup_host=False):
    """Explicit reviewed public trust inputs only; never fetch roots/bundles or use auth."""
    _, identity, filename, report_format, origin_format, _, _, qualification_limit = _policy(_windows, _linux)
    selected = [qualification_sha256, bundle_sha256, trusted_root_sha256, cli_sha256]
    if (not isinstance(qualification, bytes) or not 0 < len(qualification) <= qualification_limit
            or any(type(value) is not str or not re.fullmatch('[0-9a-f]{64}', value) for value in selected)
            or hashlib.sha256(qualification).hexdigest() != qualification_sha256
            or type(commit) is not str or not re.fullmatch('[0-9a-f]{40}', commit)
            or workflow_commit != commit or cli_version != CLI_VERSION
            or type(version) is not str or len(version) > 32
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version)
            or type(run_id) is not str or not re.fullmatch('[1-9][0-9]{0,19}', run_id)
            or type(run_attempt) is not str or not re.fullmatch('[1-9][0-9]{0,2}', run_attempt)
            or type(readback_job) is not bool):
        raise ValueError('Native attestation requires independently reviewed exact source, verifier, trust and run selections.')
    if type(_setup_host) is not bool or (_setup_host and (not _windows or _linux)):
        raise ValueError('Setup host is available only through the fixed Windows adapter.')
    windows_host = _setup_host and sys.platform == 'win32' and platform.machine().lower() in {'x86_64', 'amd64'}
    if not windows_host and (sys.platform != 'linux' or platform.machine().lower() not in {'x86_64', 'amd64'}):
        raise ValueError('The prepared native attestation consumer requires Linux x64 aggregate execution.')
    # Confirm the exact artifact being authenticated belongs to the already
    # selected version. The aggregate independently enforces its complete schema.
    try:
        report = json.loads(qualification, object_pairs_hook=unique_fields)
        if (not isinstance(report, dict)
                or not (report.get('format') == report_format
                    or (_linux and report.get('format') == 'PeerBrushLinuxDebQualificationV2'
                        and type(report.get('schema')) is int and report['schema'] == 2))
                or report.get('commit') != commit or report.get('version') != version):
            raise ValueError('report')
    except (ValueError, UnicodeError, RecursionError):
        raise ValueError('Native attestation artifact differs from the selected qualification.') from None
    bundle, trusted_root, cli = map(handoff_path, [bundle, trusted_root, cli])
    if windows_host and cli.name.casefold() != 'gh.exe':
        raise ValueError('Windows setup origin needs the independently pinned native gh.exe.')
    work = private_inspection_directory(work, source_root, fresh=True)
    if any(work.is_relative_to(path.parent) for path in [bundle, trusted_root, cli]):
        raise ValueError('Native attestation work must be separate from input directories.')
    work.mkdir(mode=0o700)
    os.chmod(work, 0o700)
    private_inspection_directory(work, source_root, fresh=False)
    copies = [(bundle, work / 'bundle.jsonl', bundle_sha256, LIMITS['bundle']),
              (trusted_root, work / 'trusted_root.jsonl', trusted_root_sha256, LIMITS['trusted-root']),
              (cli, work / ('gh.exe' if windows_host else 'gh'), cli_sha256, LIMITS['gh'])]
    for source, destination, expected, limit in copies:
        actual, size = bounded_file(source, limit, destination, executable=windows_host and source == cli)
        if actual != expected or size == 0:
            raise ValueError('Native attestation input differs from its independently reviewed SHA256.')
        destination.chmod(0o500 if destination.name in ['gh', 'gh.exe'] else 0o400)
    if windows_host:
        from pe_signing import layout
        layout((work / 'gh.exe').read_bytes())  # exact pinned x64 PE, never a shell script
    # The pinned CLI consumes a newline-delimited trusted root file. No network
    # fallback or implicit TUF trust is allowed by this consumer.
    if not (work / 'trusted_root.jsonl').read_bytes().endswith(b'\n'):
        raise ValueError('Reviewed offline trusted-root JSONL requires its final newline.')
    artifact = work / filename
    with artifact.open('xb') as stream:
        stream.write(qualification)
    artifact.chmod(0o400)
    for name in ['home', 'cache', 'config']:
        (work / name).mkdir(mode=0o700)
    executable = work / ('gh.exe' if windows_host else 'gh')
    command_options = {'_windows_host': True} if windows_host else {}
    version_output = _command(executable, ['--version'], work, 'version', **command_options)
    if not re.fullmatch(rb'gh version 2\.102\.0 \([0-9]{4}-[0-9]{2}-[0-9]{2}\)\r?\nhttps://github.com/cli/cli/releases/tag/v2\.102\.0\r?\n?', version_output):
        raise ValueError('Native attestation requires the reviewed GitHub CLI 2.102.0 binary.')
    arguments = ['attestation', 'verify', str(artifact), '--bundle', str(work / 'bundle.jsonl'),
                 '--custom-trusted-root', str(work / 'trusted_root.jsonl'), '--repo', REPOSITORY,
                 '--hostname', 'github.com', '--cert-identity', identity, '--cert-oidc-issuer', ISSUER,
                 '--source-ref', 'refs/heads/main', '--source-digest', commit, '--signer-digest', workflow_commit,
                 '--deny-self-hosted-runners', '--predicate-type', PREDICATE, '--digest-alg', 'sha256',
                 '--format', 'json']
    verified = _command(executable, arguments, work, 'verification', **command_options)
    claims = _verified_claims(verified, commit=commit, qualification_sha256=qualification_sha256,
                              run_id=run_id, run_attempt=run_attempt, _windows=_windows, _linux=_linux)
    service = _reconcile_job(claims, _windows=_windows, _linux=_linux) if readback_job else None
    private_inspection_directory(work, source_root, fresh=False)
    for source, destination, expected, limit in copies:
        is_cli = windows_host and source == cli
        if bounded_file(source, limit, executable=is_cli)[0] != expected or bounded_file(destination, limit, executable=is_cli)[0] != expected:
            raise ValueError('Native attestation input changed during verification; preserve private work.')
    if bounded_file(artifact, qualification_limit)[0] != qualification_sha256:
        raise ValueError('Native qualification changed during verification.')
    result = dict(schema=1, format=origin_format, version=version, commit=commit,
                qualification_sha256=qualification_sha256, bundle_sha256=bundle_sha256,
                verifier=dict(version=CLI_VERSION, sha256=cli_sha256), trusted_root_sha256=trusted_root_sha256,
                native_producer_authentication='verified-pinned-workflow-oidc-attestation', origin=claims,
                native_job_conclusion='service-reconciled-success' if service is not None else 'not-service-reconciled',
                installed_acceptance='not-performed',
                public_availability='unverified', candidate_verified=False, ready=False, production_ready=False)
    if service is not None:
        result['native_service'] = service
    return result

