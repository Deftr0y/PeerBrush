"""Regression source for public-byte-bound definitions; no real network or install."""
import base64
from contextlib import redirect_stderr
import io
import json
import pathlib
import tempfile
import unittest
from unittest import mock
import zipfile

import prepare_package_managers as managers
import prepare_site_release as release
import test_homebrew_upgrade as homebrew_qa
import test_nix_profile_upgrade as nix_qa
import test_prepare_site_release
import test_scoop_upgrade as scoop_qa
import test_sign_macos
import verify_package_manager_acceptance as native_readback
from test_homebrew_upgrade import select as select_homebrew
from test_scoop_upgrade import select as select_scoop


class AcceptanceSelection(unittest.TestCase):
    def test_scoop_cache_accepts_one_stock_name_and_requires_the_download_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = pathlib.Path(directory).resolve()
            version = '0.30.1'
            url = 'https://example.invalid/PeerBrush.zip'
            payload = b'authenticated-release'
            candidate = version, dict(url=url, sha256=release.sha(payload)), b'manifest'
            legacy = cache / f'peerbrush#{version}#https_example.invalid_PeerBrush.zip'
            current = cache / f'peerbrush#{version}#{release.sha(url.encode())[:7]}.zip'
            legacy.write_bytes(payload)
            scoop_qa.verify_cached_archive(cache, candidate)
            current.write_bytes(payload)
            with self.assertRaisesRegex(ValueError, 'one authenticated archive'):
                scoop_qa.verify_cached_archive(cache, candidate)
            legacy.unlink()
            scoop_qa.verify_cached_archive(cache, candidate)
            current.write_bytes(b'altered-release')
            with self.assertRaisesRegex(ValueError, 'one authenticated archive'):
                scoop_qa.verify_cached_archive(cache, candidate)

    def test_scoop_shim_and_shortcut_require_the_stable_route_without_arguments(self):
        launcher = pathlib.Path('Scoop fixture/apps/peerbrush/current/peerbrush.exe').absolute()
        original = f'path = "{launcher}"\r\n'.encode()
        scoop_qa.verify_shim_target(b'\xef\xbb\xbf' + original, launcher)
        with self.assertRaisesRegex(ValueError, 'stable launcher'):
            scoop_qa.verify_shim_target(original + b'args = --headless\r\n', launcher)
        versioned = launcher.parent.with_name('0.30.1') / launcher.name
        with self.assertRaisesRegex(ValueError, 'stable launcher'):
            scoop_qa.verify_shim_target(f'path = "{versioned}"\r\n'.encode(), launcher)

        shortcut = dict(target=str(launcher), directory=str(launcher.parent), arguments='')
        self.assertEqual(scoop_qa.verify_shortcut(json.dumps(shortcut), launcher), (launcher, launcher.parent))
        for changed in [dict(shortcut, directory=str(versioned.parent)), dict(shortcut, arguments='--headless')]:
            with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, 'working directory'):
                scoop_qa.verify_shortcut(json.dumps(changed), launcher)

    def test_homebrew_cask_readback_distinguishes_the_definition_from_the_installed_generation(self):
        version = '0.30.1'
        url = release.DOWNLOAD + f'v{version}/PeerBrush-signed.zip'
        checksum = 'a' * 64
        candidate = version, dict(url=url, sha256=checksum), b'cask'
        cask = dict(token='peerbrush', full_token=homebrew_qa.CASK, tap='peerbrush-qa/local',
                    version=version, installed=version, url=url, sha256=checksum)
        def verify(row, *, installed=True):
            homebrew_qa.verify_cask_state(json.dumps(dict(formulae=[], casks=[row])), candidate, installed=installed)
        verify(cask)
        verify(dict(cask, installed=None), installed=False)
        for changed in [dict(cask, installed='0.30.0'), dict(cask, sha256='b' * 64), dict(cask, tap='other/tap')]:
            with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, 'cask state differs'):
                verify(changed)
        with self.assertRaisesRegex(ValueError, 'cask state differs'):
            verify(cask, installed=False)

    def test_homebrew_document_launch_keeps_paths_outside_application_arguments(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-homebrew-launch-proof-') as directory:
            root = pathlib.Path(directory).resolve()
            state = root / 'Document workspace'
            state.mkdir()
            app = root / 'PeerBrush.app'
            files = [root / 'Artwork 作品.psd', root / 'Artwork 作品.psb']
            process = mock.Mock()
            process.poll.return_value = None
            env = dict(TMPDIR=str(root))

            def start(*args, **kwargs):
                (state / 'connection.json').write_text('{"pid":2345}', encoding='utf-8')
                return process

            def wait(*, timeout):
                process.poll.return_value = 0
                return 0

            process.wait.side_effect = wait
            with mock.patch.object(homebrew_qa.subprocess, 'Popen', side_effect=start) as spawn, \
                    mock.patch.object(homebrew_qa, 'command') as command:
                with homebrew_qa.launch_bundle(app, state, env, files) as alive:
                    alive()
                self.assertEqual(spawn.call_args.args[0],
                    ['/usr/bin/open', '-W', '-n', '-a', app, *files, '--args', '--state-dir', state])
                self.assertEqual(command.call_args_list, [
                    mock.call(['/usr/bin/osascript', '-l', 'JavaScript', '-e', homebrew_qa.QUIT, '2345', app, 'check'], env),
                    mock.call(['/usr/bin/osascript', '-l', 'JavaScript', '-e', homebrew_qa.QUIT, '2345', app, 'quit'], env),
                ])
                process.wait.assert_called_once_with(timeout=20)
                process.terminate.assert_not_called()

    def test_homebrew_qualification_requires_exact_cache_bytes_and_retires_whole_generations(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            cache = root / 'cache'
            cache.mkdir()
            archive = cache / 'signed archive.zip'
            archive.write_bytes(b'signed-release')
            checksum = release.sha(archive.read_bytes())
            homebrew_qa.verify_cached_archive(cache, str(archive), checksum)
            with self.assertRaisesRegex(ValueError, 'private download cache'):
                homebrew_qa.verify_cached_archive(cache, str(root / 'other.zip'), checksum)
            with self.assertRaisesRegex(ValueError, 'differs from the authenticated release'):
                homebrew_qa.verify_cached_archive(cache, str(archive), 'b' * 64)

            version = '0.30.1'
            support = root / 'share' / version
            notices = ['LICENSE', 'THIRD_PARTY_NOTICES.md', 'licenses/font/OFL.txt']
            source = f'PeerBrush-{version}-source.zip'
            for name in [*notices, source]:
                path = support / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
            asset = dict(support_files={name: release.sha(name.encode()) for name in notices},
                         source_sha256=release.sha(source.encode()))
            homebrew_qa.verify_support_tree(support, version, asset)
            extra = support / 'licenses' / 'extra.txt'
            extra.write_bytes(b'unreviewed')
            with self.assertRaisesRegex(ValueError, 'unexpected corresponding source or notice entry'):
                homebrew_qa.verify_support_tree(support, version, asset)
            caskroom = root / 'Caskroom'
            backup = caskroom / '.metadata' / (version + '.upgrading')
            backup.mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, 'retained staged files or metadata'):
                homebrew_qa.verify_retired(caskroom, support, version)
            backup.rmdir()
            with self.assertRaisesRegex(ValueError, 'retained corresponding source'):
                homebrew_qa.verify_retired(caskroom, support, version)
            empty = root / 'empty-version'
            empty.mkdir()
            homebrew_qa.verify_retired(caskroom, empty, version)

    def test_nix_store_closure_requires_valid_metadata_and_reachability(self):
        output = '/nix/store/' + 'a' * 32 + '-peerbrush-0.30.1'
        vendor = '/nix/store/' + 'b' * 32 + '-cargo-vendor-dir'
        source = '/nix/store/' + 'c' * 32 + '-peerbrush-source.zip'
        digest = 'sha256-' + base64.b64encode(b'a' * 32).decode('ascii')
        leaf = dict(narHash=digest, narSize=128, references=[])
        root = dict(leaf, references=[source, vendor])
        raw = {output: dict(root, registrationTime=123), vendor: leaf, source: leaf}
        graph = nix_qa.closure_graph(json.dumps(raw), output)
        self.assertEqual(graph[output], dict(root, references=[vendor, source]))
        self.assertEqual(graph, nix_qa.closure_graph(json.dumps(dict(reversed(list(raw.items())))), output))

        for invalid in [
            {output: root, vendor: leaf},
            dict(raw, **{output: dict(root, references=[vendor])}),
            dict(raw, **{vendor: dict(leaf, narHash='not-a-hash')}),
            dict(raw, **{source: dict(leaf, references=[{}])}),
        ]:
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                nix_qa.closure_graph(json.dumps(invalid), output)

    def test_nix_profile_requires_the_active_output_and_locked_channel(self):
        output = '/nix/store/' + 'a' * 32 + '-peerbrush-0.30.1'
        channel = 'path:/tmp/peerbrush-fixture/channel'
        locked = channel + '?narHash=fixture'
        entry = dict(active=True, attrPath='packages.x86_64-linux.peerbrush',
                     originalUrl=channel, url=locked, storePaths=[output])
        manifest = dict(version=3, elements={'peerbrush': entry})
        nix_qa.verify_profile(manifest, source=(channel, locked), output=output)
        for changed in [
            dict(entry, active=False),
            dict(entry, url=channel + '?narHash=older'),
            dict(entry, storePaths=[output, output]),
        ]:
            with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, 'expected PeerBrush output and flake'):
                nix_qa.verify_profile(dict(manifest, elements={'peerbrush': changed}),
                                      source=(channel, locked), output=output)
        with self.assertRaisesRegex(ValueError, 'exactly one package'):
            nix_qa.verify_profile(dict(manifest, elements={'peerbrush': entry, 'peerbrush-1': entry}),
                                  source=(channel, locked), output=output)
        with self.assertRaisesRegex(ValueError, 'removal left package records'):
            nix_qa.verify_profile(manifest)
        nix_qa.verify_profile(dict(manifest, elements={}))

    def test_first_install_and_upgrade_select_distinct_inputs_before_native_work(self):
        older = ['older', 'a' * 64]
        newer = ['newer', 'b' * 64]
        output = pathlib.Path('private-receipt.json')
        for harness, tool_args in [
            (scoop_qa, ['--scoop-archive', 'scoop.zip', '--scoop-sha256', 'c' * 64]),
            (homebrew_qa, ['--team', 'A123456789', '--brew-commit', 'd' * 40]),
            (nix_qa, ['--nix', 'nix', '--nix-sha256', 'e' * 64]),
        ]:
            common = ['--output', str(output), *tool_args]
            with self.subTest(harness=harness.__name__), mock.patch.object(harness, 'check') as check:
                harness.main([*common, '--install-only', *older])
                self.assertEqual(check.call_args.args, (older, None, output))
                check.reset_mock()
                harness.main([*common, '--older', *older, '--newer', *newer])
                self.assertEqual(check.call_args.args, (older, newer, output))
                check.reset_mock()
                for invalid in [
                    ['--older', *older],
                    ['--newer', *newer],
                    ['--install-only', *older, '--newer', *newer],
                    ['--install-only', *older, '--older', *older],
                ]:
                    with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as rejected:
                        harness.main([*common, *invalid])
                    self.assertEqual(rejected.exception.code, 2)
                    check.assert_not_called()


