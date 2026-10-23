"""Offline exact Sparkle payload tests; native commands are synthetic doubles."""
import copy
import base64
import contextlib
import hashlib
import io
import json
import os
import pathlib
import plistlib
import subprocess
import stat
import struct
import tempfile
import unittest
from unittest import mock
import zipfile

import package
import sparkle_bundle as bundle
import sign_macos


class Bundle(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-sparkle-bundle-')
        self.addCleanup(self.temporary.cleanup)
        self.base = pathlib.Path(self.temporary.name)
        self.pin = bundle.pin()
        self.sources = {bundle.PIN_NAME: json.dumps(self.pin).encode()}
        self.files = []
        for name, value in self.pin['framework_entries'].items():
            if value['kind'] == 'directory':
                continue
            data = value['target'].encode() if value['kind'] == 'symlink' else b'SYNTHETIC:' + name.encode()
            if value['kind'] == 'file':
                value['bytes'] = len(data)
                value['sha256'] = hashlib.sha256(data).hexdigest()
            self.files.append((bundle.PREFIX + name, data, 0o120777 if value['kind'] == 'symlink' else value['mode']))
        self.sources[bundle.PIN_NAME] = json.dumps(self.pin).encode()
        self.archive = self.base / 'candidate.zip'
        package.write_zip(self.archive, [('PeerBrush-fixture/' + n, d, m) for n, d, m in self.files], 0)

    def inspect(self, signed=False):
        with zipfile.ZipFile(self.archive) as source:
            return bundle.inspect_members(source, 'PeerBrush-fixture/', self.sources, signed)

    def damage(self, name, *, data=None, mode=None, remove=False):
        result = []
        for path, old_data, old_mode in self.files:
            if path == bundle.PREFIX + name:
                if remove:
                    continue
                old_data = old_data if data is None else data
                old_mode = old_mode if mode is None else mode
            result.append(('PeerBrush-fixture/' + path, old_data, old_mode))
        # Malicious fixtures must reach the inspector independently of the
        # production writer, which correctly refuses these links before output.
        with zipfile.ZipFile(self.archive, 'w') as archive:
            for path, data, mode in result:
                entry = zipfile.ZipInfo(path)
                entry.create_system = 3
                entry.external_attr = (mode if stat.S_IFMT(mode) else stat.S_IFREG | mode) << 16
                archive.writestr(entry, data)

    def test_exact_unsigned_inventory_and_nine_link_types(self):
        names = self.inspect()
        self.assertEqual(names, {name for name, _, _ in self.files})
        with zipfile.ZipFile(self.archive) as source:
            self.assertEqual(sum((item.external_attr >> 16) & 0o170000 == 0o120000 for item in source.infolist()), 9)

    def test_every_link_rejects_replacement_escaping_and_file_substitution(self):
        for name, value in self.pin['framework_entries'].items():
            if value['kind'] != 'symlink':
                continue
            for change in [dict(data=b'../../../../escape'), dict(data=b'/tmp/escape'), dict(mode=0o644)]:
                with self.subTest(name=name, change=change):
                    self.damage(name, **change)
                    with self.assertRaises(ValueError):
                        self.inspect()

    def test_missing_member_and_changed_unaffected_resource_refuse(self):
        for name, change in [('Versions/B/Sparkle', dict(remove=True)),
                             ('Versions/B/Headers/SPUUpdaterDelegate.h', dict(data=b'changed'))]:
            with self.subTest(name=name):
                self.damage(name, **change)
                with self.assertRaises((KeyError, ValueError)):
                    self.inspect(signed=True)

    def test_transformed_allowance_is_only_five_code_files_and_four_existing_seals(self):
        self.assertEqual(len(bundle.CODE), 5)
        self.assertEqual(len(bundle.SEALS), 4)
        self.damage('Versions/B/Sparkle', data=b'SYNTHETIC_RE_SIGNED_CODE')
        self.inspect(signed=True)  # Structure only; native verification is separate.
        with self.assertRaises(ValueError):
            self.inspect(signed=False)
        self.damage('Versions/B/Sparkle', mode=0o644)
        with self.assertRaises(ValueError):
            self.inspect(signed=True)

    def test_zip_writer_rejects_unknown_or_altered_symlink_before_output(self):
        for name, target in [('private/link', b'ignored'), (bundle.PREFIX + 'Versions/Current', b'/tmp')]:
            output = self.base / 'refused.zip'
            with self.assertRaises(ValueError):
                package.write_zip(output, [(name, target, 0o120777)], 0)
            self.assertFalse(output.exists())

    def test_disabled_plist_has_no_feed_key_or_raised_app_minimum(self):
        info = package.macos_info('0.30.0-dev.1', sparkle=True, check_layout=True)
        self.assertEqual(info['LSMinimumSystemVersion'], '11.0')
        self.assertNotIn('SUFeedURL', info)
        self.assertNotIn('SUPublicEDKey', info)
        for key, value in bundle.DISABLED_INFO.items():
            self.assertEqual(info[key], value)
        self.assertEqual(info['SUSignedFeedFailureExpirationInterval'], 0)
        self.assertNotIn('SUSignedFeedFailureExpiration', info)
        historical = package.macos_info('0.30.0-dev.1', sparkle=True)
        self.assertEqual(historical['SUSignedFeedFailureExpiration'], 0)
        self.assertNotIn('SUSignedFeedFailureExpirationInterval', historical)
        self.assertNotIn('SUEnableAutomaticChecks', package.macos_info('0.2.0'))

    def test_manual_check_public_pins_fail_closed_and_preserve_historical_contract(self):
        sources = {bundle.PIN_NAME: json.dumps(self.pin).encode(), 'src/sparkle.rs': bundle.CHECK_MARKER,
                   **{name: b'// Synthetic source only' for name in bundle.CHECK_SOURCES}}
        missing = dict(schema=1, feed_url=None, public_ed_key=None, publisher_team_id=None)
        sources[bundle.CHECK_CONFIGURATION] = json.dumps(missing).encode()
        self.assertIsNone(bundle.check_pins(sources, 'development'))
        historical = {bundle.PIN_NAME: sources[bundle.PIN_NAME], 'src/sparkle.rs': b'pub const BUNDLED_LAYOUT: u32 = 1;'}
        self.assertIsNone(bundle.check_pins(historical, '0.2.0'))
        pins = dict(schema=1, feed_url='https://peerbrush.com/updates/macos/stable/appcast.xml',
                    public_ed_key=base64.b64encode(bytes([1]) * 32).decode(), publisher_team_id='ABCD123456')
        sources[bundle.POLICY_NAME] = json.dumps(dict(schema=1, archive_sha256=self.pin['archive_sha256'],
            objects={name: dict(identifier='org.synthetic.helper', unsigned_sha256='a' * 64, entitlements={})
                     for name, _ in bundle.OBJECTS})).encode()
        for change in [dict(public_ed_key=None), dict(feed_url='https://evil.invalid/appcast.xml'),
                       dict(public_ed_key=base64.b64encode(bytes(32)).decode()), dict(publisher_team_id='wrong')]:
            sources[bundle.CHECK_CONFIGURATION] = json.dumps(pins | change).encode()
            with self.subTest(change=change), self.assertRaises(ValueError):
                bundle.check_pins(sources, '0.30.0')
        sources[bundle.CHECK_CONFIGURATION] = json.dumps(pins).encode()
        with self.assertRaises(ValueError):
            bundle.check_pins(sources, '0.30.0-dev.1')
        self.assertEqual(bundle.check_pins(sources, '0.30.0'), pins)
        info = package.macos_info('0.30.0', sparkle=True, check_pins=pins, check_layout=True)
        self.assertEqual(info['SUFeedURL'], pins['feed_url'])
        self.assertEqual(info['SUPublicEDKey'], pins['public_ed_key'])
        self.assertEqual(info['LSMinimumSystemVersion'], '11.0')
        for key, value in bundle.DISABLED_INFO.items():
            self.assertEqual(info[key], value)
        del sources[bundle.POLICY_NAME]
        with self.assertRaises(KeyError):
            bundle.check_pins(sources, '0.30.0')

    def test_committed_unreviewed_policy_blocks_before_any_native_signing(self):
        with self.assertRaisesRegex(ValueError, 'awaits independent review'):
            bundle.reviewed_policy()
        app = self.base / 'PeerBrush.app'
        (app / 'Contents/Frameworks/Sparkle.framework').mkdir(parents=True)
        native = mock.Mock()
        with self.assertRaisesRegex(ValueError, 'awaits independent review'):
            bundle.sign_nested(app, 'A' * 40, None, native)
        native.assert_not_called()

    def test_nested_signing_order_identity_timestamp_and_precise_entitlement_retention(self):
        app = self.base / 'PeerBrush.app'
        (app / 'Contents/Frameworks/Sparkle.framework').mkdir(parents=True)
        policy = {name: dict(identifier='org.synthetic.' + str(index), entitlements={}, unsigned_sha256='0' * 64)
                  for index, (name, _) in enumerate(bundle.OBJECTS)}
        calls = []
        with mock.patch.object(bundle, 'reviewed_policy', return_value=policy):
            bundle.sign_nested(app, 'A' * 40, self.base / 'existing.keychain-db', calls.append)
        self.assertEqual([str(app / 'Contents/Frameworks/Sparkle.framework' / name) for name, _ in bundle.OBJECTS],
                         [call[-1] for call in calls])
        for index, call in enumerate(calls):
            self.assertIn('--timestamp', call)
            self.assertIn('runtime', call)
            self.assertIn('A' * 40, call)
            self.assertNotIn('--deep', call)
            self.assertEqual('--preserve-metadata=entitlements' in call, index == 1)

    def test_native_normalization_changes_only_owned_copy_and_requires_exact_result(self):
        original = self.base / 'original'
        original.write_bytes(b'SYNTHETIC_CODE:SIGNATURE')
        owned = self.base / 'owned'
        owned.mkdir()
        calls = []
        def native(command):
            calls.append(command)
            self.assertEqual(command[:2], ['/usr/bin/codesign', '--remove-signature'])
            self.assertTrue(pathlib.Path(command[-1]).is_relative_to(owned))
            pathlib.Path(command[-1]).write_bytes(b'SYNTHETIC_CODE')
        self.assertEqual(bundle.normalized_hash(original, native, owned), hashlib.sha256(b'SYNTHETIC_CODE').hexdigest())
        self.assertEqual(original.read_bytes(), b'SYNTHETIC_CODE:SIGNATURE')
        self.assertEqual(len(calls), 1)

    def test_inventory_and_hash_walks_never_materialize_regular_payload_files(self):
        root = self.base / 'streamed-payload'
        root.mkdir()
        selected = root / 'public-file'
        selected.write_bytes(b'SYNTHETIC_PUBLIC_PAYLOAD' * 1024)
        expected = package.digest(selected)
        real_read = pathlib.Path.read_bytes
        def read(path):
            if path.is_relative_to(root):
                raise AssertionError('Inventory/hash walk must stream regular payload files')
            return real_read(path)
        with mock.patch.object(pathlib.Path, 'read_bytes', read):
            self.assertEqual(sign_macos.tree_files(root), {'public-file': selected})
            self.assertEqual(sign_macos.tree_hashes(root), {'public-file': expected})

    def test_finalization_hashes_existing_payload_once_without_quadratic_walks(self):
        import test_sign_macos
        fixture = test_sign_macos.Signing(methodName='test_success_final_hashes_source_preservation_and_native_validation')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        real_hashes = sign_macos.tree_hashes
        with mock.patch.object(sign_macos, 'tree_hashes', wraps=real_hashes) as hashes:
            fixture.sign()
        finalized = [call.args[0] for call in hashes.call_args_list
                     if call.args[0].name == 'payload' and call.args[0].parent.name.startswith('.finalize-')]
        self.assertEqual(len(finalized), 1)

    def test_native_facts_preserve_actual_exact_entitlements_without_private_output(self):
        actual = {'com.apple.security.inherit': True}
        def native(command):
            if '--entitlements' in command:
                return subprocess.CompletedProcess(command, 0, plistlib.dumps(actual).decode(), '')
            return subprocess.CompletedProcess(command, 0, '', 'Identifier=org.synthetic.helper\n')
        identifier, entitlements, _ = bundle.native_facts(self.base / 'object', native)
        self.assertEqual(identifier, 'org.synthetic.helper')
        self.assertEqual(entitlements, actual)

    @unittest.skipIf(os.name == "nt", "Exact Unix link/mode round trip runs on native macOS/Linux")
    def test_extraction_restores_exact_links_and_rejects_arbitrary_injection(self):
        destination = self.base / 'restored'
        try:
            sign_macos.extract_archive(self.archive, destination)
        except OSError as error:
            if os.name == 'nt' and getattr(error, 'winerror', None) == 1314:
                self.skipTest('Windows cannot create actual filesystem links without the required privilege')
            raise
        root = destination / 'PeerBrush-fixture'
        values = bundle.disk_entries(root)
        self.assertEqual(set(values), {name for name, _, _ in self.files})
        for name, data, mode in self.files:
            actual, actual_mode, target = values[name]
            self.assertEqual(actual_mode, mode)
            self.assertEqual(target if target is not None else actual.read_bytes(), data)
        link = root / 'private'
        link.symlink_to('PeerBrush.app')
        with self.assertRaises(ValueError):
            bundle.disk_entries(root)

    def test_policy_candidate_json_is_never_a_reviewed_policy(self):
        (self.base / 'packaging/sparkle').mkdir(parents=True)
        (self.base / bundle.POLICY_NAME).write_text(json.dumps(dict(schema=1, archive_sha256=self.pin['archive_sha256'],
            objects={}, authority='review-candidate-only', production_ready=False)))
        with self.assertRaises(ValueError):
            bundle.reviewed_policy(self.base)

    def test_actual_package_and_source_inspector_require_complete_new_layout(self):
        import test_package
        import verify_release
        fixture = test_package.Packages(methodName='test_platform_architecture_source_license_and_macos_bundle_match')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        notice = (pathlib.Path(__file__).resolve().parents[1] / 'packaging/sparkle/LICENSE').read_text(encoding='utf-8').encode('utf-8')
        selected = copy.deepcopy(self.pin)
        upstream = self.base / 'upstream-synthetic.zip'
        with zipfile.ZipFile(upstream, 'x') as archive:
            for name, item in selected['framework_entries'].items():
                member = selected['framework_prefix'] + ('' if name == '.' else name)
                if item['kind'] == 'directory':
                    if not member.endswith('/'):
                        member += '/'
                    data, mode = b'', stat.S_IFDIR | item['mode']
                elif item['kind'] == 'symlink':
                    data, mode = item['target'].encode(), stat.S_IFLNK | 0o777
                else:
                    data = b'SYNTHETIC:' + name.encode()
                    mode = stat.S_IFREG | item['mode']
                info = zipfile.ZipInfo(member)
                info.create_system, info.external_attr = 3, mode << 16
                archive.writestr(info, data)
            archive.writestr('LICENSE', notice)
        selected.update(archive_bytes=upstream.stat().st_size, archive_sha256=package.digest(upstream))
        for name, data in {
            'scripts/sparkle_bundle.py': b'# Synthetic source sentinel, never executed\n',
            bundle.PIN_NAME: json.dumps(selected).encode(),
            bundle.POLICY_NAME: b'{"schema":1,"objects":null}\n',
            'packaging/sparkle/LICENSE': notice,
            'src/sparkle.rs': b'pub const BUNDLED_LAYOUT: u32 = 1;\n' + bundle.CHECK_MARKER,
            **{name: b'// Synthetic source sentinel, never executed\n' for name in bundle.CHECK_SOURCES},
            bundle.CHECK_CONFIGURATION: b'{"schema":1,"feed_url":null,"public_ed_key":null,"publisher_team_id":null}',
        }.items():
            target = fixture.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            fixture.policy['source'].append(name)
        fixture.save_policy()
        fixture.commit()
        header = bytearray(128)
        header[:8] = b'\xcf\xfa\xed\xfe' + struct.pack('<I', 0x100000c)
        binary = fixture.root / 'synthetic-binary'
        binary.write_bytes(header)
        args = (fixture.root, binary, fixture.output, fixture.root / 'registry', 'macOS')
        # Real writer/inspector must preserve the exact old ignored-key layout
        # when inspecting historical source without the new capability marker.
        marker = (fixture.root / 'src/sparkle.rs').read_bytes()
        (fixture.root / 'src/sparkle.rs').write_bytes(b'pub const BUNDLED_LAYOUT: u32 = 1;')
        fixture.commit()
        with contextlib.redirect_stdout(io.StringIO()):
            historical = package.package(*args, verify_binary=False, rust_docs=fixture.rust_docs, sparkle_archive=upstream)
        old_commit = package.git(fixture.root, 'rev-parse', 'HEAD').decode().strip()
        old = verify_release.inspect_archive(historical, old_commit, '0.2.0', fixture.root)
        with zipfile.ZipFile(historical) as zipped:
            info = plistlib.loads(zipped.read(old['prefix'] + 'PeerBrush.app/Contents/Info.plist'))
        self.assertEqual(info['SUSignedFeedFailureExpiration'], 0)
        self.assertNotIn('SUSignedFeedFailureExpirationInterval', info)
        (fixture.root / 'src/sparkle.rs').write_bytes(marker)
        fixture.commit()
        with self.assertRaisesRegex(ValueError, 'explicitly selected pinned'):
            package.package(*args, verify_binary=False, rust_docs=fixture.rust_docs)
        with contextlib.redirect_stdout(io.StringIO()):
            archive = package.package(*args, verify_binary=False, rust_docs=fixture.rust_docs, sparkle_archive=upstream)
        commit = package.git(fixture.root, 'rev-parse', 'HEAD').decode().strip()
        result = verify_release.inspect_archive(archive, commit, '0.2.0', fixture.root)
        self.assertEqual(result['manifest']['macos_sparkle_layout'], 1)
        with zipfile.ZipFile(archive) as zipped:
            prefix = result['prefix']
            info = plistlib.loads(zipped.read(prefix + 'PeerBrush.app/Contents/Info.plist'))
            self.assertEqual(info['SUSignedFeedFailureExpirationInterval'], 0)
            self.assertNotIn('SUSignedFeedFailureExpiration', info)
            self.assertEqual(zipped.read(prefix + bundle.NOTICE), notice)
            self.assertEqual(zipped.read(prefix + 'PeerBrush.app/Contents/Resources/' + bundle.NOTICE), notice)
            self.assertEqual(sum(stat.S_ISLNK(item.external_attr >> 16) for item in zipped.infolist()), 9)
            original = [(item.filename, zipped.read(item), item.external_attr >> 16) for item in zipped.infolist()]
        # Source capability, not the global helper revision, selects the exact
        # new plist. Replacing it with the historical ignored key must refuse.
        replaced = dict(info)
        replaced['SUSignedFeedFailureExpiration'] = replaced.pop('SUSignedFeedFailureExpirationInterval')
        info_name = result['prefix'] + 'PeerBrush.app/Contents/Info.plist'
        package.write_zip(archive, [(name, plistlib.dumps(replaced) if name == info_name else data, mode)
                                    for name, data, mode in original], 0)
        with self.assertRaisesRegex(ValueError, 'bundle metadata differs from source'):
            verify_release.inspect_archive(archive, commit, '0.2.0', fixture.root)
        package.write_zip(archive, original, 0)
        native = mock.Mock()
        with mock.patch.object(sign_macos.sys, 'platform', 'darwin'), mock.patch.object(sign_macos, 'preflight', native):
            with self.assertRaisesRegex(ValueError, 'awaits independent review'):
                sign_macos.sign_archive(archive, self.base / 'signed', self.base / 'sign-work', commit,
                    '0.2.0', package.digest(archive), 'ABCD123456', 'A' * 40, 'synthetic-existing-profile',
                    '1', source_root=fixture.root)
        native.assert_not_called()
        # The same real writer/inspector binds reviewed public pins to source;
        # no native provider is reached for a mismatching publisher.
        pins = dict(schema=1, feed_url='https://peerbrush.com/updates/macos/stable/appcast.xml',
                    public_ed_key=base64.b64encode(bytes([1]) * 32).decode(), publisher_team_id='ABCD123456')
        (fixture.root / bundle.CHECK_CONFIGURATION).write_text(json.dumps(pins))
        (fixture.root / bundle.POLICY_NAME).write_text(json.dumps(dict(schema=1, archive_sha256=selected['archive_sha256'],
            objects={name: dict(identifier='org.synthetic.helper', unsigned_sha256='a' * 64, entitlements={})
                     for name, _ in bundle.OBJECTS})))
        fixture.commit()
        with contextlib.redirect_stdout(io.StringIO()):
            archive = package.package(*args, verify_binary=False, rust_docs=fixture.rust_docs, sparkle_archive=upstream)
        commit = package.git(fixture.root, 'rev-parse', 'HEAD').decode().strip()
        result = verify_release.inspect_archive(archive, commit, '0.2.0', fixture.root)
        with zipfile.ZipFile(archive) as zipped:
            info = plistlib.loads(zipped.read(result['prefix'] + 'PeerBrush.app/Contents/Info.plist'))
        self.assertEqual(info, package.macos_info('0.2.0', sparkle=True, check_pins=pins, check_layout=True))
        with mock.patch.object(sign_macos.sys, 'platform', 'darwin'), mock.patch.object(sign_macos, 'preflight', native):
            with self.assertRaisesRegex(ValueError, 'compiled manual-check Team ID'):
                sign_macos.sign_archive(archive, self.base / 'wrong-team', self.base / 'wrong-team-work', commit,
                    '0.2.0', package.digest(archive), 'ZZZZ123456', 'A' * 40, 'synthetic-existing-profile',
                    '1', source_root=fixture.root)
        native.assert_not_called()
        # A manifest cannot relabel this source as the historical regular layout.
        with zipfile.ZipFile(archive) as zipped:
            values = [(item.filename, zipped.read(item), item.external_attr >> 16) for item in zipped.infolist()]
        manifest_name = result['prefix'] + 'release.json'
        broken = copy.deepcopy(result['manifest'])
        del broken['macos_sparkle_layout']
        package.write_zip(archive, [(name, json.dumps(broken).encode() if name == manifest_name else data, mode)
                                    for name, data, mode in values], 0)
        with self.assertRaisesRegex(ValueError, 'exact committed source contract'):
            verify_release.inspect_archive(archive, commit, '0.2.0', fixture.root)

    def test_native_audit_is_bounded_review_only_and_never_modifies_upstream_code(self):
        import platform
        import prepare_sparkle
        selected = copy.deepcopy(self.pin)
        archive_path = self.base / 'native-audit-upstream.zip'
        notice = (pathlib.Path(__file__).resolve().parents[1] / 'packaging/sparkle/LICENSE').read_text(encoding='utf-8').encode()
        with zipfile.ZipFile(archive_path, 'x') as archive:
            for name, item in selected['framework_entries'].items():
                member = selected['framework_prefix'] + ('' if name == '.' else name)
                if item['kind'] == 'directory':
                    member = member.rstrip('/') + '/'
                    data, mode = b'', stat.S_IFDIR | item['mode']
                elif item['kind'] == 'symlink':
                    data, mode = item['target'].encode(), stat.S_IFLNK | 0o777
                else:
                    data, mode = b'SYNTHETIC:' + name.encode(), stat.S_IFREG | item['mode']
                info = zipfile.ZipInfo(member)
                info.create_system, info.external_attr = 3, mode << 16
                archive.writestr(info, data)
            archive.writestr('LICENSE', notice)
        selected.update(archive_bytes=archive_path.stat().st_size, archive_sha256=package.digest(archive_path))
        original = archive_path.read_bytes()
        calls = []
        def native(command):
            calls.append(command)
            self.assertEqual(command[0], '/usr/bin/codesign')
            self.assertNotIn('--sign', command)
            if '--remove-signature' in command:
                self.assertTrue(pathlib.Path(command[-1]).is_relative_to(self.base / 'audit'))
                pathlib.Path(command[-1]).write_bytes(b'SYNTHETIC_UNSIGNED')
                return subprocess.CompletedProcess(command, 0, '', '')
            if '--entitlements' in command:
                return subprocess.CompletedProcess(command, 0, plistlib.dumps({}).decode(), '')
            return subprocess.CompletedProcess(command, 0, '', 'Identifier=org.synthetic.helper\n')
        with mock.patch.object(prepare_sparkle, 'load_pin', return_value=selected), \
                mock.patch.object(bundle.sys, 'platform', 'darwin'), \
                mock.patch.object(platform, 'mac_ver', return_value=('12.0', (), 'arm64')):
            result = bundle.audit_native(archive_path, self.base / 'audit', native)
            with self.assertRaises(ValueError):
                bundle.audit_native(archive_path, self.base / 'audit', native)
        self.assertEqual(result['authority'], 'review-candidate-only')
        self.assertFalse(result['updater_started'] or result['signing_performed'] or result['production_ready'])
        self.assertEqual(set(result['objects']), {name for name, _ in bundle.OBJECTS})
        self.assertLess(len(json.dumps(result).encode()), 16 * 1024)
        self.assertEqual(archive_path.read_bytes(), original)
        self.assertEqual(sum('--remove-signature' in call for call in calls), 5)

    def test_source_layout_cannot_omit_native_policy_or_downgrade_marker(self):
        sources = [('src/sparkle.rs', b'pub const BUNDLED_LAYOUT: u32 = 1;', 0o644)]
        with self.assertRaisesRegex(ValueError, 'incomplete'):
            package.macos_sparkle_required(sources)
        self.assertFalse(package.macos_sparkle_required([('src/sparkle.rs', b'// Historical probe', 0o644)]))
        complete = {name: b'' for name in ['scripts/sparkle_bundle.py', bundle.POLICY_NAME,
                    'packaging/sparkle/LICENSE', bundle.PIN_NAME]}
        complete['src/sparkle.rs'] = bundle.CHECK_MARKER
        with self.assertRaisesRegex(ValueError, 'incomplete'):
            package.macos_sparkle_required([(name,data,0o644) for name,data in complete.items()])

    def test_native_verifier_rejects_publisher_timestamp_entitlement_and_code_changes(self):
        app = self.base / 'PeerBrush.app'
        framework = app / 'Contents/Frameworks/Sparkle.framework'
        framework.mkdir(parents=True)
        normalized = hashlib.sha256(b'SYNTHETIC_UNSIGNED_CODE').hexdigest()
        policy = {name: dict(identifier='org.synthetic.helper', entitlements={}, unsigned_sha256=normalized)
                  for name, _ in bundle.OBJECTS}
        for _, code in bundle.OBJECTS:
            target = framework / code
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(b'SYNTHETIC_SIGNED_CODE')
        for damage in ['publisher', 'timestamp', 'entitlements', 'identifier', 'runtime', 'code']:
            with self.subTest(damage=damage):
                def native(command):
                    if '--remove-signature' in command:
                        pathlib.Path(command[-1]).write_bytes(b'CHANGED' if damage == 'code' else b'SYNTHETIC_UNSIGNED_CODE')
                        return subprocess.CompletedProcess(command, 0, '', '')
                    if '--entitlements' in command:
                        data = {'com.apple.security.get-task-allow': True} if damage == 'entitlements' else {}
                        return subprocess.CompletedProcess(command, 0, plistlib.dumps(data).decode(), '')
                    text = 'Identifier=' + ('org.changed' if damage == 'identifier' else 'org.synthetic.helper') + '\n'
                    text += 'TeamIdentifier=' + ('CHANGED000' if damage == 'publisher' else 'ABCD123456') + '\n'
                    text += 'Authority=Developer ID Application: Synthetic (ABCD123456)\n'
                    text += 'CodeDirectory v=20500 flags=0x10000(' + ('none' if damage == 'runtime' else 'runtime') + ')\n'
                    if damage != 'timestamp':
                        text += 'Timestamp=Synthetic public fixture\n'
                    return subprocess.CompletedProcess(command, 0, '', text)
                with mock.patch.object(bundle, 'reviewed_policy', return_value=policy), self.assertRaises(ValueError):
                    bundle.verify_native(app, 'ABCD123456', native)


if __name__ == '__main__':
    unittest.main()
