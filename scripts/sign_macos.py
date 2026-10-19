"""Explicit Developer ID signing on macOS; never installs or collects credentials.

This operates on a verified unsigned package in an isolated working directory.
A submission is recorded before waiting and can be resumed without resubmitting.
Nothing is published, and no existing release archive is overwritten.
"""
import argparse
import hashlib
import json
import os
import pathlib
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

from package import digest, git, regular_file, relative_path, write_zip
from verify_release import inspect_archive, MACOS_SIGNING_FILES, MACOS_SIGNING_KIND


APP = 'PeerBrush.app'
BUNDLE_ID = 'com.peerbrush.PeerBrush'
UUID = r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}'


def run(command, timeout=120):
    """Do not echo commands, account identifiers or raw tool logs on failure."""
    try:
        return subprocess.run(command, check=True, capture_output=True, text=True,
                              timeout=timeout, env=dict(os.environ, LC_ALL='C'))
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f'{pathlib.Path(command[0]).name} did not complete successfully; inspect the native step locally before retrying') from error


def parse_json(text, description):
    try:
        result = json.loads(text)
    except (ValueError, TypeError) as error:
        raise ValueError(f'Invalid {description} response') from error
    if not isinstance(result, dict):
        raise ValueError(f'Invalid {description} response')
    return result


def validate_configuration(team_id, identity, profile, revision, keychain=None, *, version):
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version or ''):
        raise ValueError('Developer ID signing requires a stable A.B.C release; internal development candidates cannot be signed')
    if sys.platform != 'darwin':
        raise ValueError('Developer ID signing requires an actual macOS runner')
    if not re.fullmatch(r'[A-Z0-9]{10}', team_id or ''):
        raise ValueError('An actual 10-character Apple Team ID is required')
    if not re.fullmatch(r'[A-Fa-f0-9]{40}', identity or ''):
        raise ValueError('The exact existing Developer ID Application certificate SHA-1 fingerprint is required')
    if not isinstance(profile, str) or not profile.strip() or any(ord(c) < 32 for c in profile) or len(profile) > 128:
        raise ValueError('An existing notarytool keychain profile name is required')
    if not re.fullmatch(r'[1-9][0-9]*', str(revision)):
        raise ValueError('An explicit positive signing packaging revision is required')
    if keychain is not None:
        keychain = pathlib.Path(keychain)
        if (not keychain.is_absolute() or not keychain.is_file()
                or any(path.is_symlink() for path in [keychain, *keychain.parents])):
            raise ValueError('An explicit keychain must be an existing absolute regular file without links')


def notary_credentials(profile, keychain=None):
    # TN3147: a file-based keychain must be selected explicitly on every call.
    # Adding it to the keychain search list does not select it for notarytool.
    options = ['--keychain-profile', profile]
    return options + (['--keychain', str(keychain)] if keychain is not None else [])


def preflight(team_id, identity, profile, keychain=None):
    available = run(['/usr/bin/security', 'find-identity', '-v', '-p', 'codesigning']
                    + ([str(keychain)] if keychain is not None else [])).stdout
    identities = re.findall(r'\b([0-9A-Fa-f]{40})\s+"([^"\n]+)"', available)
    selected = [name for fingerprint, name in identities if fingerprint.upper() == identity]
    if len(selected) != 1 or not selected[0].startswith('Developer ID Application: ') or not selected[0].endswith(f' ({team_id})'):
        raise ValueError('The selected valid Developer ID Application identity and Team ID are not available in the existing keychain')
    # Validate only existing access. No imports, store-credentials, unlock,
    # partition-list changes, API keys or account configuration are performed.
    run(['/usr/bin/xcrun', '--find', 'notarytool'])
    run(['/usr/bin/xcrun', '--find', 'stapler'])
    run(['/usr/bin/xcrun', 'notarytool', 'history', *notary_credentials(profile, keychain),
         '--output-format', 'json'])