class NativeReadback(unittest.TestCase):
    def receipt(self, channel, format, **fields):
        return dict(schema=1, format=format, release_ready=False, public_availability='unverified',
            upgrade='not-performed', user_files='preserved',
            mcp_registration='private-client-config-backed-up-and-reused',
            desktop_startup_documents=native_readback.DESKTOP[channel],
            workspace=dict(tool_preferences_sha256='c' * 64, projects=[
                dict(format=extension, bit_depth=depth, file_sha256='d' * 64, rendered_sha256='e' * 64)
                for depth in (8, 16) for extension in ('psd', 'psb')
            ])) | fields

    def test_scoop_readback_requires_native_receipt_digest_and_exact_installed_archive(self):
        digest = 'a' * 64
        current = dict(version='0.30.1', definition_sha256=digest, archive_sha256='b' * 64, owner_observed=True)
        report = self.receipt('scoop', 'PeerBrushScoopInstallV1', candidate=current,
            install='verified', removal='verified', mcp='live-headless-bridge', archive_cache='verified',
            current_junction='verified', start_menu_and_shim='verified', desktop_launcher='rendered-frame-and-clean-close',
            shell_desktop=native_readback.SHELL_DESKTOP, desktop_connect=native_readback.DESKTOP_CONNECT,
            publisher_trust='unverified', scoop_source_sha256='f' * 64)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            receipt = root / 'native.json'
            definition = root / 'package-managers.json', digest
            raw = release.encode(report)
            receipt.write_bytes(raw)
            filename = 'PeerBrush-0.30.1-Windows-x64.zip'
            with mock.patch.object(native_readback, 'definition_binding', return_value=(current, None, 'c' * 64, filename)):
                options = dict(version=current['version'], source_commit='a' * 40)
                result = native_readback.check('scoop', definition, (receipt, release.sha(raw)), **options)
                self.assertEqual(result['native_lifecycle'], 'install')
                self.assertEqual(result['shell_desktop'], native_readback.SHELL_DESKTOP)
                self.assertEqual(result['desktop_connect'], native_readback.DESKTOP_CONNECT)
                self.assertEqual(result['source_archive_sha256'], 'c' * 64)
                self.assertEqual(result['artifact_file'], filename)
                self.assertIs(result['release_ready'], False)
                with self.assertRaises(ValueError):
                    native_readback.check('scoop', definition, (receipt, '0' * 64), **options)
                raw = release.encode(dict(report, candidate=dict(current, archive_sha256='f' * 64)))
                receipt.write_bytes(raw)
                with self.assertRaisesRegex(ValueError, 'Native package'):
                    native_readback.check('scoop', definition, (receipt, release.sha(raw)), **options)
                for missing in ('shell_desktop', 'desktop_connect'):
                    with self.subTest(missing=missing):
                        raw = release.encode({key: value for key, value in report.items() if key != missing})
                        receipt.write_bytes(raw)
                        with self.assertRaisesRegex(ValueError, 'Scoop acceptance'):
                            native_readback.check('scoop', definition, (receipt, release.sha(raw)), **options)

    def test_nix_upgrade_rejects_headless_runs_and_wrong_profile_generations(self):
        current_pin, old_pin = 'a' * 64, 'b' * 64
        inputs = dict(version='0.30.1')
        nar = 'sha256-' + 'A' * 43 + '='
        generation = dict(inputs=inputs, generation=2, flake_nar_sha256=nar, nar_sha256=nar,
                          closure=dict(objects=8, registry_crates=4, graph_sha256='c' * 64))
        report = self.receipt('nix', 'PeerBrushNixProfileUpgradeV1', upgrade='verified', rollback='verified',
            newer=generation, older=dict(generation, inputs=dict(version='0.30.0'), generation=1),
            newer_receipt_sha256=current_pin, older_receipt_sha256=old_pin, update_owner='NixPackage',
            removal='verified', removal_generation=3, mcp='live-headless-bridge',
            desktop_integration='profile-entry-icon-and-mime-verified', profile='private-custom-profile',
            tool_version='nix (Nix) 2.35.4', tool_sha256='d' * 64)

        def verify(value):
            native_readback.native_evidence('nix', value, dict(inputs=inputs), inputs['version'],
                                            current_pin, old_pin, None, None)

        verify(report)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            receipt = root / 'native.json'
            install = dict(report, format='PeerBrushNixProfileInstallV1', upgrade='not-performed',
                           install='verified', rollback='not-performed', receipt_sha256=current_pin,
                           candidate=generation)
            filename = 'PeerBrush-0.30.1-source.zip'
            with mock.patch.object(native_readback, 'definition_binding',
                                   return_value=(dict(inputs=inputs), None, 'd' * 64, filename)):
                for evidence, previous in [(install, None), (report, old_pin)]:
                    raw = release.encode(evidence)
                    receipt.write_bytes(raw)
                    result = native_readback.check('nix', (root / 'nix.json', current_pin), (receipt, release.sha(raw)),
                                                   version=inputs['version'], source_commit='a' * 40, previous=previous)
                    self.assertEqual(result['nix_output'], dict(flake_nar_sha256=nar, nar_sha256=nar,
                                                               closure_sha256='c' * 64))
                    self.assertEqual(result['artifact_file'], filename)
        for changed in [dict(report, desktop_startup_documents='not-performed'),
                        dict(report, older_receipt_sha256='e' * 64),
                        dict(report, removal_generation=2)]:
            with self.assertRaises(ValueError):
                verify(changed)

    def test_archived_apt_upgrade_checks_both_the_historical_and_current_predecessor_pins(self):
        current, previous, refreshed = 'a' * 64, 'b' * 64, 'c' * 64
        fingerprint = 'A' * 40
        candidate = dict(version='0.30.1', commit='f' * 40, package_sha256='e' * 64)
        report = self.receipt('apt', 'PeerBrushAptArchivedUpgradeV1', upgrade='verified',
            newer=candidate, older=dict(version='0.30.0'), newer_receipt_sha256=current,
            older_receipt_sha256=previous, historical_install_sha256='d' * 64, predecessor_receipt_sha256=refreshed,
            product='PeerBrush', platform='Ubuntu 24.04 amd64', transport='local-file', primary_fingerprint=fingerprint,
            missing_key='refused', update_owner='LinuxPackage', purge='verified', initial_install='historical-receipt-verified')
        native_readback.native_evidence('apt', report, candidate, '0.30.1', current, previous, refreshed, fingerprint)
        for old, predecessor in [(refreshed, refreshed), (previous, previous)]:
            with self.assertRaises(ValueError):
                native_readback.native_evidence('apt', report, candidate, '0.30.1', current, old, predecessor, fingerprint)

    def test_first_apt_release_can_upgrade_an_authenticated_standalone_deb(self):
        current, baseline = 'a' * 64, 'b' * 64
        fingerprint = 'A' * 40
        candidate = dict(version='0.4.0', commit='f' * 40, package_sha256='e' * 64)
        earlier = dict(version='0.30.1', commit='1' * 40, package_sha256=baseline,
                       executable_sha256='2' * 64, source_sha256='3' * 64)
        report = self.receipt('apt', 'PeerBrushAptStandaloneUpgradeV1', upgrade='verified',
            older=earlier, newer=candidate, newer_receipt_sha256=current,
            product='PeerBrush', platform='Ubuntu 24.04 amd64', transport='public-https', apt_https={},
            primary_fingerprint=fingerprint, missing_key='refused', update_owner='LinuxPackage',
            purge='verified', initial_install='standalone-deb-verified')
        native_readback.native_evidence('apt', report, candidate, '0.4.0', current, None, None, fingerprint,
                                         standalone=baseline)
        with self.assertRaisesRegex(ValueError, 'Standalone DEB baseline'):
            native_readback.native_evidence('apt', report, candidate, '0.4.0', current, None, None, fingerprint,
                                             standalone='c' * 64)
        with self.assertRaisesRegex(ValueError, 'initial signed candidate'):
            native_readback.native_evidence('apt', report, candidate, '0.4.0', current, None, 'd' * 64, fingerprint,
                                             standalone=baseline)

    def test_apt_clean_install_binds_current_repository_history_without_an_upgrade(self):
        current, predecessor = 'a' * 64, 'b' * 64
        fingerprint = 'A' * 40
        candidate = dict(version='0.30.1', commit='f' * 40, package_sha256='e' * 64)
        report = self.receipt('apt', 'PeerBrushAptInstallV1', candidate=candidate,
            receipt_sha256=current, predecessor_receipt_sha256=predecessor, product='PeerBrush',
            platform='Ubuntu 24.04 amd64', transport='local-file', primary_fingerprint=fingerprint,
            missing_key='refused', update_owner='LinuxPackage', purge='verified', initial_install='verified')
        native_readback.native_evidence('apt', report, candidate, '0.30.1', current, None, predecessor, fingerprint)
        for pin in [None, 'c' * 64]:
            with self.subTest(pin=pin), self.assertRaisesRegex(ValueError, 'signed repository predecessor'):
                native_readback.native_evidence('apt', dict(report, predecessor_receipt_sha256=pin),
                                                candidate, '0.30.1', current, None, predecessor, fingerprint)
        # First-generation receipts written before the new field remain valid.
        del report['predecessor_receipt_sha256']
        native_readback.native_evidence('apt', report, candidate, '0.30.1', current, None, None, fingerprint)

    def test_homebrew_readback_requires_the_selected_apple_team_and_native_signing(self):
        digest, publisher = 'a' * 64, 'TEAM123456'
        current = dict(version='0.30.1', definition_sha256=digest, archive_sha256='b' * 64, owner_observed=True)
        report = self.receipt('homebrew', 'PeerBrushHomebrewInstallV1', candidate=current, install='verified',
            removal='verified', mcp='live-stdio-bridge', archive_cache='verified', support_tree='verified',
            cask_state='verified', publisher_team_id=publisher, brew_commit='a' * 40,
            signing='native-Developer-ID-stapled-Gatekeeper', app='quarantined-LaunchServices-rendered-workspace')
        native_readback.native_evidence('homebrew', report, current, '0.30.1', digest, None, None, publisher)
        with self.assertRaisesRegex(ValueError, 'Homebrew acceptance'):
            native_readback.native_evidence('homebrew', report, current, '0.30.1', digest, None, None, 'TEAM654321')


