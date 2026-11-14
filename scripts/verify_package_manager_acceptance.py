"""Bind private native package-manager evidence to one exact release candidate."""
import argparse
import json
from pathlib import Path
import re

from attach_apt_signatures import signed_receipt
from final_release import read
from prepare_site_release import STABLE
from package import release_version_order
from production_readiness import unique_fields
from test_homebrew_upgrade import select as select_homebrew
from test_nix_profile_upgrade import NAR_HASH, definition_inventory, source_inputs
from test_scoop_upgrade import DESKTOP_CONNECT, SHELL_DESKTOP, select as select_scoop
from verify_apt_signatures import FINGERPRINT
from verify_apt_stage import package_binding, verify_https_scope


HEX = r'[0-9a-f]{64}'
FORMATS = dict(scoop='Scoop', homebrew='Homebrew', apt='Apt', nix='NixProfile')
DESKTOP = dict(scoop='native8-and-native16-psd-and-psb-verified',
               homebrew='launch-services-native8-and-native16-psd-and-psb-verified',
               apt='gio-native8-and-native16-psd-and-psb-verified',
               nix='gio-native8-and-native16-psd-and-psb-verified')


def matches(pattern, value):
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def require_fields(value, expected, label):
    if not isinstance(value, dict) or any(
            key not in value or type(value[key]) is not type(want) or value[key] != want
            for key, want in expected.items()):
        raise ValueError(f'{label} does not match the selected candidate or qualification scope.')


def workspace_evidence(workspace):
    cells = {(extension, depth) for extension in ('psd', 'psb') for depth in (8, 16)}
    if not isinstance(workspace, dict) or not matches(HEX, workspace.get('tool_preferences_sha256')):
        raise ValueError('Native evidence lacks the retained workspace preferences.')
    projects = workspace.get('projects')
    if not isinstance(projects, list) or len(projects) != len(cells):
        raise ValueError('Native evidence lacks all four retained PSD and PSB fixtures.')
    for project in projects:
        if (not isinstance(project, dict) or not matches(r'psd|psb', project.get('format'))
                or type(project.get('bit_depth')) is not int
                or (project['format'], project['bit_depth']) not in cells
                or any(not matches(HEX, project.get(key)) for key in ('file_sha256', 'rendered_sha256'))):
            raise ValueError('Native evidence has ambiguous or incomplete project fixtures.')
        cells.remove((project['format'], project['bit_depth']))


def definition_binding(channel, definition, version, source_commit, publisher, *, https_evidence=None):
    path, digest = definition
    if channel == 'apt':
        report, _ = signed_receipt(path, digest)
        package_path, _, candidate = package_binding(report)
        require_fields(candidate, dict(version=version, commit=source_commit), 'Signed APT definition')
        require_fields(report.get('signature_verification'), dict(primary_fingerprint=publisher), 'APT signer')
        if https_evidence is not None:
            verify_https_scope(report, https_evidence)
        return (candidate, report.get('predecessor_manifest_sha256'), candidate['source_sha256'],
                package_path.rsplit('/', 1)[1])
    expected_name = 'nix-package.json' if channel == 'nix' else 'package-managers.json'
    if path.name != expected_name:
        raise ValueError(f'Select the reviewed {expected_name} definition receipt.')
    report = json.loads(read(path, 512 * 1024, digest), object_pairs_hook=unique_fields)
    if channel == 'nix':
        require_fields(report, dict(version=version, commit=source_commit), 'Nix definition')
        files, _ = definition_inventory(report)
        inputs = read(path.parent / 'source-inputs.json', 64 * 1024, files['source-inputs.json'])
        source = source_inputs(report, files, inputs)
        return dict(inputs=source), None, source['source_sha256'], f'PeerBrush-{version}-source.zip'
    require_fields(report, dict(version='v' + version, commit=source_commit), 'Catalog definition')
    _, asset, _ = (select_scoop((path.parent, digest), for_install=True) if channel == 'scoop'
                   else select_homebrew((path.parent, digest), publisher))
    require_fields(asset, dict(binary_source_commit=source_commit), 'Installed archive source')
    candidate = dict(version=version, definition_sha256=digest, archive_sha256=asset['sha256'], owner_observed=True)
    return candidate, None, asset['source_sha256'], asset['file']


def nix_generation(candidate):
    closure = candidate.get('closure')
    if (type(candidate.get('generation')) is not int or candidate['generation'] < 1
            or any(not matches(NAR_HASH, candidate.get(key)) for key in ('flake_nar_sha256', 'nar_sha256'))
            or not isinstance(closure, dict) or not matches(HEX, closure.get('graph_sha256'))
            or any(type(closure.get(key)) is not int or closure[key] < 1
                   for key in ('objects', 'registry_crates'))):
        raise ValueError('Nix evidence lacks the native rebuild and closure binding.')
    return candidate['generation']