def verify_native(app, team_id, notarized=False):
    """Actual Apple validation is mandatory before releasing a final artifact."""
    requirement = ('=anchor apple generic and certificate leaf[subject.OU] = '
                   f'"{team_id}" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists')
    run(['/usr/bin/codesign', '--verify', '--deep', '--strict', '--verbose=2',
         '--test-requirement', requirement, str(app)])
    detail = run(['/usr/bin/codesign', '--display', '--verbose=4', str(app)])
    text = detail.stdout + '\n' + detail.stderr
    if (f'TeamIdentifier={team_id}' not in text.splitlines()
            or f'Identifier={BUNDLE_ID}' not in text.splitlines()
            or not re.search(r'^Authority=Developer ID Application: .+ \(' + re.escape(team_id) + r'\)$', text, re.M)
            or not re.search(r'^CodeDirectory .*flags=0x[0-9a-fA-F]+\([^\n)]*\bruntime\b[^\n)]*\)', text, re.M)
            or not re.search(r'^Timestamp=\S.+$', text, re.M)):
        raise ValueError('Native signature lacks the expected Developer ID, bundle ID, hardened runtime or secure timestamp')
    entitlements = run(['/usr/bin/codesign', '--display', '--entitlements', '-', '--xml', str(app)]).stdout.strip()
    if entitlements:
        try:
            parsed = plistlib.loads(entitlements.encode())
        except (ValueError, plistlib.InvalidFileException) as error:
            raise ValueError('Unable to inspect signed entitlements') from error
        if parsed != {}:
            raise ValueError('Unexpected entitlements; the reviewed PeerBrush bundle requires no hardened-runtime exceptions')
    if notarized:
        run(['/usr/bin/xcrun', 'stapler', 'validate', str(app)])
        assessed = run(['/usr/sbin/spctl', '--assess', '--type', 'execute', '--verbose=4', str(app)])
        if 'source=Notarized Developer ID' not in (assessed.stdout + assessed.stderr):
            raise ValueError('Gatekeeper did not report Notarized Developer ID')


def tree_files(root):
    """Reject links, extra metadata and nonregular entries before packaging."""
    files = {}
    for path in sorted(root.rglob('*')):
        if path.is_symlink() or getattr(path.lstat(), 'st_file_attributes', 0) & 0x400:
            raise ValueError('Signing work must not contain links or reparse points')
        if path.is_dir():
            continue
        name = path.relative_to(root).as_posix()
        files[name] = regular_file(root, name)
    return files


def tree_hashes(root):
    return {name: digest(path) for name, path in tree_files(root).items()}


def extract_archive(archive_path, destination):
    """Call only after inspect_archive; never use general ZIP extractall."""
    destination.mkdir()
    with zipfile.ZipFile(archive_path) as archive:
        for item in archive.infolist():
            path = destination / relative_path(item.filename)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.read(item))
            path.chmod(item.external_attr >> 16 & 0o777)


def outside_checkout(path, root):
    path = path.absolute()
    # Also reject pre-existing links in an otherwise external working path.
    for parent in [path, *path.parents]:
        if parent.is_symlink():
            raise ValueError('Signing work/output paths must not use links')
    if path.resolve().is_relative_to(root.resolve()):
        raise ValueError('Signing work and outputs must be outside the source checkout')
    return path


def save_state(work, state):
    temporary = work / 'state.json.tmp'
    with temporary.open('x', encoding='utf-8') as stream:
        json.dump(state, stream, indent=2)
        stream.write('\n')
    os.replace(temporary, work / 'state.json')