class Definitions(unittest.TestCase):
    def setUp(self):
        self.fixture = test_prepare_site_release.Published(methodName='runTest')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-manager-test-')
        self.addCleanup(self.temporary.cleanup)
        self.output = pathlib.Path(self.temporary.name).resolve() / 'definitions'

    def prepare(self, **kwargs):
        f = self.fixture
        return managers.prepare(f.root, f.inventory, 'v0.2.0', f.commit, self.output, get=f.get, **kwargs)

    def test_actual_public_archives_define_paths_hashes_source_and_no_cleanup_hooks(self):
        f = self.fixture
        f.sign_macos_portable()
        report = self.prepare()
        scoop = json.loads((self.output / 'peerbrush.json').read_bytes())
        windows = next(r for r in f.inventory['assets'] if r.get('platform') == 'Windows')
        self.assertEqual(scoop['architecture'], {'64bit': dict(
            url=release.DOWNLOAD + 'v0.2.0/' + windows['file'],
            hash=windows['sha256'], extract_dir='PeerBrush-0.2.0-Windows-x64')})
        self.assertEqual(scoop['bin'], 'peerbrush.exe')
        self.assertEqual(scoop['license'], 'GPL-3.0-only')
        self.assertFalse({'persist', 'env_set', 'pre_install', 'post_install', 'uninstaller', 'autoupdate'} & set(scoop))
        receipt = self.output / 'package-managers.json'
        version, asset, selected = select_scoop((self.output, release.sha(receipt.read_bytes())), for_install=True)
        self.assertEqual(version, '0.2.0')
        self.assertEqual(json.loads(selected), scoop)
        for field in ['executable_sha256', 'source_sha256', 'binary_source_commit']:
            self.assertEqual(asset[field], windows[field])
        with zipfile.ZipFile(io.BytesIO(f.assets[windows['file']])) as archive:
            prefix = 'PeerBrush-0.2.0-Windows-x64/'
            support = {'LICENSE', 'THIRD_PARTY_NOTICES.md'}
            support.update(name.removeprefix(prefix) for name in archive.namelist()
                           if name.startswith(prefix + 'licenses/') and not name.endswith('/'))
            self.assertEqual(asset['support_files'], {name: release.sha(archive.read(prefix + name)) for name in support})
            installed = self.output.parent / 'installed'
            for name in {*support, f'PeerBrush-{version}-source.zip'}:
                path = installed / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(archive.read(prefix + name))
        managers.verify_installed_support(installed, version, asset)
        (installed / 'THIRD_PARTY_NOTICES.md').write_text('Changed notice', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'notices differ'):
            managers.verify_installed_support(installed, version, asset)
        cask = (self.output / 'peerbrush.rb').read_text(encoding='utf-8')
        mac = next(r for r in f.inventory['assets'] if r.get('platform') == 'macOS')
        self.assertIn('sha256 "' + mac['sha256'] + '"', cask)
        self.assertIn('url "' + release.DOWNLOAD + 'v0.2.0/' + mac['file'] + '"', cask)
        self.assertIn('depends_on arch: :arm64', cask)
        self.assertIn('depends_on :macos', cask)
        self.assertIn('app "PeerBrush-0.2.0-macOS-arm64/PeerBrush.app"', cask)
        self.assertIn('binary "#{appdir}/PeerBrush.app/Contents/MacOS/peerbrush"', cask)
        for name in ['PeerBrush-0.2.0-source.zip', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'licenses']:
            self.assertIn('target: "#{HOMEBREW_PREFIX}/share/peerbrush/0.2.0/' + name + '"', cask)
        homebrew = report['channels']['homebrew']['asset']
        receipt_hash = release.sha(receipt.read_bytes())
        self.assertEqual(select_homebrew((self.output, receipt_hash), test_sign_macos.TEAM),
                         ('0.2.0', homebrew, cask.encode('utf-8')))
        with self.assertRaisesRegex(ValueError, 'selected Apple publisher'):
            select_homebrew((self.output, receipt_hash), 'Z' * 10)
        self.assertIn('LICENSE', homebrew['support_files'])
        self.assertTrue(any(name.startswith('licenses/') for name in homebrew['support_files']))
        self.assertEqual(homebrew['native_publisher_signing'], 'developer-id-notarized')
        self.assertNotIn('zap', cask)
        self.assertNotIn('auto_updates', cask)
        self.assertFalse(report['published'])
        self.assertFalse(report['release_ready'])
        for channel in managers.CHANNELS:
            self.assertTrue(report['channels'][channel]['definition_prepared'])
            self.assertEqual(report['channels'][channel]['native_publisher_trust'], 'unverified')
            self.assertEqual(report['channels'][channel]['installed_acceptance'], 'not-performed')
            self.assertEqual(report['channels'][channel]['update_owner_acceptance'], 'not-performed')
        self.assertEqual(report['files'], {name: release.sha((self.output / name).read_bytes()) for name in ['peerbrush.json', 'peerbrush.rb']})

    def test_older_scoop_receipt_can_identify_catalog_bytes_but_cannot_qualify_installation(self):
        report = self.prepare(channels=('scoop',))
        del report['channels']['scoop']['asset']['support_files']
        raw = release.encode(report)
        (self.output / 'package-managers.json').write_bytes(raw)
        selection = self.output, release.sha(raw)
        self.assertEqual(select_scoop(selection)[2], (self.output / 'peerbrush.json').read_bytes())
        with self.assertRaisesRegex(ValueError, 'license and notice inventory'):
            select_scoop(selection, for_install=True)

    def test_missing_public_asset_never_invents_definition_or_download(self):
        f = self.fixture
        f.sign_macos_portable()
        del f.assets['PeerBrush-0.2.0-Windows-x64.zip']
        f.metadata()
        report = self.prepare()
        self.assertFalse(report['channels']['scoop']['definition_prepared'])
        self.assertIsNone(report['channels']['scoop']['asset'])
        self.assertFalse((self.output / 'peerbrush.json').exists())
        self.assertTrue((self.output / 'peerbrush.rb').is_file())

    def test_unsigned_macos_asset_withholds_only_the_homebrew_definition(self):
        report = self.prepare()
        self.assertTrue((self.output / 'peerbrush.json').is_file())
        self.assertFalse((self.output / 'peerbrush.rb').exists())
        self.assertIsNone(report['channels']['homebrew']['asset'])
        self.assertEqual(report['channels']['homebrew']['definition_unavailable_reason'],
                         'developer-id-notarized-archive-required')

    def test_late_verification_failure_cannot_leave_partial_definitions(self):
        f = self.fixture
        # Windows/macOS may already have been consumed when the last ZIP fails.
        f.assets['PeerBrush-0.2.0-macOS-arm64.zip'] += b'changed after packaging'
        f.metadata()
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_package_manager_consumer_does_not_bypass_website_distribution_policy(self):
        f = self.fixture
        f.installed_policy()
        f.sign_macos_portable()
        report = self.prepare()
        self.assertTrue(all(row['definition_prepared'] for row in report['channels'].values()))
        entry, _ = f.published()
        self.assertEqual(entry['platforms'], [])

    def test_homebrew_uses_bound_disk_image_with_its_root_app_and_source_tree(self):
        f = self.fixture
        f.final_dmg()
        report = self.prepare(channels=['homebrew'])
        asset = report['channels']['homebrew']['asset']
        cask = (self.output / 'peerbrush.rb').read_bytes()
        self.assertEqual(asset['file'], f.dmg_name)
        self.assertEqual(asset['sha256'], f.dmg_record['sha256'])
        self.assertEqual(asset['signed_zip'], f.qualification['signed_zip'])
        self.assertIn(b'app "PeerBrush.app"', cask)
        self.assertIn(b'artifact "Documentation and source/PeerBrush-0.2.0-source.zip"', cask)
        digest = release.sha((self.output / 'package-managers.json').read_bytes())
        self.assertEqual(select_homebrew((self.output, digest), test_sign_macos.TEAM), ('0.2.0', asset, cask))
        _, excluded = f.published()
        image = next(row for row in excluded if row.get('file') == f.dmg_name)
        self.assertTrue(image['asset_binding_verified'])
        self.assertFalse(image['ready'])
        self.assertFalse(report['release_ready'])

    def test_existing_output_is_immutable_and_channel_selection_is_explicit(self):
        report = self.prepare(channels=['scoop'])
        self.assertEqual(set(report['channels']), {'scoop'})
        self.assertFalse((self.output / 'peerbrush.rb').exists())
        before = {p.name: p.read_bytes() for p in self.output.iterdir()}
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertEqual(before, {p.name: p.read_bytes() for p in self.output.iterdir()})
        for channels in [[], ['scoop', 'scoop'], ['apt'], ['nix']]:
            with self.subTest(channels=channels), self.assertRaises(ValueError):
                self.prepare(channels=channels)

    def test_cli_requires_independent_inventory_digest_and_forwards_exact_selection(self):
        f = self.fixture
        selected = pathlib.Path(self.temporary.name).resolve() / 'inventory.json'
        raw = release.encode(f.inventory)
        selected.write_bytes(raw)
        args = ['--inventory', str(selected), '--inventory-sha256', release.sha(raw),
                '--tag', 'v0.2.0', '--commit', f.commit, '--output', str(self.output), '--channel', 'scoop']
        with mock.patch.object(managers, 'prepare', return_value={'channels': {'scoop': {'definition_prepared': True}}}) as prepare:
            self.assertEqual(managers.main(args), 0)
        self.assertEqual(prepare.call_args.args[1:], (f.inventory, 'v0.2.0', f.commit, self.output, ['scoop']))
        selected.write_bytes(raw + b' ')
        with mock.patch.object(managers, 'prepare') as prepare, self.assertRaises(ValueError):
            managers.main(args)
        prepare.assert_not_called()


if __name__ == '__main__':
    unittest.main()