def native_evidence(channel, report, candidate, version, digest, previous, predecessor, publisher, *, standalone=None):
    upgrade = previous is not None or standalone is not None
    archive = channel == 'apt' and upgrade and report.get('format') == 'PeerBrushAptArchivedUpgradeV1'
    action = ('StandaloneUpgrade' if standalone is not None else 'ArchivedUpgrade' if archive
              else 'Upgrade' if upgrade else 'Install')
    require_fields(report, dict(schema=1, format=f'PeerBrush{FORMATS[channel]}{action}V1',
        upgrade='verified' if upgrade else 'not-performed', release_ready=False, public_availability='unverified',
        user_files='preserved', mcp_registration='private-client-config-backed-up-and-reused',
        desktop_startup_documents=DESKTOP[channel]), 'Native receipt')
    workspace_evidence(report.get('workspace'))
    current = report.get('newer' if upgrade else 'candidate')
    require_fields(current, candidate, 'Native package')
    old = report.get('older')
    if upgrade:
        if standalone is not None:
            require_fields(old, dict(package_sha256=standalone), 'Standalone DEB baseline')
            require_fields(report, dict(newer_receipt_sha256=digest), 'Native upgrade definition')
            if (predecessor is not None or old['package_sha256'] == candidate['package_sha256']
                    or any(not matches(HEX, old.get(key)) for key in ('executable_sha256', 'source_sha256'))
                    or not matches(r'[0-9a-f]{40}', old.get('commit'))):
                raise ValueError('Standalone DEB upgrade requires the initial signed candidate and a distinct baseline.')
        elif channel in ('scoop', 'homebrew'):
            require_fields(old, dict(definition_sha256=previous), 'Previous package')
        else:
            require_fields(report, dict(older_receipt_sha256=previous, newer_receipt_sha256=digest),
                           'Native upgrade definitions')
        if channel == 'nix':
            old = old.get('inputs') if isinstance(old, dict) else None
        old_version = old.get('version') if isinstance(old, dict) else None
        if not matches(STABLE, old_version) or release_version_order(old_version) >= release_version_order(version):
            raise ValueError('Native upgrade lacks an earlier application version.')
    elif channel in ('apt', 'nix'):
        require_fields(report, dict(receipt_sha256=digest), 'Native install definition')

    if channel == 'scoop':
        require_fields(report, dict(install='verified', removal='verified', mcp='live-headless-bridge',
            archive_cache='verified', current_junction='verified', start_menu_and_shim='verified',
            desktop_launcher='rendered-frame-and-clean-close', shell_desktop=SHELL_DESKTOP,
            desktop_connect=DESKTOP_CONNECT,
            publisher_trust='unverified'), 'Scoop acceptance')
        if not matches(HEX, report.get('scoop_source_sha256')):
            raise ValueError('Scoop evidence lacks the pinned installer source.')
        if upgrade:
            require_fields(report, dict(previous_payload_preserved=True), 'Scoop upgrade')
    elif channel == 'homebrew':
        require_fields(report, dict(install='verified', removal='verified', mcp='live-stdio-bridge',
            archive_cache='verified', support_tree='verified', cask_state='verified', publisher_team_id=publisher,
            signing='native-Developer-ID-stapled-Gatekeeper', app='quarantined-LaunchServices-rendered-workspace'),
            'Homebrew acceptance')
        if not matches(r'[0-9a-f]{40}', report.get('brew_commit')):
            raise ValueError('Homebrew evidence lacks the pinned package manager commit.')
        if upgrade:
            require_fields(report, dict(previous_generation_retired=True), 'Homebrew upgrade')
    elif channel == 'apt':
        require_fields(report, dict(product='PeerBrush', platform='Ubuntu 24.04 amd64',
            primary_fingerprint=publisher, missing_key='refused', update_owner='LinuxPackage', purge='verified',
            initial_install=('standalone-deb-verified' if standalone is not None else
                             'historical-receipt-verified' if archive else 'verified')), 'APT acceptance')
        transport = report.get('transport')
        if transport == 'public-https':
            if (upgrade and not archive and standalone is None) or not isinstance(report.get('apt_https'), dict):
                raise ValueError('Native APT HTTPS evidence requires an install or verified local upgrade baseline.')
        elif transport != 'local-file' or 'apt_https' in report:
            raise ValueError('Native APT evidence does not identify a supported transport.')
        expected_predecessor = report.get('predecessor_receipt_sha256') if archive or not upgrade else previous
        if (predecessor != expected_predecessor
                or (predecessor is not None and not matches(HEX, predecessor))
                or (archive and (predecessor is None or not matches(HEX, report.get('historical_install_sha256'))))):
            raise ValueError('APT evidence does not bind the signed repository predecessor.')
    else:
        require_fields(report, dict(update_owner='NixPackage', removal='verified', mcp='live-headless-bridge',
            desktop_integration='profile-entry-icon-and-mime-verified',
            rollback='verified' if upgrade else 'not-performed'), 'Nix acceptance')
        profile = report.get('profile')
        generation = nix_generation(current)
        removal = report.get('removal_generation')
        if (profile not in ('private-local-flake', 'private-custom-profile')
                or not matches(HEX, report.get('tool_sha256'))
                or not matches(r'nix \(Nix\) 2\.35\.[0-9]+', report.get('tool_version'))
                or type(removal) is not int or removal <= generation
                or (upgrade and nix_generation(report['older']) >= generation)):
            raise ValueError('Nix evidence does not bind the expected profile lifecycle.')
        if not upgrade:
            require_fields(report, dict(install='verified'), 'Nix install')