def sign_archive(archive, output, work, commit, version, expected_sha256,
                 team_id, identity, profile, revision, source_root=None, resume=False,
                 keychain=None, on_submission=None):
    validate_configuration(team_id, identity, profile, revision, keychain, version=version)
    credentials = notary_credentials(profile, keychain)
    identity = identity.upper()
    root = source_root or pathlib.Path(__file__).resolve().parents[1]
    archive = regular_file(archive.absolute().parent, archive.name)
    if not re.fullmatch(r'[0-9a-f]{64}', expected_sha256 or '') or digest(archive) != expected_sha256:
        raise ValueError('Unsigned archive SHA-256 differs from the explicitly selected input')
    inspected = inspect_archive(archive, commit, version, root)
    manifest = inspected['manifest']
    if manifest['platform'] != 'macOS' or manifest['native_publisher_signing'] != 'not-configured' or inspected['signing_suffix']:
        raise ValueError('An unsigned verified macOS release package is required')
    work, output = outside_checkout(work, root), outside_checkout(output, root)
    if output.resolve().is_relative_to(work.resolve()) or work.resolve().is_relative_to(output.resolve()):
        raise ValueError('Signing work and final output directories must be separate')
    filename = archive.stem + f'-apple-signed-r{revision}.zip'
    targets = [output / filename, output / (filename + '.sha256'), output / (filename + '.verification.json')]
    if any(path.exists() or path.is_symlink() for path in targets):
        raise ValueError('Refusing to replace an existing signed artifact or receipt; select a new revision')
    configuration = {'schema': 1, 'input_sha256': expected_sha256, 'commit': commit,
                     'version': version, 'team_id': team_id, 'certificate_sha1': identity,
                     'revision': f'apple-signed-r{revision}'}
    if resume:
        state = parse_json(regular_file(work, 'state.json').read_text(), 'saved signing state')
        if state.get('configuration') != configuration or not re.fullmatch(UUID, state.get('submission_id', '')):
            raise ValueError('Saved signing state does not match this input, identity and revision')
        if state.get('bundle_hashes') != tree_hashes(work / 'payload') or state.get('submission_sha256') != digest(regular_file(work, 'submission.zip')):
            raise ValueError('Signing work changed after submission; do not resume or resubmit it')
    else:
        if work.exists() or work.is_symlink():
            raise ValueError('Use a fresh signing work directory, or --resume for an existing submission')
        work.mkdir(mode=0o700, parents=True)
        # A failed/ambiguous submit leaves this directory in place. Do not retry
        # automatically; reconcile notarytool history before another upload.
        state = {'configuration': configuration}
    preflight(team_id, identity, profile, keychain)
    prefix = inspected['prefix'].rstrip('/')
    bundle = work / 'payload' / prefix
    app = bundle / APP
    if not resume:
        extract_archive(archive, work / 'payload')
        # One Mach-O executable in this deliberately narrow bundle layout.
        # Signing the bundle signs its main executable. --deep is verification
        # only; newly introduced helpers require an explicit inside-out review.
        run(['/usr/bin/codesign', '--force', '--sign', identity, '--options', 'runtime',
             '--timestamp', '--identifier', BUNDLE_ID]
            + (['--keychain', str(keychain)] if keychain is not None else []) + [str(app)])
        verify_native(app, team_id)
        before = tree_hashes(bundle)
        allowed = set(MACOS_SIGNING_FILES) - {'PeerBrush.app/Contents/CodeResources'}
        with zipfile.ZipFile(archive) as unsigned:
            original = {i.filename[len(inspected['prefix']):]: hashlib.sha256(unsigned.read(i)).hexdigest() for i in unsigned.infolist()}
        if set(before) != set(original) | allowed or any(before[name] != checksum for name, checksum in original.items() if name != manifest['executable']):
            raise ValueError('Code signing changed files outside the reviewed executable and resource seal')
        run(['/usr/bin/ditto', '-c', '-k', '--keepParent', str(app), str(work / 'submission.zip')])
        state['submission_sha256'] = digest(regular_file(work, 'submission.zip'))
        state['bundle_hashes'] = tree_hashes(work / 'payload')
        save_state(work, state)
        submitted = parse_json(run(['/usr/bin/xcrun', 'notarytool', 'submit', str(work / 'submission.zip'),
                                    *credentials, '--output-format', 'json'], timeout=1800).stdout, 'notarization submission')
        submission_id = submitted.get('id', '')
        if not re.fullmatch(UUID, submission_id):
            raise ValueError('No valid notarization submission ID returned; reconcile history before another upload')
        state['submission_id'] = submission_id
        save_state(work, state)
    submission_id = state['submission_id']
    if on_submission is not None:
        # Publish only a credential-free receipt before the potentially long
        # wait, so an interrupted runner can reconcile this exact submission.
        on_submission({'submission_id': submission_id, 'commit': commit,
                       'unsigned_archive_sha256': expected_sha256,
                       'submission_archive_sha256': state['submission_sha256']})
    # A timeout or pending result keeps the exact payload and ID for --resume.
    # Do not use another submit call to check an existing job.
    run(['/usr/bin/xcrun', 'notarytool', 'wait', submission_id, *credentials,
         '--timeout', '30m', '--output-format', 'json'], timeout=1860)
    status = parse_json(run(['/usr/bin/xcrun', 'notarytool', 'info', submission_id,
                             *credentials, '--output-format', 'json']).stdout, 'notarization status')
    if status.get('id') != submission_id or status.get('status') != 'Accepted':
        raise ValueError('Notarization is not Accepted; preserve the work directory and resume only the same submission')
    log = parse_json(run(['/usr/bin/xcrun', 'notarytool', 'log', submission_id,
                          *credentials]).stdout, 'notarization log')
    if str(log.get('jobId', '')).lower() != submission_id or log.get('status') != 'Accepted' or log.get('sha256') != state['submission_sha256']:
        raise ValueError('Notarization log does not match the exact submitted archive')
    if log.get('issues') not in [None, []]:
        raise ValueError('Notarization log contains issues; inspect the log locally before distribution')
    # Keep the submitted payload immutable so a failed staple/assessment/output
    # step can be retried with the same submission rather than uploaded again.
    with tempfile.TemporaryDirectory(prefix='.finalize-', dir=work) as temporary:
        stage = pathlib.Path(temporary)
        shutil.copytree(work / 'payload', stage / 'payload')
        bundle = stage / 'payload' / prefix
        app = bundle / APP
        run(['/usr/bin/xcrun', 'stapler', 'staple', str(app)])
        verify_native(app, team_id, notarized=True)
        files = tree_files(bundle)
        expected = {name[len(prefix) + 1:] for name in state['bundle_hashes']} | MACOS_SIGNING_FILES
        if set(files) != expected:
            raise ValueError('Unexpected files after stapling; refuse an unreviewed signing metadata layout')
        for name, checksum in state['bundle_hashes'].items():
            if digest(regular_file(stage / 'payload', name)) != checksum:
                raise ValueError('Stapling changed an existing payload file')
        manifest['executable_sha256'] = digest(files[manifest['executable']])
        manifest['native_publisher_signing'] = MACOS_SIGNING_KIND
        manifest['macos_signing'] = {
            'schema': 1, 'revision': configuration['revision'], 'team_id': team_id,
            'certificate_sha1': identity, 'unsigned_archive_sha256': expected_sha256,
            'unsigned_executable_sha256': inspected['record']['executable_sha256'],
            'submission_archive_sha256': state['submission_sha256'],
            'notary_submission_id': submission_id, 'notary_status': 'Accepted',
            'hardened_runtime': True, 'secure_timestamp': True, 'stapled': True,
            'metadata_files': {name: digest(files[name]) for name in sorted(MACOS_SIGNING_FILES)},
        }
        (bundle / 'release.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
        final = stage / filename
        epoch = int(git(root, 'show', '-s', '--format=%ct', commit))
        write_zip(final, [(f'{prefix}/{name}', path.read_bytes(), 0o755 if name == manifest['executable'] else 0o644)
                          for name, path in tree_files(bundle).items()], epoch)
        checked = inspect_archive(final, commit, version, root)
        # Native verification must use the *final ZIP's extracted bytes*, not just
        # the pre-archive staging bundle. This also detects ticket-loss in ZIP tools.
        extract_archive(final, stage / 'final-check')
        verify_native(stage / 'final-check' / prefix / APP, team_id, notarized=True)
        final_hash = digest(final)
        receipt = {'schema': 1, 'file': filename, 'sha256': final_hash, 'commit': commit,
                   'binary_source_commit': checked['record']['binary_source_commit'],
                   'executable_sha256': manifest['executable_sha256'],
                   'source_sha256': manifest['source_sha256'],
                   'unsigned_archive_sha256': expected_sha256,
                   'packaging_revision': configuration['revision'],
                   'native_checks': ['codesign Developer ID and Team ID requirement',
                                     'hardened runtime and secure timestamp',
                                     'empty entitlements', 'stapler validate',
                                     'spctl Notarized Developer ID'],
                   'native_launch_test': 'not-performed',
                   'fresh_quarantined_download_test': 'not-performed'}
        output.mkdir(parents=True, exist_ok=True)
        # Exclusive creation never silently replaces an artifact, including races.
        with targets[0].open('xb') as stream:
            stream.write(final.read_bytes())
        with targets[1].open('x', encoding='utf-8') as stream:
            stream.write(f'{final_hash}  {filename}\n')
        with targets[2].open('x', encoding='utf-8') as stream:
            json.dump(receipt, stream, indent=2)
            stream.write('\n')
        return targets[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    path_help = {'archive': 'Verified unsigned native macOS ZIP; kept unchanged',
                 'output': 'Final output directory outside the source checkout',
                 'work': 'Fresh private work directory outside the checkout; retained for --resume'}
    for name, help_text in path_help.items():
        parser.add_argument('--' + name, type=pathlib.Path, required=True, help=help_text)
    input_help = {'commit': 'Exact full committed source SHA matching the input archive',
                  'version': 'Exact package version, for example 0.25.0',
                  'expected-sha256': 'Independently verified SHA-256 of the unsigned input ZIP',
                  'team-id': 'Actual public ten-character Apple Developer Team ID',
                  'identity': '40-hex SHA-1 selector of an existing Developer ID Application certificate',
                  'profile': 'Existing notarytool Keychain profile name; never a password or key',
                  'revision': 'Positive integer for the explicit apple-signed-rN packaging label'}
    for name, help_text in input_help.items():
        parser.add_argument('--' + name, required=True, help=help_text)
    parser.add_argument('--keychain', type=pathlib.Path,
                        help='Existing absolute file-based keychain for both signing and every notarytool call')
    parser.add_argument('--resume', action='store_true', help='Wait for the saved submission without uploading again')
    args = parser.parse_args()
    try:
        result = sign_archive(args.archive, args.output, args.work, args.commit, args.version,
                              args.expected_sha256, args.team_id, args.identity, args.profile,
                              args.revision, resume=args.resume, keychain=args.keychain,
                              on_submission=lambda receipt: print(json.dumps(receipt), flush=True))
        print(json.dumps({'file': result.name, 'sha256': digest(result),
                          'status': 'signed-notarized-stapled; native UI/download testing still required'}))
    except (ValueError, OSError, KeyError, zipfile.BadZipFile) as error:
        message = str(error) if isinstance(error, ValueError) else 'Input/output validation failed; review the local work directory'
        parser.exit(1, 'macOS signing failed: ' + message + '\n')


if __name__ == '__main__':
    main()
