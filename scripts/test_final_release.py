"""Offline source/byte composition; synthetic signatures/native results grant no readiness."""
import copy
import contextlib
import io
import hashlib
import json
import zipfile
from pathlib import Path
import unittest
from unittest import mock

import final_release as final
import test_appinstaller
import test_deb
import test_dmg_handoff
import test_msix
import test_signed_msix
import test_windows_executable_signing as executable_fixtures
from package import digest
import publish_release

ROOT = Path(__file__).resolve().parents[1]


class Composition(unittest.TestCase):
    def setUp(self):
        original = test_deb.DebianPackage.setUp
        def combined(instance):
            original(instance)
            for suffix in ['AppxManifest.xml.in', *['Assets/' + name for name in final.msix.ASSETS]]:
                name = 'packaging/msix/' + suffix
                path = instance.root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes((ROOT / name).read_bytes())
                instance.fixture.policy['source'].append(name)
            policy = 'packaging/release-policy.json'
            (instance.root / policy).write_bytes((ROOT / policy).read_bytes())
            instance.fixture.policy['source'].append(policy)
            # final_package owns these generated outputs before later platform
            # builds; keep the version-2 source checkout clean without packaging them.
            with (instance.root / '.gitignore').open('a', encoding='utf-8') as stream:
                stream.write('/final-deb-stage/\n/peerbrush_0.2.0-1_amd64.deb\n')
            instance.fixture.save_policy()
            instance.fixture.commit()
            instance.fixture.require_provenance()
            instance.archive = instance.fixture.build('Linux')
            original_build = instance.fixture.build
            def structural_windows(platform):
                archive = original_build(platform)
                if platform != 'Windows':
                    return archive
                with zipfile.ZipFile(archive) as source:
                    metadata = {item.filename: item for item in source.infolist()}
                    entries = {name: source.read(name) for name in source.namelist()}
                name = next(name for name in entries if name.endswith('/peerbrush.exe'))
                entries[name] = executable_fixtures.executable(entries[name][128:])
                manifest = next(name for name in entries if name.endswith('/release.json'))
                release = json.loads(entries[manifest])
                release['executable_sha256'] = hashlib.sha256(entries[name]).hexdigest()
                entries[manifest] = final.encode(release)
                with zipfile.ZipFile(archive, 'w') as destination:
                    for name, data in entries.items():
                        destination.writestr(metadata[name], data)
                return archive
            instance.fixture.build = structural_windows
        self.fixture = test_dmg_handoff.Handoff(methodName='runTest')
        with mock.patch.object(test_deb.DebianPackage, 'setUp', combined):
            self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        f = self.fixture
        f.handoff()
        self.root, self.directory = f.root, f.output
        self.inventory = json.loads((self.directory / 'release-inventory.json').read_bytes())
        self.output = f.base / 'final-composition'
        self.direct = f.base / 'direct-inputs'
        self.direct.mkdir(mode=0o700)
        self.identity = dict(name='PeerBrush.DirectFixture', publisher='CN=PeerBrush Direct Fixture', publisher_display_name='PeerBrush fixture')
        identity = self.direct / 'identity.json'
        identity.write_bytes(final.encode(self.identity))
        windows = next(r for r in self.inventory['assets'] if r.get('platform') == 'Windows')
        payload, _ = final.msix.expected_payload(self.root, self.directory / windows['file'], self.identity, require_provenance=True)
        self.original_payload = dict(payload)
        self.original_executable = payload['peerbrush.exe']
        payload = final.msix.signed_executable_payload(payload,
            executable_fixtures.signed_executable(self.original_executable))
        self.unsigned = self.direct / 'PeerBrush-1.2.0.0-Windows-x64.msix'
        self.unsigned.write_bytes(test_msix.synthetic_package(payload))
        self.package = self.direct / 'PeerBrush-1.2.0.0-Windows-x64-signed-r1.msix'
        self.package.write_bytes(test_signed_msix.signed_package(self.unsigned.read_bytes()))
        self.runtime = self.direct / 'Microsoft.VCLibs.x64.Desktop.appx'
        self.runtime.write_bytes(test_appinstaller.runtime_bytes())
        descriptor_output = f.base / 'descriptor-candidate'
        self.descriptor = descriptor_output / 'PeerBrush-1.2.0.0-Windows-x64.appinstaller'
        urls = {key + '_uri': 'https://downloads.example.invalid/immutable/' + path.name
                for key, path in [('descriptor', self.descriptor), ('package', self.package), ('runtime', self.runtime)]}
        signtool, powershell = self.direct / 'signtool.exe', self.direct / 'pwsh.exe'
        signtool.write_bytes(b'not-a-real-sdk-executable')
        powershell.write_bytes(b'not-a-real-powershell-executable')
        license = self.direct / 'runtime-license.txt'
        license.write_text('Synthetic reviewed redistribution license')
        self.descriptor_native_calls = []
        def descriptor_native(tool, arguments, work, label, **kwargs):
            selected = kwargs['package'] if label == 'certificate' else Path(arguments[-1])
            self.descriptor_native_calls.append((label, selected.name))
            if label == 'signtool':
                self.assertEqual(arguments[:5], ['verify', '/pa', '/all', '/tw', '/q'])
                return b'synthetic native boundary; no operating-system trust'
            is_runtime = selected.name == self.runtime.name
            metadata = dict(schema=1, powershell=final.signed.POWERSHELL_VERSION, status=0, kind=1,
                subject=test_appinstaller.PUBLISHER if is_runtime else self.identity['publisher'],
                certificate_sha256='c' * 64 if is_runtime else 'a' * 64,
                timestamp_certificate_sha256='b' * 64, signers=1, timestamp_attributes=1, digest='sha256')
            return json.dumps(metadata).encode()
        with mock.patch.object(final.signed.sys, 'platform', 'win32'), \
                mock.patch.object(final.signed, '_command', side_effect=descriptor_native):
            prepared_descriptor = final.installer.prepare(self.root, self.directory / windows['file'],
                self.unsigned, self.package, self.runtime, identity=identity, runtime_license=license,
                commit=f.commit, version='0.2.0', package_family=final.msix.derived_package_family(self.identity),
                portable_sha256=windows['sha256'], unsigned_sha256=digest(self.unsigned), package_sha256=digest(self.package),
                certificate_sha256='a' * 64, runtime_sha256=digest(self.runtime), runtime_certificate_sha256='c' * 64,
                runtime_license_sha256=digest(license), **urls, signtool=signtool, signtool_sha256=digest(signtool),
                powershell=powershell, powershell_sha256=digest(powershell),
                work=f.base / 'descriptor-work', output=descriptor_output, require_executable_signature=True)
        self.assertEqual(prepared_descriptor, self.descriptor)
        self.assertEqual(self.descriptor_native_calls, [
            ('signtool', 'peerbrush.exe'), ('certificate', 'peerbrush.exe'),
            ('signtool', self.package.name), ('certificate', self.package.name),
            ('signtool', self.runtime.name), ('certificate', self.runtime.name)])
        self.windows = dict(package_family=final.msix.derived_package_family(self.identity), **urls)
        for key, path in [('unsigned', self.unsigned), ('package', self.package), ('runtime', self.runtime), ('descriptor', self.descriptor), ('identity', identity)]:
            self.windows[key], self.windows[key + '_sha256'] = path, digest(path)
        self.receipt = f.base / 'private-linux-installation.json'
        linux = next(r for r in self.inventory['assets'] if r.get('platform') == 'Linux' and r['file'].endswith('.zip'))
        self.receipt.write_bytes(final.encode(dict(schema=1, format='PeerBrushDebInstallationV1', product='PeerBrush',
            version='0.2.0', commit=f.commit, package=dict(file=f.deb.name, sha256=digest(f.deb)),
            executable_sha256=linux['executable_sha256'], source_sha256=linux['source_sha256'],
            target='Ubuntu 24.04 amd64', architecture='amd64', checks=final.readiness.DEB_INSTALLATION_CHECKS,
            limits=final.readiness.DEB_INSTALLATION_LIMITS, release_ready=False, production_readiness='not-assessed')))
        trust = dict(cli_version='2.102.0', workflow_commit=f.commit, run_id='12345', run_attempt='1', readback_job=True,
                     work=f.base / 'unused-native-work')
        trust_inputs = f.base / 'native-trust-inputs'
        trust_inputs.mkdir(mode=0o700)
        for key in ['bundle', 'trusted_root', 'cli']:
            path = trust_inputs / key
            path.write_bytes(('SYNTHETIC_UNTRUSTED_' + key).encode())
            trust[key], trust[key + '_sha256'] = path, digest(path)
        self.options = dict(commit=f.commit, version='0.2.0', maintainer=test_deb.MAINTAINER,
            macos=dict(qualification=f.evidence, qualification_sha256=digest(f.evidence), team_id=f.claims['publisher_team_id'], trust=trust),
            windows=self.windows, linux_receipt=self.receipt, linux_receipt_sha256=digest(self.receipt))
        self.origin = dict(native_producer_authentication='verified-pinned-workflow-oidc-attestation', native_job_conclusion='service-reconciled-success')
        self.sequence = 0

    def compose(self, **changes):
        self.sequence += 1
        selection = copy.deepcopy(dict(self.options, **changes))
        selection['macos']['trust']['work'] = self.fixture.base / ('native-auth-' + str(self.sequence))
        def authenticate(qualification, **inputs):
            work = Path(inputs['work'])
            work.mkdir(mode=0o700)
            for key, name in [('bundle', 'bundle.jsonl'), ('trusted_root', 'trusted_root.jsonl'), ('cli', 'gh')]:
                (work / name).write_bytes(Path(inputs[key]).read_bytes())
            (work / 'dmg-qualification.json').write_bytes(qualification)
            return self.origin
        with mock.patch('verify_macos_qualification.authenticate', side_effect=authenticate) as native:
            result = final.compose(self.root, self.directory, self.output, **selection)
        self.assertTrue(native.call_args.kwargs['readback_job'])
        return result

    def test_actual_content_consumers_compose_explicit_public_bytes_without_granting_native_or_store_authority(self):
        (self.directory / 'private-recovery.json').write_bytes(b'PRIVATE_CANARY_DO_NOT_COPY')
        (self.direct / 'signed-msix-qualification.json').write_bytes(b'{"production_ready":true}')
        originals = {path: path.read_bytes() for path in [self.receipt, self.unsigned, self.package, self.runtime, self.descriptor]}
        candidate = self.compose()
        candidate.verify()
        self.assertIs(candidate.assessment['production_ready'], False)
        self.assertEqual(candidate.assessment['linux_installation_binding'], 'verified')
        self.assertEqual(candidate.assessment['windows_native_signature'], 'not-authenticated-by-aggregate')
        self.assertIn('windows-native-final-producer-not-authenticated', candidate.blockers)
        self.assertIn('windows-executable-signing-and-installed-acceptance-not-established', candidate.blockers)
        self.assertEqual(candidate.manifest['channels']['microsoft-store'], final.store_channel())
        public = {r['file'] for r in candidate.manifest['files']} | {final.MANIFEST, 'SHA256SUMS'}
        self.assertEqual(public, {p.name for p in self.output.iterdir()})
        self.assertNotIn(self.unsigned.name, public)
        self.assertNotIn(self.receipt.name, public)
        for path in self.output.iterdir():
            self.assertNotIn(b'PRIVATE_CANARY', path.read_bytes())
        self.assertEqual(originals, {path: path.read_bytes() for path in originals})
        client = mock.Mock()
        report = publish_release.publish(candidate, client, 'Reviewed public notes', execute=True)
        self.assertEqual(report['phase'], 'blocked')
        client.assert_not_called()
        self.assertEqual(client.method_calls, [])
        with self.assertRaisesRegex(ValueError, 'JSON report'):
            publish_release.publish(dict(candidate.assessment, production_ready=True), client, 'notes', execute=True)

    def test_native_origin_failure_or_missing_platform_cannot_prepare_final_output(self):
        with mock.patch('verify_macos_qualification.authenticate', side_effect=ValueError('unverified native origin')):
            with self.assertRaisesRegex(ValueError, 'unverified native origin'):
                final.compose(self.root, self.directory, self.output, **self.options)
        self.assertFalse(self.output.exists())
        raw = (self.directory / 'release-inventory.json').read_bytes()
        bad = copy.deepcopy(self.inventory)
        bad['assets'].pop()
        (self.directory / 'release-inventory.json').write_bytes(final.encode(bad))
        try:
            with self.assertRaisesRegex(ValueError, 'complete six-record'):
                self.compose()
        finally:
            (self.directory / 'release-inventory.json').write_bytes(raw)
        self.assertFalse(self.output.exists())

    def test_reconstructed_descriptor_payload_and_independent_input_digests_are_required(self):
        for field in ['package_sha256', 'runtime_sha256', 'descriptor_sha256', 'unsigned_sha256']:
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'independent final digest'):
                self.compose(windows=dict(self.windows, **{field: '0' * 64}))
            self.assertFalse(self.output.exists())
        self.descriptor.write_bytes(self.descriptor.read_bytes().replace(b'<MainPackage', b'<UpdateSettings /><MainPackage'))
        with self.assertRaisesRegex(ValueError, 'exact shared descriptor'):
            self.compose(windows=dict(self.windows, descriptor_sha256=digest(self.descriptor)))
        self.assertFalse(self.output.exists())

    def test_direct_composition_refuses_changed_code_overlay_and_unsigned_executables(self):
        unsigned_before, package_before = self.unsigned.read_bytes(), self.package.read_bytes()
        with zipfile.ZipFile(self.unsigned) as archive:
            signed_executable = archive.read('peerbrush.exe')
        variants = [self.original_executable]
        for offset in [512, len(self.original_executable) - 1]:
            changed = bytearray(signed_executable)
            changed[offset] ^= 1
            variants.append(bytes(changed))
        for index, executable in enumerate(variants):
            if index == 0:
                payload = dict(self.original_payload)
            else:
                # Recompute ordinary content hashes/block maps to isolate the
                # original-byte PE binding, not a stale checksum failure.
                with zipfile.ZipFile(io.BytesIO(unsigned_before)) as archive:
                    payload = {name: archive.read(final.msix.opc_name(name)) for name in self.original_payload}
                payload['peerbrush.exe'] = executable
                release = json.loads(payload['release.json'])
                checksum = hashlib.sha256(executable).hexdigest()
                release['executable_sha256'] = checksum
                release['windows_executable_signing']['signed_sha256'] = checksum
                payload['release.json'] = (json.dumps(release, indent=2) + '\n').encode()
            self.unsigned.write_bytes(test_msix.synthetic_package(payload))
            self.package.write_bytes(test_signed_msix.signed_package(self.unsigned.read_bytes()))
            selection = dict(self.windows, unsigned_sha256=digest(self.unsigned), package_sha256=digest(self.package))
            try:
                with self.subTest(variant=index), self.assertRaisesRegex(ValueError, 'PE signature|PE signing'):
                    self.compose(windows=selection)
                self.assertFalse(self.output.exists())
            finally:
                self.unsigned.write_bytes(unsigned_before)
                self.package.write_bytes(package_before)

    def test_copy_mutation_is_detected_and_previous_output_is_never_replaced(self):
        original = final.bounded_file
        def mutate(path, limit, destination=None):
            result = original(path, limit, destination)
            if destination is not None and destination.parent == self.output and destination.name == self.runtime.name:
                destination.write_bytes(b'changed selected output')
            return result
        with mock.patch.object(final, 'bounded_file', side_effect=mutate), self.assertRaises(ValueError):
            self.compose()
        self.assertTrue(self.output.exists())
        before = {p.name: p.read_bytes() for p in self.output.iterdir()}
        with self.assertRaises(ValueError):
            self.compose()
        self.assertEqual(before, {p.name: p.read_bytes() for p in self.output.iterdir()})

    def test_final_manifest_versions_files_channels_and_disabled_store_are_immutable(self):
        manifest = self.compose().manifest
        final.immutable(manifest, copy.deepcopy(manifest))
        for change in ['source', 'bytes', 'missing', 'channel', 'ready', 'store']:
            selected = copy.deepcopy(manifest)
            if change == 'source': selected['commit'] = '0' * 40
            elif change == 'bytes': selected['files'][0]['sha256'] = '0' * 64
            elif change == 'missing': selected['files'].pop()
            elif change == 'channel': selected['channels']['windows-direct']['dependencies'].reverse()
            elif change == 'ready': selected['channels']['windows-direct']['ready'] = True
            else: selected['channels']['microsoft-store']['ready'] = True
            with self.subTest(change=change), self.assertRaises(ValueError):
                final.immutable(manifest, selected)

    def test_real_public_site_adapter_reads_all_final_bytes_but_never_selects_unverified_installer_or_store(self):
        import prepare_site_release as website
        from test_publish_release import Service
        candidate = self.compose()
        service = Service(candidate.manifest)
        tag = 'v' + candidate.manifest['version']
        service.create(tag, candidate.manifest['commit'], 'PeerBrush ' + candidate.manifest['version'], 'Reviewed fixture notes')
        for path in candidate.directory.iterdir(): service.upload(17, path)
        service.publish(17)
        entry, excluded = website.published(self.root, candidate.manifest['base_inventory'], tag,
            candidate.manifest['commit'], dict(notes=['Reviewed fixture notes'], caution='Native acceptance remains incomplete.'),
            service.public_get, maintainer=test_deb.MAINTAINER)
        self.assertEqual(entry['platforms'], [], 'Committed Store/DMG/DEB policy cannot be widened by final metadata')
        direct = next(r for r in excluded if r['file'] == self.descriptor.name)
        self.assertTrue(direct['asset_binding_verified'])
        self.assertFalse(direct['ready'] or direct['production_ready'] or direct['content_verified'])
        self.assertEqual(direct['installed_acceptance'], 'unverified')
        self.assertEqual(direct['public_availability'], 'verified-by-exact-public-byte-readback')
        self.assertTrue(any(r.get('reason') == 'store-public-availability-unverified' for r in excluded))

    def test_cli_recomposes_inputs_and_cannot_use_a_report_or_request_credentials_when_native_gates_block(self):
        candidate = self.compose()
        notes = self.fixture.base / 'reviewed-notes.txt'
        notes.write_text('Reviewed synthetic public notes', encoding='utf-8')
        args = ['--source-root', str(self.root), '--directory', str(self.directory), '--output', str(self.output),
                '--selection', 'selection.json', '--selection-sha256', 'a' * 64,
                '--notes', str(notes), '--notes-sha256', digest(notes), '--execute']
        with mock.patch.object(final, 'from_selection', return_value=candidate) as compose, \
                mock.patch.object(publish_release, 'GitHub') as credentials, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(publish_release.main(args), 2)
            compose.assert_called_once()
            credentials.assert_not_called()
        with contextlib.redirect_stderr(io.StringIO()), mock.patch.object(final, 'from_selection') as compose:
            with self.assertRaises(SystemExit): publish_release.main(args + ['--ready-report', 'untrusted.json'])
            compose.assert_not_called()


class StoreStatus(unittest.TestCase):
    def test_real_reconciler_statuses_preserve_delays_rejection_failure_and_unknown_public_availability(self):
        import store_release_state as state
        for service, category in state.STATUSES.items():
            observed = dict(service_state=category, public_availability='unverified')
            result = final.store_channel(observed)
            self.assertEqual(result['service_state'], category, service)
            self.assertFalse(result['submission_enabled'] or result['ready'])
            self.assertEqual(result['public_availability'], 'unverified')
        with self.assertRaises(ValueError):
            final.store_channel(dict(service_state='available', public_availability='verified'))
        with mock.patch('store_release_state.reconcile', return_value=dict(service_state='rejected')) as reconcile:
            client, journal = object(), object()
            result = final.observe_store(client, journal)
            reconcile.assert_called_once_with(client, journal)
            self.assertEqual(result['service_state'], 'rejected')
            self.assertFalse(result['ready'])


if __name__ == '__main__':
    unittest.main()