def check(channel, definition, native, *, version, source_commit, publisher=None, previous=None, standalone=None):
    definition = Path(definition[0]), definition[1]
    if (channel not in FORMATS or not matches(STABLE, version) or not matches(r'[0-9a-f]{40}', source_commit)
            or not all(matches(HEX, digest) for digest in (definition[1], native[1]))
            or (previous is not None and (not matches(HEX, previous) or previous == definition[1]))
            or (standalone is not None and (channel != 'apt' or previous is not None or not matches(HEX, standalone)))
            or (channel == 'apt' and not matches(FINGERPRINT, publisher))
            or (channel == 'homebrew' and not matches(r'[A-Z0-9]{10}', publisher))
            or (channel in ('scoop', 'nix') and publisher is not None)):
        raise ValueError('Select independent candidate, native receipt and applicable publisher pins.')
    report = json.loads(read(native[0], 128 * 1024, native[1]), object_pairs_hook=unique_fields)
    if not isinstance(report, dict):
        raise ValueError('Native receipt must be an object.')
    https_evidence = report.get('apt_https') if channel == 'apt' else None
    candidate, predecessor, source, artifact_file = definition_binding(
        channel, definition, version, source_commit, publisher, https_evidence=https_evidence)
    native_evidence(channel, report, candidate, version, definition[1], previous, predecessor, publisher,
                    standalone=standalone)
    upgrade = previous is not None or standalone is not None
    result = dict(schema=1, format='PeerBrushPackageManagerNativeReadbackV1', channel=channel,
                  version=version, source_commit=source_commit, definition_receipt_sha256=definition[1],
                  native_receipt_sha256=native[1], native_lifecycle='upgrade' if upgrade else 'install',
                  desktop_startup_documents=DESKTOP[channel],
                  source_archive_sha256=source, artifact_file=artifact_file,
                  native_upgrade='verified' if upgrade else 'not-performed',
                  public_availability='unverified', release_ready=False)
    if channel == 'scoop':
        result['shell_desktop'] = SHELL_DESKTOP
        result['desktop_connect'] = DESKTOP_CONNECT
    if previous is not None:
        result['previous_definition_receipt_sha256'] = previous
    if standalone is not None:
        result['standalone_deb_sha256'] = standalone
    if publisher is not None:
        result['publisher'] = publisher
    if channel == 'apt':
        result['artifact_sha256'] = candidate['package_sha256']
        result['repository_predecessor_sha256'] = predecessor
        if https_evidence is not None:
            result['apt_https'] = https_evidence
    elif channel == 'nix':
        current = report['newer' if previous is not None else 'candidate']
        result['artifact_sha256'] = source
        result['profile'] = report['profile']
        result['native_rollback'] = report['rollback']
        result['nix_output'] = dict(flake_nar_sha256=current['flake_nar_sha256'],
                                    nar_sha256=current['nar_sha256'],
                                    closure_sha256=current['closure']['graph_sha256'])
    else:
        result['artifact_sha256'] = candidate['archive_sha256']
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--channel', required=True, choices=FORMATS)
    parser.add_argument('--version', required=True)
    parser.add_argument('--source-commit', required=True)
    for name in ('definition', 'native'):
        parser.add_argument('--' + name, nargs=2, metavar=('RECEIPT', 'SHA256'), required=True)
    parser.add_argument('--publisher', help='Approved APT fingerprint or Apple Developer ID team.')
    baseline = parser.add_mutually_exclusive_group()
    baseline.add_argument('--previous-definition-sha256', help='Require an upgrade from this independently selected receipt.')
    baseline.add_argument('--standalone-deb-sha256', help='Require an initial APT repository upgrade from this independently selected DEB.')
    args = parser.parse_args(argv)
    report = check(args.channel, args.definition, args.native, version=args.version, source_commit=args.source_commit,
                   publisher=args.publisher, previous=args.previous_definition_sha256, standalone=args.standalone_deb_sha256)
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
