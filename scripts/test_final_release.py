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
            policy_bytes = (ROOT / policy).read_bytes()
            if hasattr(self, 'release_policy'):
                policy_bytes = final.encode(self.release_policy)
            (instance.root / policy).write_bytes(policy_bytes)
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
        self.assertEqual((self.output / final.installer.STABLE_DESCRIPTOR).read_bytes(), self.descriptor.read_bytes())
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

    def test_stable_discovery_alias_is_immutable_exact_bytes_and_public_readback_cannot_omit_it(self):
        candidate = self.compose()
        alias = final.installer.STABLE_DESCRIPTOR
        descriptor_bytes = self.descriptor.read_bytes()
        path = self.output / alias
        self.assertEqual(path.read_bytes(), descriptor_bytes)
        altered = copy.deepcopy(candidate.manifest)
        next(row for row in altered['files'] if row['file'] == alias)['sha256'] = 'f' * 64
        with self.assertRaisesRegex(ValueError, 'exact immutable Windows descriptor'):
            final.validate_manifest(altered)
        missing = copy.deepcopy(candidate.manifest)
        missing['files'] = [row for row in missing['files'] if row['file'] != alias]
        with self.assertRaisesRegex(ValueError, 'exact immutable Windows descriptor'):
            final.validate_manifest(missing)
        path.chmod(0o600)
        path.write_bytes(descriptor_bytes + b'\n')
        with self.assertRaisesRegex(ValueError, 'changed after composition'):
            candidate.verify()
        path.write_bytes(descriptor_bytes)
        candidate.verify()
        from test_publish_release import Service
        service = Service(candidate.manifest)
        service.create('v' + candidate.manifest['version'], candidate.manifest['commit'], 'Fixture', 'Fixture')
        for selected in candidate.directory.iterdir():
            service.upload(17, selected)
        service.publish(17)
        def get(url, maximum, **options):
            if url.endswith('/' + alias):
                raise ValueError('Selected stable alias unavailable')
            return service.public_get(url, maximum, **options)
        with self.assertRaisesRegex(ValueError, 'Selected stable alias unavailable'):
            publish_release.public_readback(candidate.manifest, get)
        self.assertEqual(path.read_bytes(), descriptor_bytes)

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
            elif change == 'channel':
                selected['channels']['windows-direct']['file'], selected['channels']['windows-direct']['dependencies'][0] = (
                    selected['channels']['windows-direct']['dependencies'][0], selected['channels']['windows-direct']['file'])
            elif change == 'ready': selected['channels']['windows-direct']['ready'] = True
            else: selected['channels']['microsoft-store']['ready'] = True
            with self.subTest(change=change), self.assertRaises(ValueError):
                final.immutable(manifest, selected)

    def test_verified_local_msix_entrypoint_keeps_discovery_separate_and_legacy_readback_strict(self):
        manifest = self.compose().manifest
        entry = manifest['channels']['windows-direct']
        self.assertEqual((manifest['schema'], manifest['format']), (2, final.FORMAT))
        self.assertEqual(entry['file'], self.package.name)
        self.assertEqual(entry['dependencies'], [self.runtime.name])
        self.assertEqual(entry['discovery'], dict(versioned=self.descriptor.name, stable=final.installer.STABLE_DESCRIPTOR))
        self.assertEqual(entry['update_route'], 'verified-local')
        self.assertFalse(entry['ready'])
        legacy = copy.deepcopy(manifest)
        legacy.update(schema=1, format=final.LEGACY_FORMAT)
        legacy['channels']['windows-direct'] = dict(platform='Windows', file=self.descriptor.name,
            dependencies=[self.package.name, self.runtime.name], installed_acceptance='unverified', ready=False)
        self.assertEqual(final.validate_manifest(legacy), final.validate_manifest(manifest))
        self.assertEqual(final.windows_files(legacy), final.windows_files(manifest))
        with self.assertRaisesRegex(ValueError, 'immutable'):
            final.immutable(legacy, manifest)
        for change in ['old-schema', 'route', 'discovery-install', 'unknown-discovery', 'wrong-stable',
                       'runtime-entrypoint', 'duplicate-role', 'ready']:
            selected = copy.deepcopy(manifest)
            current = selected['channels']['windows-direct']
            if change == 'old-schema': selected['schema'] = 1
            elif change == 'route': current['update_route'] = 'appinstaller'
            elif change == 'discovery-install': current['file'] = self.descriptor.name
            elif change == 'unknown-discovery': current['discovery']['install'] = self.package.name
            elif change == 'wrong-stable': current['discovery']['stable'] = self.descriptor.name
            elif change == 'runtime-entrypoint': current['file'] = self.runtime.name
            elif change == 'duplicate-role': current['dependencies'] = [self.package.name]
            else: current['ready'] = True
            with self.subTest(change=change), self.assertRaises(ValueError):
                final.validate_manifest(selected)
        legacy['channels']['windows-direct']['update_route'] = 'verified-local'
        with self.assertRaises(ValueError):
            final.validate_manifest(legacy)

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
        selected_entry, selected_excluded = website.published(self.root, candidate.manifest, tag,
            candidate.manifest['commit'], dict(notes=['Reviewed fixture notes'], caution='Native acceptance remains incomplete.'),
            service.public_get, maintainer=test_deb.MAINTAINER)
        self.assertEqual((selected_entry, selected_excluded), (entry, excluded))
        self.assertEqual(entry['platforms'], [], 'Committed Store/DMG/DEB policy cannot be widened by final metadata')
        direct = next(r for r in excluded if r.get('file') == self.package.name)
        self.assertTrue(direct['asset_binding_verified'])
        self.assertFalse(direct['ready'] or direct['production_ready'] or direct['content_verified'])
        self.assertEqual(direct['installed_acceptance'], 'unverified')
        self.assertEqual(direct['public_availability'], 'verified-by-exact-public-byte-readback')
        self.assertTrue(any(r.get('reason') == 'store-public-availability-unverified' for r in excluded))
        import readback_site_release as readback
        self.assertNotIn('release-inventory.json', service.data)
        report = dict(version=tag, commit=candidate.manifest['commit'],
                      release_inventory_sha256=website.inventory_sha(candidate.manifest['base_inventory']))
        result = readback.downloads(self.root, report, entry, service.public_get)
        self.assertEqual(result, dict(status='verified', reason='public-bytes-source-checksums-match', verified_platforms=[]))
        self.assertFalse(any(channel['ready'] for channel in candidate.manifest['channels'].values()))

    def test_final_inventory_cli_reaches_real_public_consumer_and_exact_site_source_without_publication(self):
        import prepare_site_release as website
        from test_publish_release import Service
        candidate = self.compose()
        service = Service(candidate.manifest)
        tag, commit = 'v' + candidate.manifest['version'], candidate.manifest['commit']
        service.create(tag, commit, 'PeerBrush ' + candidate.manifest['version'], 'Reviewed fixture notes')
        for path in candidate.directory.iterdir(): service.upload(17, path)
        service.publish(17)
        previous = json.loads((ROOT / 'tests/fixtures/site-release-output.json').read_bytes())['files']
        # Independent site-content objects do not alter the source checkout or its index.
        objects = []
        for name in website.SITE_FILES:
            blob = website.git(self.root, 'hash-object', '-w', '--stdin', input=previous[name].encode()).decode().strip()
            objects.append(f'100644 blob {blob}\t{name}\n')
        tree = website.git(self.root, 'mktree', input=''.join(objects).encode()).decode().strip()
        tree = website.git(self.root, 'mktree', input=f'040000 tree {tree}\tpublic-site\n'.encode()).decode().strip()
        site_commit = website.git(self.root, '-c', 'user.name=Package Tests', '-c', 'user.email=package-tests@example.invalid',
                                  'commit-tree', tree, input=b'Synthetic reviewed site fixture\n').decode().strip()
        selected = self.fixture.base / 'selected-final-inventory.json'
        selected.write_bytes(final.encode(candidate.manifest))
        notes = self.fixture.base / 'selected-site-notes.json'
        notes.write_bytes(final.encode(dict(notes=['Reviewed fixture notes'], caution='Native acceptance remains incomplete.')))
        output = self.fixture.base / 'selected-site-output'
        args = ['--site-commit', site_commit, '--tag', tag, '--commit', commit,
                '--final-inventory', str(selected), '--final-inventory-sha256', digest(selected),
                '--notes', str(notes), '--output', str(output), '--maintainer', test_deb.MAINTAINER]
        original = website.prepare
        def prepare(*arguments, **options):
            return original(*arguments, **options, get=service.public_get)
        status = website.git(self.root, 'status', '--porcelain')
        mutations = list(service.mutations)
        with mock.patch.object(website, '__file__', str(self.root / 'scripts/prepare_site_release.py')), \
                mock.patch.object(website, 'prepare', side_effect=prepare) as consumer, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(website.main(args), 2)
        self.assertEqual(consumer.call_count, 1)
        self.assertEqual(consumer.call_args.args[1], site_commit)
        self.assertEqual(consumer.call_args.args[2], candidate.manifest)
        report = json.loads((output / 'website-output.json').read_bytes())
        self.assertEqual(report['site_commit'], site_commit)
        self.assertEqual(report['release_inventory_sha256'], website.inventory_sha(candidate.manifest['base_inventory']))
        self.assertFalse(report['site_update_prepared'] or report['deployment_performed'])
        self.assertEqual(report['files'], {})
        self.assertEqual(report['production_readiness'], 'not-assessed')
        self.assertEqual(report['store_public_availability'], 'unverified')
        self.assertTrue(any(r.get('reason') == 'store-public-availability-unverified' for r in report['excluded']))
        self.assertEqual({path.name for path in output.iterdir()}, {'website-output.json'})
        self.assertEqual(website.site_sources(self.root, site_commit), previous)
        self.assertEqual(website.git(self.root, 'rev-parse', 'HEAD').decode().strip(), commit)
        self.assertEqual(website.git(self.root, 'status', '--porcelain'), status)
        self.assertEqual(service.mutations, mutations)
        self.assertEqual(selected.read_bytes(), final.encode(candidate.manifest))
        self.assertFalse(any(channel['ready'] for channel in candidate.manifest['channels'].values()))
        for change in ['wrong-hash', 'noncanonical', 'duplicate', 'unknown-field', 'legacy-selector']:
            raw = final.encode(candidate.manifest)
            if change == 'noncanonical': raw = json.dumps(candidate.manifest).encode()
            elif change == 'duplicate': raw = b'{"schema":1,' + raw[1:]
            elif change == 'unknown-field': raw = final.encode(dict(candidate.manifest, ready=True))
            selected.write_bytes(raw)
            refused = output.with_name('site-refused-' + change)
            arguments = list(args)
            arguments[arguments.index('--output') + 1] = str(refused)
            arguments[arguments.index('--final-inventory-sha256') + 1] = '0' * 64 if change == 'wrong-hash' else digest(selected)
            if change == 'legacy-selector':
                position = arguments.index('--final-inventory-sha256')
                del arguments[position:position + 2]
                arguments[arguments.index('--final-inventory')] = '--inventory'
            with self.subTest(change=change), mock.patch.object(website, 'prepare') as forbidden, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit if change == 'legacy-selector' else ValueError):
                website.main(arguments)
            forbidden.assert_not_called()
            self.assertFalse(refused.exists())
        self.assertEqual(website.git(self.root, 'status', '--porcelain'), status)
        self.assertEqual(service.mutations, mutations)

    def test_selected_final_inventory_refuses_different_full_manifest_missing_final_and_changed_media(self):
        import prepare_site_release as website
        from test_publish_release import Service
        candidate = self.compose()
        service = Service(candidate.manifest)
        tag, commit = 'v' + candidate.manifest['version'], candidate.manifest['commit']
        service.create(tag, commit, 'PeerBrush ' + candidate.manifest['version'], 'Reviewed fixture notes')
        for path in candidate.directory.iterdir(): service.upload(17, path)
        service.publish(17)
        metadata, data = copy.deepcopy(service.current), service.data.copy()
        selected = copy.deepcopy(candidate.manifest)
        for change in ['different-full-manifest', 'noncanonical-remote', 'missing-final-with-legacy',
                       'malformed-final-with-legacy', 'unavailable-final', 'changed-media', 'unavailable-media']:
            service.current, service.data = copy.deepcopy(metadata), data.copy()
            if change == 'different-full-manifest':
                value = copy.deepcopy(candidate.manifest)
                sidecar = next(r for r in value['files'] if r['file'].endswith('.qualification.json'))
                sidecar['sha256'] = '0' * 64
                final.validate_manifest(value)
                self.assertEqual(value['base_inventory'], selected['base_inventory'])
                raw = final.encode(value)
                service.data[final.MANIFEST] = raw
                next(a for a in service.current['assets'] if a['name'] == final.MANIFEST).update(
                    size=len(raw), digest='sha256:' + hashlib.sha256(raw).hexdigest())
            elif change == 'noncanonical-remote':
                raw = json.dumps(selected).encode()
                service.data[final.MANIFEST] = raw
                next(a for a in service.current['assets'] if a['name'] == final.MANIFEST).update(
                    size=len(raw), digest='sha256:' + hashlib.sha256(raw).hexdigest())
            elif change in ['missing-final-with-legacy', 'malformed-final-with-legacy']:
                if change == 'missing-final-with-legacy':
                    service.current['assets'] = [a for a in service.current['assets'] if a['name'] != final.MANIFEST]
                    service.data.pop(final.MANIFEST)
                else:
                    service.data[final.MANIFEST] = b'{}'
                    next(a for a in service.current['assets'] if a['name'] == final.MANIFEST).update(
                        size=2, digest='sha256:' + hashlib.sha256(b'{}').hexdigest())
                legacy = final.encode(selected['base_inventory'])
                service.data['release-inventory.json'] = legacy
                service.current['assets'].append(dict(id=900, name='release-inventory.json', state='uploaded',
                    size=len(legacy), digest='sha256:' + hashlib.sha256(legacy).hexdigest()))
            elif change == 'unavailable-final': service.data.pop(final.MANIFEST)
            else:
                name = selected['channels']['windows-direct']['dependencies'][0]
                if change == 'unavailable-media': service.data.pop(name)
                else: service.data[name] = b'!' + service.data[name][1:]
            requested = []
            def get(url, limit, **options):
                requested.append(url)
                return service.public_get(url, limit, **options)
            error = website.Unavailable if change in ['unavailable-final', 'unavailable-media'] else ValueError
            message = ('independently selected final inventory' if change in ['different-full-manifest', 'noncanonical-remote']
                       else 'legacy fallback is not permitted' if change == 'missing-final-with-legacy' else None)
            with self.subTest(change=change), self.assertRaisesRegex(error, message or '.*'):
                website.published(self.root, selected, tag, commit,
                    dict(notes=['Reviewed fixture notes'], caution='Native acceptance remains incomplete.'),
                    get, maintainer=test_deb.MAINTAINER)
            self.assertNotIn(website.DOWNLOAD + tag + '/release-inventory.json', requested)
            self.assertEqual(selected, candidate.manifest)
            self.assertFalse(any(channel['ready'] for channel in selected['channels'].values()))

    def test_final_public_readback_rejects_changed_identity_metadata_and_unavailable_bytes_without_legacy_fallback(self):
        import prepare_site_release as website
        import readback_site_release as readback
        from test_publish_release import Service
        candidate = self.compose()
        tag = 'v' + candidate.manifest['version']
        service = Service(candidate.manifest)
        service.create(tag, candidate.manifest['commit'], 'PeerBrush ' + candidate.manifest['version'], 'Reviewed fixture notes')
        for path in candidate.directory.iterdir(): service.upload(17, path)
        service.publish(17)
        entry, _ = website.published(self.root, candidate.manifest['base_inventory'], tag,
            candidate.manifest['commit'], dict(notes=['Reviewed fixture notes'], caution='Native acceptance remains incomplete.'),
            service.public_get, maintainer=test_deb.MAINTAINER)
        report = dict(version=tag, commit=candidate.manifest['commit'],
                      release_inventory_sha256=website.inventory_sha(candidate.manifest['base_inventory']))
        metadata, data = copy.deepcopy(service.current), service.data.copy()
        for change in ['base-digest', 'source', 'version', 'schema', 'duplicate-key', 'digest', 'size',
                       'duplicate-asset', 'unavailable-final', 'missing-final', 'unavailable-media', 'changed-media', 'malformed-with-legacy']:
            service.current, service.data = copy.deepcopy(metadata), data.copy()
            selected = report.copy()
            asset = next(a for a in service.current['assets'] if a['name'] == final.MANIFEST)
            if change == 'base-digest': selected['release_inventory_sha256'] = '0' * 64
            elif change == 'source': selected['commit'] = '0' * 40
            elif change in ['version', 'schema', 'duplicate-key', 'malformed-with-legacy']:
                value = copy.deepcopy(candidate.manifest)
                if change == 'version': value['version'] = '0.2.1'
                elif change == 'schema': value['format'] = 'unsupported'
                raw = final.encode(value)
                if change == 'duplicate-key': raw = b'{"schema":1,' + raw[1:]
                if change == 'malformed-with-legacy':
                    raw = b'{}'
                    legacy = final.encode(candidate.manifest['base_inventory'])
                    service.data['release-inventory.json'] = legacy
                    service.current['assets'].append(dict(id=900, name='release-inventory.json', state='uploaded',
                        size=len(legacy), digest='sha256:' + hashlib.sha256(legacy).hexdigest()))
                service.data[final.MANIFEST] = raw
                asset.update(size=len(raw), digest='sha256:' + hashlib.sha256(raw).hexdigest())
            elif change == 'digest': asset['digest'] = 'sha256:' + '0' * 64
            elif change == 'size': asset['size'] += 1
            elif change == 'duplicate-asset': service.current['assets'].append(copy.deepcopy(asset))
            elif change == 'unavailable-final': service.data.pop(final.MANIFEST)
            elif change == 'missing-final':
                service.current['assets'].remove(asset)
                service.data.pop(final.MANIFEST)
            else:
                name = candidate.manifest['channels']['windows-direct']['dependencies'][0]
                if change == 'unavailable-media': service.data.pop(name)
                else: service.data[name] = b'!' + service.data[name][1:]
            requested = []
            def get(url, limit, **options):
                requested.append(url)
                return service.public_get(url, limit, **options)
            with self.subTest(change=change):
                if change == 'missing-final':
                    result = readback.downloads(self.root, selected, entry, get)
                    self.assertEqual(result, dict(status='unavailable', reason='public-release-inventory-unavailable', verified_platforms=[]))
                else:
                    error = website.Unavailable if change in ['unavailable-final', 'unavailable-media'] else ValueError
                    with self.assertRaises(error): readback.downloads(self.root, selected, entry, get)
                self.assertNotIn(website.DOWNLOAD + tag + '/release-inventory.json', requested)
                self.assertFalse(any(channel['ready'] for channel in candidate.manifest['channels'].values()))
                self.assertEqual(final.store_channel(), candidate.manifest['channels']['microsoft-store'])

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
