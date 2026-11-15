"""Qualify a signed APT install or version upgrade in a disposable Ubuntu container."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

import prepare_apt_repository as apt
import verify_apt_stage
from attach_apt_signatures import client_sources, signed_receipt
from final_release import bounded_file, read
from package_deb import MAX_DEB_BYTES, deb_archives, deb_filename, deb_version, tar_entries
from package import release_version_order
from production_readiness import handoff_path, unique_fields
from readback_apt_repository import check as check_public_repository
from test_deb_installation import (APP_ID, DOCUMENTATION_OPTIONS, MIME, installed_identity,
                                   preservation_fixture, receipt_path, require_disposable,
                                   run, selected_package, validate_expected, write_receipt)
from test_deb_installed_gui import (DESKTOP, Window, check_startup_window,
                                    desktop_session_environment, isolated_display_environment)
from test_deb_version_upgrade import (PROJECT_SIZE, PackageRegistration, PackageWorkspace, Session,
                                      failure_diagnostic, installed_runuser)
from test_msix_runtime import require_linux_update_owner, smoke
from verify_apt_signatures import KEYRING_NAME
from verify_apt_stage import package_binding


KEYRING = 'bootstrap/' + KEYRING_NAME
DESKTOP_INPUT_LIMIT = 256 * 1024


def snapshot(selection, destination, repository_uri, fingerprint, previous):
    stage, receipt, checksum = selection
    stage = handoff_path(stage)
    options = dict(repository_uri=repository_uri, fingerprint=fingerprint, previous=previous)
    verify_apt_stage.check(stage, receipt, checksum, **options)
    report, _ = signed_receipt(receipt, checksum)
    inventory = {f'repository/{name}': record for name, record in report['files'].items()}
    inventory.update(report['bootstrap']['files'])
    modified = apt.timestamp(report['date']).timestamp()
    for name, record in inventory.items():
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        apt.copy_package(handoff_path(stage / name), target, record)
        target.chmod(0o644)
        # APT's file transport can reuse metadata with the same modification time.
        os.utime(target, (modified, modified))
    verify_apt_stage.check(destination, receipt, checksum, **options)
    package_path, _, expected = package_binding(report)
    selected_package(destination / 'repository' / package_path, expected)
    return report, expected


def archived_package(selection, stage, successor, repository_uri, fingerprint):
    receipt, checksum, install, install_sha256 = selection
    report, _ = signed_receipt(receipt, checksum)
    package_path, record, expected = package_binding(report)
    if not re.fullmatch('[0-9a-f]{64}', install_sha256):
        raise ValueError('Select the independently authenticated historical APT install receipt.')
    observed = json.loads(read(install, 128 * 1024, install_sha256), object_pairs_hook=unique_fields)
    required = dict(schema=1, format='PeerBrushAptInstallV1', product='PeerBrush',
        platform='Ubuntu 24.04 amd64', primary_fingerprint=fingerprint,
        receipt_sha256=checksum, candidate=expected, missing_key='refused',
        predecessor_receipt_sha256=report.get('predecessor_manifest_sha256'),
        initial_install='verified', purge='verified', update_owner='LinuxPackage',
        user_files='preserved', upgrade='not-performed')
    if not isinstance(observed, dict) or any(observed.get(key) != value for key, value in required.items()):
        raise ValueError('The historical APT install receipt does not qualify this package.')
    if observed.get('transport') == 'public-https':
        verify_apt_stage.verify_https_scope(report, observed.get('apt_https'))
    elif observed.get('transport') != 'local-file' or 'apt_https' in observed:
        raise ValueError('The historical APT install receipt does not qualify this transport.')
    verified = report.get('signature_verification')
    bootstrap = report['bootstrap'].get('files')
    if (report['bootstrap'].get('repository_uri') != repository_uri
            or not isinstance(verified, dict) or verified.get('primary_fingerprint') != fingerprint
            or not isinstance(bootstrap, dict)
            or bootstrap.get(KEYRING) != successor['bootstrap']['files'][KEYRING]):
        raise ValueError('APT upgrade acceptance requires one unchanged approved keyring and repository.')
    if successor['files'].get(package_path) != record:
        raise ValueError('The active APT candidate does not retain the historical package at its immutable URL.')
    package = handoff_path(stage / 'repository' / package_path)
    selected_package(package, expected)
    return expected, package


def apt_environment(work, sources, *, public_https=False):
    empty = work / 'empty'
    for directory in (empty, work / 'lists', work / 'archives'):
        directory.mkdir()
    config = work / 'apt.conf'
    entries = {
        'Dir::Etc::Parts': empty, 'Dir::Etc::main': empty / 'apt.conf',
        'Dir::Etc::sourcelist': sources, 'Dir::Etc::sourceparts': empty,
        'Dir::Etc::preferences': empty / 'preferences', 'Dir::Etc::preferencesparts': empty,
        'Dir::Etc::trusted': empty / 'trusted.gpg', 'Dir::Etc::trustedparts': empty,
        'Dir::State::lists': work / 'lists', 'Dir::Cache::archives': work / 'archives',
        'Dir::Cache::pkgcache': '', 'Dir::Cache::srcpkgcache': '',
        'Acquire::AllowInsecureRepositories': 'false', 'Acquire::AllowDowngradeToInsecureRepositories': 'false',
        'Acquire::Check-Date': 'true', 'Acquire::Check-Valid-Until': 'true',
        'APT::Get::AllowUnauthenticated': 'false', 'Acquire::Languages': 'none',
    }
    if public_https:
        entries.update({'Acquire::https::Verify-Peer': 'true', 'Acquire::https::Verify-Host': 'true',
                        'Acquire::https::Proxy': 'DIRECT', 'Acquire::https::AllowRedirect': 'false',
                        'Acquire::http::AllowRedirect': 'false'})
    config.write_text(''.join(f'{name} "{value}";\n' for name, value in entries.items()), encoding='ascii')
    return dict(APT_CONFIG=str(config), PATH='/usr/sbin:/usr/bin:/sbin:/bin', HOME='/root',
                LC_ALL='C', DEBIAN_FRONTEND='noninteractive')


def require_apt_bytes(path, record):
    if bounded_file(path, record['bytes']) != (record['sha256'], record['bytes']):
        raise ValueError(f'Native APT bytes for {path.name} differ from the signed candidate.')


def require_uninstalled():
    packages = run(['dpkg-query', '--show', '--showformat=${binary:Package}\t${db:Status-Status}\n'])
    for line in packages.splitlines():
        name, _, status = line.partition('\t')
        if name.partition(':')[0] == 'peerbrush' and status != 'not-installed':
            raise ValueError('APT acceptance requires PeerBrush to be fully uninstalled.')
    for path in ('/usr/bin/peerbrush', f'/usr/share/applications/{APP_ID}.desktop',
                 f'/usr/share/mime/packages/{APP_ID}.xml'):
        if Path(path).exists() or Path(path).is_symlink():
            raise ValueError('APT acceptance found existing PeerBrush launch integration.')
    cache = Path('/usr/share/applications/mimeinfo.cache')
    if cache.is_file() and f'{APP_ID}.desktop' in cache.read_text(encoding='utf-8'):
        raise ValueError('APT acceptance found stale PeerBrush desktop registration.')


def require_missing_key_refusal(env):
    try:
        run(['apt-get', '--error-on=any', 'update'], env=env)
    except ValueError as error:
        cause = error.__cause__
        if isinstance(cause, subprocess.CalledProcessError) and re.search(r'\bNO_PUBKEY\b', cause.stderr or ''):
            return
        raise ValueError('APT did not produce the expected missing-key refusal.') from None
    raise ValueError('APT accepted signed repository metadata without the selected key.')


def desktop_input(package, expected, workspace):
    selected_package(package, expected)
    workspace.preserve()
    entries = tar_entries(deb_archives(package)['data.tar.xz'])
    _, data = entries.get(DESKTOP.relative_to('/').as_posix(), (None, None))
    if data is None or not 0 < len(data) <= 64 * 1024:
        raise ValueError('Selected APT package lacks its bounded desktop entry.')
    desktop = bytes(data).decode('utf-8')
    lines = desktop.splitlines()
    if ([line for line in lines if line.partition('=')[0].strip() == 'Exec'] != ['Exec=/usr/bin/peerbrush -- %F']
            or any(line.partition('=')[0].strip() == 'DBusActivatable' for line in lines)):
        raise ValueError('Selected APT package lacks the guarded desktop Exec route.')
    return dict(expected=expected, desktop=desktop, projects={
        path.name: dict(sha256=workspace.files[path], definition=definition, native_hex=samples[3].hex())
        for path, (definition, samples) in workspace.projects.items()
    })


def desktop_child():
    if (sys.platform != 'linux' or os.environ.get('PEERBRUSH_DISPOSABLE_APT_DESKTOP') != '1'
            or os.environ.get('PEERBRUSH_DISPOSABLE_PACKAGE_TEST') != '1' or not Path('/.dockerenv').is_file()):
        raise ValueError('APT desktop checks require the explicitly disposable installation container.')
    import pwd
    account = pwd.getpwnam('peerbrush-qa')
    home = Path('/home/peerbrush-qa')
    if os.geteuid() != account.pw_uid or account.pw_uid == 0 or Path(account.pw_dir) != home or os.environ.get('HOME') != str(home):
        raise ValueError('APT desktop checks require the synthetic non-root account.')
    display = isolated_display_environment()
    raw = sys.stdin.buffer.read(DESKTOP_INPUT_LIMIT + 1)
    if len(raw) > DESKTOP_INPUT_LIMIT:
        raise ValueError('APT desktop fixture exceeds its readback budget.')
    facts = json.loads(raw, object_pairs_hook=unique_fields)
    validate_expected(facts['expected'])
    artwork = home / 'Repository artwork'
    workspace = PackageWorkspace(home / 'Repository QA state', artwork)
    for depth in (8, 16):
        for extension in ('psd', 'psb'):
            path = artwork / f'Artwork {depth} 作品.{extension}'
            record = facts['projects'][path.name]
            checksum = record['sha256']
            native = record['native_hex']
            width, height = PROJECT_SIZE
            if (not re.fullmatch('[0-9a-f]{64}', checksum)
                    or len(native) != width * height * depth
                    or not re.fullmatch('[0-9a-f]+', native)):
                raise ValueError('APT desktop fixture lacks its exact native sample evidence.')
            workspace.files[path] = checksum
            workspace.projects[path] = record['definition'], (width, height, depth, bytes.fromhex(native))
    if set(facts['projects']) != {path.name for path in workspace.projects}:
        raise ValueError('Unexpected APT desktop fixture inventory.')
    workspace.preserve()
    with tempfile.TemporaryDirectory(prefix='display-', dir=home / 'Repository desktop QA') as temporary:
        root = Path(temporary)
        env = desktop_session_environment(root, dict(display, HOME=str(home)))
        for path in workspace.projects:
            if run(['/usr/bin/xdg-mime', 'query', 'filetype', str(path)], env=env) != MIME:
                raise ValueError('Installed APT MIME integration did not recognize the native fixture.')
        window = Window(facts, root, connection=Path(env['TMPDIR']) / 'PeerBrush/connection.json', env=env)
        check_startup_window(window, workspace)


def check(older, newer, output, *, repository_uri, fingerprint, older_previous=None,
          archived=None, standalone=None, newer_previous=None, desktop=False, public_bootstrap_uri=None):
    if standalone is not None and (newer is None or any(
            value is not None for value in (older, archived, older_previous, newer_previous))):
        raise ValueError('A standalone DEB baseline requires one initial signed upgrade target.')
    public_https = public_bootstrap_uri is not None
    if public_https and newer is not None and archived is None and standalone is None:
        raise ValueError('Public APT acceptance requires a clean install or a verified local upgrade baseline.')
    require_disposable()
    output = receipt_path(output)
    if run(['dpkg', '--print-architecture']) != 'amd64':
        raise ValueError('APT acceptance requires a native amd64 package database.')
    require_uninstalled()
    with tempfile.TemporaryDirectory(prefix='peerbrush-apt-qa-', dir='/tmp') as directory:
        work = Path(directory)
        work.chmod(0o755)  # The sandboxed _apt user must read the fixture.
        first, second = work / 'older', work / 'newer'
        baseline_package = None
        if standalone is not None:
            path, version, commit, package_sha256, executable_sha256, source_sha256 = standalone
            prior = dict(version=version, commit=commit, package_sha256=package_sha256,
                         executable_sha256=executable_sha256, source_sha256=source_sha256)
            validate_expected(prior)
            path = handoff_path(path)
            if path.name != deb_filename(version):
                raise ValueError('Standalone DEB baseline has a different versioned filename.')
            baseline_package = work / path.name
            checksum, _ = bounded_file(path, MAX_DEB_BYTES, baseline_package)
            if checksum != package_sha256:
                raise ValueError('Standalone DEB bytes differ from the selected baseline.')
            baseline_package.chmod(0o644)
            selected_package(baseline_package, prior)
            next_report, current = snapshot(newer, second, repository_uri, fingerprint, None)
            active_stage, trust_report = second, next_report
        elif archived is not None:
            next_report, current = snapshot(newer, second, repository_uri, fingerprint, newer_previous or archived[:2])
            prior, baseline_package = archived_package(archived, second, next_report, repository_uri, fingerprint)
            active_stage, trust_report = second, next_report
        else:
            prior_report, prior = snapshot(older, first, repository_uri, fingerprint, older_previous)
            active_stage, trust_report = first, prior_report
            if newer is not None:
                next_report, current = snapshot(newer, second, repository_uri, fingerprint, older[1:])
                if prior_report['bootstrap']['files'][KEYRING] != next_report['bootstrap']['files'][KEYRING]:
                    raise ValueError('APT upgrade acceptance requires one unchanged approved keyring.')
        if newer is not None:
            if release_version_order(current['version']) <= release_version_order(prior['version']):
                raise ValueError('APT upgrade acceptance requires two increasing application versions.')
        if public_https:
            https = verify_apt_stage.https_scope(trust_report, public_bootstrap_uri)
            check_public_repository(repository_uri, public_bootstrap_uri, trust_report['files'],
                                    trust_report['bootstrap']['files'])

        home, user, retained = preservation_fixture()
        retained()
        state, artwork = home / 'Repository QA state', home / 'Repository artwork'
        client_home = home / 'Repository client'
        user(['/usr/bin/mkdir', '-m', '700', state, artwork, client_home, client_home / '.cursor'])
        workspace = PackageWorkspace(state, artwork)
        registration = PackageRegistration(client_home, state)
        environment = dict(PATH=os.defpath, HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
                           XDG_DATA_HOME=str(home / '.local/share'), XDG_CACHE_HOME=str(home / '.cache'),
                           TMPDIR=str(state), LANG='C.UTF-8')
        logs = work / 'workspace-check'
        logs.mkdir(mode=0o700)
        runuser = installed_runuser()
        run_as = [runuser, '-u', 'peerbrush-qa', '--']
        if desktop:
            user(['/usr/bin/mkdir', '-m', '700', home / 'Repository desktop QA'])

        def discovery_command(args, env):
            # Set the fixture home after runuser/PAM selects the account home.
            return run([*run_as, '/usr/bin/env', f'HOME={env["HOME"]}',
                        f'CODEX_HOME={env["CODEX_HOME"]}', *args], env=env)
        active = work / 'active'
        active.symlink_to(active_stage.name, target_is_directory=True)
        keyring = work / KEYRING_NAME
        keyring.touch(mode=0o644, exist_ok=False)
        keyring.chmod(0o644)
        sources = work / 'peerbrush.sources'
        source_entry = client_sources(repository_uri, trust_report, fingerprint)
        if not public_https:
            source_entry = source_entry.replace(f'URIs: {repository_uri}\n'.encode('ascii'),
                                                f'URIs: {(active / "repository").as_uri()}\n'.encode('ascii'), 1)
        sources.write_bytes(source_entry.replace(f'/etc/apt/keyrings/{KEYRING_NAME} '.encode('ascii'),
                                                 f'{keyring} '.encode('ascii'), 1))
        sources.chmod(0o644)
        env = apt_environment(work, sources, public_https=public_https)
        require_missing_key_refusal(env)
        # Replace the intentionally empty keyring with the exact scoped trust input.
        selected_key = work / 'verified-keyring.gpg'
        apt.copy_package(active_stage / KEYRING, selected_key, trust_report['bootstrap']['files'][KEYRING])
        selected_key.chmod(0o644)
        os.replace(selected_key, keyring)

        def refresh(expected):
            run(['apt-get', '--error-on=any', 'update'], env=env)
            if public_https:
                metadata = list((work / 'lists').glob('*_InRelease'))
                if len(metadata) != 1:
                    raise ValueError('Native APT did not accept exactly one HTTPS InRelease.')
                require_apt_bytes(metadata[0], trust_report['files'][apt.DIST + 'InRelease'])
            policy = run(['apt-cache', 'policy', 'peerbrush'], env=env)
            if re.findall(r'^\s*Candidate: (\S+)\s*$', policy, re.M) != [deb_version(expected['version'])]:
                raise ValueError('APT did not select the expected signed repository version.')

        def install(expected, *options, package='peerbrush'):
            documentation = [part for option in DOCUMENTATION_OPTIONS for part in ('-o', 'Dpkg::Options::=' + option)]
            command = ['apt-get', 'install', '-y', '--no-remove', '--no-install-recommends', *documentation, *options]
            if public_https and package == 'peerbrush':
                _, record, _ = package_binding(trust_report)
                version = deb_version(expected['version'])
                selected = f'peerbrush={version}'
                run([*command, '--download-only', selected], env=env)
                # APT percent-encodes the epoch colon in its cache filename.
                cached = f'peerbrush_{version.replace(":", "%3a")}_amd64.deb'
                require_apt_bytes(work / 'archives' / cached, record)
                run([*command, '--no-download', selected], env=env)
            else:
                run([*command, str(package)], env=env)
            build = installed_identity(expected, user, deb_version(expected['version']))
            raw = user(['/usr/bin/peerbrush', '--installation-info']).encode('utf-8')
            require_linux_update_owner(raw, 'LinuxPackage', build)
            if (not Path(f'/usr/share/applications/{APP_ID}.desktop').is_file()
                    or not Path(f'/usr/share/mime/packages/{APP_ID}.xml').is_file()
                    or f'{APP_ID}.desktop' not in Path('/usr/share/applications/mimeinfo.cache').read_text(encoding='utf-8')):
                raise ValueError('APT install did not register PeerBrush desktop and MIME integration.')
            launcher = Path('/usr/bin/peerbrush')
            with Session(launcher, environment, logs, state, runuser,
                         account='peerbrush-qa') as session:
                route = registration.check(launcher, client_home / 'old' / 'peerbrush', environment,
                                           discovery_command)
                observation = smoke(route[0], state, allow_portable=True, env=environment,
                                    run_as=run_as)['installation_observation']
                if observation['build'] != build or observation.get('update_owner') != 'LinuxPackage':
                    raise ValueError('The saved APT MCP launcher reached a different installation.')
                workspace.check(session.rpc)
            if desktop:
                archive = Path(package)
                if package == 'peerbrush':
                    filename = deb_filename(expected["version"])
                    archive = active.resolve(strict=True) / 'repository' / apt.POOL / filename
                facts = json.dumps(desktop_input(archive, expected, workspace))
                if len(facts) > DESKTOP_INPUT_LIMIT:
                    raise ValueError('APT desktop fixture exceeds its handoff budget.')
                run([*run_as, '/usr/bin/xvfb-run', '-a', '-s', '-screen 0 1600x1000x24 -nolisten tcp',
                     '/usr/bin/dbus-run-session', '--', sys.executable, '-B', str(Path(__file__).resolve()),
                     '--inside-desktop'], env=dict(environment, PEERBRUSH_DISPOSABLE_PACKAGE_TEST='1',
                                                   PEERBRUSH_DISPOSABLE_APT_DESKTOP='1'), input=facts)
                workspace.preserve()
            retained()

        if baseline_package is None:
            refresh(prior)
            install(prior)
        else:
            install(prior, package=baseline_package)
        if newer is not None:
            if baseline_package is None:
                replacement = work / 'next-generation'
                replacement.symlink_to(second.name, target_is_directory=True)
                os.replace(replacement, active)
            refresh(current)
            install(current, '--only-upgrade')
        run(['apt-get', 'purge', '-y', 'peerbrush'], env=env)
        require_uninstalled()
        retained()
        workspace_evidence = workspace.retention_receipt()
        registration.preserve()
    evidence = dict(schema=1, product='PeerBrush', platform='Ubuntu 24.04 amd64',
        primary_fingerprint=fingerprint, transport='local-file', missing_key='refused',
        initial_install='verified', purge='verified', update_owner='LinuxPackage',
        user_files='preserved', workspace=workspace_evidence,
        mcp_registration='private-client-config-backed-up-and-reused',
        desktop_startup_documents='gio-native8-and-native16-psd-and-psb-verified' if desktop else 'not-performed',
        public_availability='unverified', release_ready=False)
    if public_https:
        evidence.update(transport='public-https', apt_https=https)
    if newer is None:
        evidence.update(format='PeerBrushAptInstallV1', receipt_sha256=older[2],
                        predecessor_receipt_sha256=prior_report.get('predecessor_manifest_sha256'),
                        candidate=prior, upgrade='not-performed')
    elif standalone is not None:
        evidence.update(format='PeerBrushAptStandaloneUpgradeV1', initial_install='standalone-deb-verified',
                        newer_receipt_sha256=newer[2], older=prior, newer=current, upgrade='verified')
    elif archived is not None:
        evidence.update(format='PeerBrushAptArchivedUpgradeV1', initial_install='historical-receipt-verified',
                        older_receipt_sha256=archived[1], historical_install_sha256=archived[3],
                        newer_receipt_sha256=newer[2], predecessor_receipt_sha256=(newer_previous or archived[:2])[1],
                        older=prior, newer=current, upgrade='verified')
    else:
        evidence.update(format='PeerBrushAptUpgradeV1', older_receipt_sha256=older[2],
                        newer_receipt_sha256=newer[2], older=prior, newer=current, upgrade='verified')
    write_receipt(output, evidence)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    argv = sys.argv[1:] if argv is None else argv
    if argv == ['--inside-desktop']:
        try:
            desktop_child()
        except Exception as error:
            parser.exit(1, 'Disposable APT desktop acceptance failed. ' + failure_diagnostic(error) + '\n')
        return 0
    mode = parser.add_mutually_exclusive_group(required=True)
    selection = ('STAGE', 'RECEIPT', 'SHA256')
    mode.add_argument('--install-only', nargs=3, metavar=selection)
    mode.add_argument('--older', nargs=3, metavar=selection)
    mode.add_argument('--archived-baseline', nargs=4,
        metavar=('SIGNED_RECEIPT', 'SHA256', 'INSTALL_RECEIPT', 'INSTALL_SHA256'))
    mode.add_argument('--standalone-baseline', nargs=6,
        metavar=('DEB', 'VERSION', 'COMMIT', 'DEB_SHA256', 'EXECUTABLE_SHA256', 'SOURCE_SHA256'))
    parser.add_argument('--newer', nargs=3, metavar=selection)
    parser.add_argument('--older-previous', nargs=2, metavar=('RECEIPT', 'SHA256'))
    parser.add_argument('--install-previous', nargs=2, metavar=('RECEIPT', 'SHA256'),
                        help='The signed predecessor of an --install-only candidate with repository history.')
    parser.add_argument('--newer-previous', nargs=2, metavar=('RECEIPT', 'SHA256'))
    parser.add_argument('--repository-uri', required=True)
    parser.add_argument('--public-bootstrap-uri', help='Qualify the approved public HTTPS repository and bootstrap base.')
    parser.add_argument('--fingerprint', required=True)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--desktop', action='store_true', help='Check GIO document startup in an isolated Xvfb display.')
    args = parser.parse_args(argv)
    if (args.older or args.archived_baseline or args.standalone_baseline) and args.newer is None:
        parser.error('An upgrade baseline requires --newer')
    if args.install_only and args.newer:
        parser.error('--install-only accepts no upgrade target')
    if args.older_previous and not args.older:
        parser.error('--older-previous requires --older')
    if args.install_previous and not args.install_only:
        parser.error('--install-previous requires --install-only')
    if args.newer_previous and not args.archived_baseline:
        parser.error('--newer-previous requires --archived-baseline')
    if args.public_bootstrap_uri is not None and args.older:
        parser.error('--public-bootstrap-uri requires --install-only, --archived-baseline or --standalone-baseline')
    options = dict(repository_uri=args.repository_uri, fingerprint=args.fingerprint,
                   older_previous=args.install_previous or args.older_previous)
    if args.desktop:
        options['desktop'] = True
    if args.public_bootstrap_uri is not None:
        options['public_bootstrap_uri'] = args.public_bootstrap_uri
    if args.archived_baseline:
        options.update(archived=args.archived_baseline, newer_previous=args.newer_previous)
    if args.standalone_baseline:
        options['standalone'] = args.standalone_baseline
    check(args.install_only or args.older, args.newer, args.output, **options)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
