"""Offline auxiliary handoff regressions; synthetic MSIX is not SDK or Store authority."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock

from package import digest, git
import handoff_store_msix as handoff
import test_store_msix_preflight as preflight_tests
import test_dmg_handoff as dmg_tests
from test_msix import synthetic_package
from verify_release import verify
from test_production_readiness import assert_publication_phases


class StoreHandoff(unittest.TestCase):
    @contextlib.contextmanager
    def checked_common_assessment(self):
        original, seen = handoff.assess, []
        def inspect(*args, **kwargs):
            report = original(*args, **kwargs)
            assert_publication_phases(self, report)
            seen.append(report)
            return report
        with mock.patch.object(handoff, 'assess', side_effect=inspect):
            yield
        self.assertEqual(len(seen), 1, 'The actual common inspection must supply informational phases')

    def setUp(self):
        self.seed = preflight_tests.StorePreflight(methodName='runTest')
        self.addCleanup(self.seed.doCleanups)
        self.seed.setUp()
        self.root = self.seed.root
        self.fixture = self.seed.fixture.msix.fixture
        self.package = self.seed.package.rename(self.fixture.output / self.seed.package.name)
        self.report = self.seed.report.rename(self.fixture.output / self.seed.report.name)
        self.seed.package, self.seed.report = self.package, self.report
        self.commit = self.seed.inputs['commit']
        archives = [self.seed.fixture.portable, self.fixture.build('macOS'), self.fixture.build('Linux')]
        raw = self.fixture.output / 'selected-platforms'
        raw.mkdir()
        for archive in archives:
            shutil.copyfile(archive, raw / archive.name)
        self.directory = self.fixture.output / 'common-candidate'
        with contextlib.redirect_stdout(io.StringIO()):
            verify(raw, self.commit, '0.26.0', self.directory, self.root)
        self.sidecar = self.package.with_name(self.package.name + '.sha256')
        self.sidecar.write_bytes(f"{self.seed.inputs['package_sha256']}  {self.package.name}\n".encode())
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-store-bundle-test-')
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name).resolve()
        self.output = self.parent / 'bundle'
        self.assertEqual(git(self.root, 'status', '--porcelain', '--untracked-files=normal'), b'')

    def arguments(self, **overrides):
        values = dict(commit=self.commit, version='0.26.0', previous_version='1.25.0.0',
            inventory_sha256=digest(self.directory / 'release-inventory.json'),
            portable_sha256=self.seed.inputs['portable_sha256'],
            package_sha256=digest(self.package), report_sha256=digest(self.report))
        values.update(overrides)
        return values

    def run_handoff(self, **overrides):
        package = overrides.pop('package', self.package)
        report = overrides.pop('report', self.report)
        output = overrides.pop('output', self.output)
        with mock.patch.object(handoff.preflight.store, 'Store', side_effect=AssertionError('No Store authentication')), \
                mock.patch.object(handoff.preflight.store, 'request', side_effect=AssertionError('No network')):
            return handoff.handoff(self.root, self.directory, package, report, output,
                                   **self.arguments(**overrides))

    def test_real_content_enters_a_closed_auxiliary_bundle_without_changing_common_or_authority(self):
        before = {path.name: digest(path) for path in self.directory.iterdir()}
        (self.package.parent / 'unrelated-private-state.json').write_text('PRIVATE_UNUSED_STATE')
        with self.checked_common_assessment():
            result = self.run_handoff()
        expected = {self.package.name, self.report.name, self.sidecar.name, handoff.MANIFEST}
        self.assertEqual({path.name for path in self.output.iterdir()}, expected)
        self.assertEqual(json.loads((self.output / handoff.MANIFEST).read_bytes()), result)
        self.assertEqual((self.output / self.package.name).read_bytes(), self.package.read_bytes())
        self.assertEqual((self.output / self.report.name).read_bytes(), self.report.read_bytes())
        self.assertEqual(before, {path.name: digest(path) for path in self.directory.iterdir()})
        for asset in result['assets']:
            self.assertEqual(digest(self.output / asset['file']), asset['sha256'])
            self.assertEqual((self.output / asset['file']).stat().st_size, asset['bytes'])
        self.assertTrue(result['content_verified'])
        self.assertFalse(result['ready'])
        self.assertFalse(result['production_ready'])
        self.assertNotIn('publication_phases', result, 'The closed Store inventory contract is unchanged')
        self.assertNotIn('publication_phases', json.loads((self.directory / 'release-inventory.json').read_bytes()))
        self.assertEqual(result['native_sdk_validation'], 'not-performed-by-consumer')
        self.assertEqual(result['native_producer_authentication'], 'unverified-requires-trusted-native-job')
        self.assertEqual(result['installed_acceptance'], 'not-performed')
        self.assertEqual(result['public_availability'], 'unverified')
        self.assertEqual(result['store_submission'], 'not-performed')
        self.assertNotIn('PRIVATE_UNUSED_STATE', json.dumps(result))
        self.assertNotIn(str(self.root), json.dumps(result))

    def test_exact_source_version_and_independent_hashes_are_required(self):
        for key, value in [('commit', 'b' * 40), ('version', '0.26.1'), ('version', '0.26.0-dev.1'),
                           ('previous_version', '1.26.0.0'), ('previous_version', None), ('inventory_sha256', 'b' * 64),
                           ('portable_sha256', 'b' * 64), ('package_sha256', 'b' * 64),
                           ('report_sha256', 'b' * 64), ('report_sha256', True)]:
            with self.subTest(field=key), self.assertRaises(ValueError):
                self.run_handoff(**{key: value})
            self.assertFalse(self.output.exists())

    def test_refreshed_package_hash_report_and_sidecar_cannot_hide_foreign_payload(self):
        self.package.write_bytes(synthetic_package(dict(self.seed.fixture.payload, **{'peerbrush.exe': b'foreign'})))
        report = json.loads(self.report.read_bytes())
        report['sha256'] = digest(self.package)
        self.report.write_text(json.dumps(report), encoding='utf-8')
        self.sidecar.write_bytes(f"{digest(self.package)}  {self.package.name}\n".encode())
        with self.assertRaisesRegex(ValueError, 'payload differs'):
            self.run_handoff()
        self.assertFalse(self.output.exists())

    def test_unbound_explicit_store_version_development_identity_and_extra_report_fields_fail(self):
        original = json.loads(self.report.read_bytes())
        for change in [dict(version='2.0.0.0'), dict(development_identity=True),
                       dict(package_family_name='foreign_family'), dict(private_note='PRIVATE_REPORT')]:
            self.report.write_text(json.dumps(dict(original, **change)), encoding='utf-8')
            with self.subTest(fields=list(change)), self.assertRaises(ValueError):
                self.run_handoff()
            self.assertFalse(self.output.exists())
        self.report.write_text(json.dumps(original), encoding='utf-8')
        renamed = self.package.with_name('PeerBrush-2.0.0.0-Windows-x64.msix')
        renamed.write_bytes(self.package.read_bytes())
        with self.assertRaisesRegex(ValueError, 'filenames'):
            self.run_handoff(package=renamed)
        self.assertFalse(self.output.exists())

    def test_common_inventory_checksums_shape_and_source_remain_strict(self):
        path = self.directory / 'release-inventory.json'
        original = path.read_bytes()
        inventory = json.loads(original)
        changes = [dict(inventory, packaging_correction='repacked'),
                   dict(inventory, binary_source_commit='b' * 40),
                   dict(inventory, assets=inventory['assets'][:-1]),
                   dict(inventory, assets=[*inventory['assets'], inventory['assets'][0]])]
        for altered in changes:
            path.write_text(json.dumps(altered), encoding='utf-8')
            with self.subTest(change=list(altered)), self.assertRaises(ValueError):
                self.run_handoff()
        path.write_bytes(original)
        sums = self.directory / 'SHA256SUMS'
        sums.write_bytes(sums.read_bytes() + b'\n')
        with self.assertRaisesRegex(ValueError, 'checksum'):
            self.run_handoff()
        self.assertFalse(self.output.exists())

    def test_five_and_six_record_inputs_use_actual_shared_inspection_and_explicit_context(self):
        fixture = dmg_tests.Handoff(methodName='runTest')
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        for count in [5, 6]:
            if count == 6:
                fixture.handoff()
            directory = fixture.directory if count == 5 else fixture.output
            inventory = json.loads((directory / 'release-inventory.json').read_bytes())
            windows = next(record for record in inventory['assets'] if record.get('platform') == 'Windows')
            source = next(record for record in inventory['assets'] if 'platform' not in record)
            context = dict(maintainer=dmg_tests.test_deb.MAINTAINER,
                           dmg_qualification=None, dmg_qualification_sha256=None, macos_team_id=None)
            if count == 6:
                context.update(dmg_qualification=fixture.evidence,
                    dmg_qualification_sha256=digest(fixture.evidence),
                    macos_team_id=dmg_tests.test_sign_macos.TEAM)
            with self.checked_common_assessment():
                result = handoff.common(directory, fixture.root, fixture.commit, '0.2.0',
                    digest(directory / 'release-inventory.json'), windows['sha256'], **context)
            self.assertEqual(result[0], directory)
            self.assertEqual(result[1], directory / windows['file'])
            self.assertEqual(result[2], source['sha256'])
            inputs = {path: (checksum, size) for path, checksum, size, _ in result[3]}
            self.assertIn(directory / fixture.deb.name, inputs)
            with self.subTest(count=count, missing='maintainer'), self.assertRaises(ValueError):
                handoff.common(directory, fixture.root, fixture.commit, '0.2.0',
                    digest(directory / 'release-inventory.json'), windows['sha256'],
                    **dict(context, maintainer=None))
            if count == 6:
                self.assertIn(fixture.evidence, inputs)
                for change in [dict(dmg_qualification=None, dmg_qualification_sha256=None),
                               dict(dmg_qualification_sha256='b' * 64), dict(macos_team_id='OTHER12345')]:
                    with self.subTest(count=count, change=sorted(change)), self.assertRaises(ValueError):
                        handoff.common(directory, fixture.root, fixture.commit, '0.2.0',
                            digest(directory / 'release-inventory.json'), windows['sha256'],
                            **dict(context, **change))
        deb = fixture.output / fixture.deb.name
        deb.write_bytes(deb.read_bytes() + b'foreign')
        with self.assertRaises(ValueError):
            handoff.common(fixture.output, fixture.root, fixture.commit, '0.2.0',
                digest(fixture.output / 'release-inventory.json'), windows['sha256'], **context)

    def test_common_assessment_cannot_be_replaced_by_a_caller_report(self):
        with mock.patch.object(handoff, 'assess', side_effect=ValueError('synthetic existing inspector rejection')), \
                mock.patch.object(handoff.preflight, 'qualify') as qualify, self.assertRaises(ValueError):
            self.run_handoff()
        qualify.assert_not_called()
        self.assertFalse(self.output.exists())

    def test_sidecar_is_checked_and_crlf_input_gets_a_canonical_separate_output(self):
        expected = self.sidecar.read_bytes()
        self.sidecar.write_bytes(b'0' * 64 + b'  unrelated.msix\n')
        with self.assertRaisesRegex(ValueError, 'sidecar'):
            self.run_handoff()
        self.sidecar.write_bytes(expected.replace(b'\n', b'\r\n'))
        self.run_handoff()
        self.assertEqual((self.output / self.sidecar.name).read_bytes(), expected)
        self.assertEqual(self.sidecar.read_bytes(), expected.replace(b'\n', b'\r\n'))

    def test_existing_output_git_worktree_and_overlap_are_rejected_without_overwrite(self):
        self.output.mkdir()
        marker = self.output / 'human.txt'
        marker.write_bytes(b'Exact retained bytes')
        with self.assertRaises(ValueError):
            self.run_handoff()
        self.assertEqual(marker.read_bytes(), b'Exact retained bytes')
        for destination in [self.root / 'unsafe', self.directory / 'nested']:
            with self.subTest(destination=destination.name), self.assertRaises(ValueError):
                self.run_handoff(output=destination)
            self.assertFalse(destination.exists())
        worktree = self.parent / 'worktree'
        worktree.mkdir()
        (worktree / '.git').write_text('gitdir: private-worktree-fixture')
        with self.assertRaises(ValueError):
            self.run_handoff(output=worktree / 'unsafe')
        self.assertFalse((worktree / 'unsafe').exists())

    def test_reparse_input_rejects_before_copy(self):
        original = Path.lstat
        def reparse(path, *args, **kwargs):
            info = original(path, *args, **kwargs)
            if path == self.package:
                return mock.Mock(st_mode=info.st_mode, st_file_attributes=0x400)
            return info
        with mock.patch.object(Path, 'lstat', reparse), self.assertRaisesRegex(ValueError, 'links or reparse'):
            self.run_handoff()
        self.assertFalse(self.output.exists())

    def test_interrupted_copy_never_writes_success_inventory_or_reuses_partial_output(self):
        original = handoff.bounded_file
        def interrupted(path, limit, destination=None):
            if destination is not None and destination.parent == self.output:
                destination.write_bytes(b'partial')
                raise OSError('PRIVATE_IO_DETAIL')
            return original(path, limit, destination)
        with mock.patch.object(handoff, 'bounded_file', side_effect=interrupted), self.assertRaises(OSError):
            self.run_handoff()
        self.assertFalse((self.output / handoff.MANIFEST).exists())
        self.assertTrue(self.output.exists())
        with self.assertRaises(ValueError):
            self.run_handoff()

    def test_common_inventory_changes_after_validation_reject_final_acceptance(self):
        original = handoff.bounded_file
        changed = False
        def changed_during_copy(path, limit, destination=None):
            nonlocal changed
            result = original(path, limit, destination)
            if destination is not None and destination.parent == self.output and not changed:
                changed = True
                inventory = self.directory / 'release-inventory.json'
                inventory.write_bytes(inventory.read_bytes() + b' ')
            return result
        with mock.patch.object(handoff, 'bounded_file', side_effect=changed_during_copy), self.assertRaises(ValueError):
            self.run_handoff()
        self.assertFalse((self.output / handoff.MANIFEST).exists())

    def test_unexpected_output_file_blocks_success_without_deleting_it(self):
        original = handoff.bounded_file
        def added_during_copy(path, limit, destination=None):
            result = original(path, limit, destination)
            if destination is not None and destination.parent == self.output:
                (self.output / 'unrelated-private.txt').write_bytes(b'PRIVATE_UNRELATED_OUTPUT')
            return result
        with mock.patch.object(handoff, 'bounded_file', side_effect=added_during_copy), self.assertRaises(ValueError):
            self.run_handoff()
        self.assertFalse((self.output / handoff.MANIFEST).exists())
        self.assertEqual((self.output / 'unrelated-private.txt').read_bytes(), b'PRIVATE_UNRELATED_OUTPUT')

    def test_tracked_source_changes_after_validation_reject_final_acceptance(self):
        original = handoff.bounded_file
        changed = False
        def edited_during_copy(path, limit, destination=None):
            nonlocal changed
            result = original(path, limit, destination)
            if destination is not None and destination.parent == self.output and not changed:
                changed = True
                cargo = self.root / 'Cargo.toml'
                cargo.write_bytes(cargo.read_bytes() + b'\n# synthetic tracked edit\n')
            return result
        with mock.patch.object(handoff, 'bounded_file', side_effect=edited_during_copy), self.assertRaisesRegex(ValueError, 'source changed'):
            self.run_handoff()
        self.assertFalse((self.output / handoff.MANIFEST).exists())

    def test_retained_inventory_requires_exact_same_version_assets_and_monotonic_store_baseline(self):
        manifest = self.run_handoff()
        previous = self.output / handoff.MANIFEST
        selected = dict(previous_inventory=previous, previous_inventory_sha256=digest(previous))
        self.assertEqual(self.run_handoff(output=self.parent / 'reconciled', **selected), manifest)
        for change in ['source', 'hash', 'missing', 'extra', 'authority', 'binding', 'prior-type', 'prior-canonical']:
            altered = copy.deepcopy(manifest)
            if change == 'source':
                altered['commit'] = 'b' * 40
            elif change == 'hash':
                altered['assets'][0]['sha256'] = 'b' * 64
            elif change == 'missing':
                altered['assets'].pop()
            elif change == 'extra':
                altered['assets'].append(dict(file='extra.msix', bytes=1, sha256='b' * 64))
            elif change == 'binding':
                altered['common_inventory_sha256'] = 'b' * 64
            elif change == 'prior-type':
                altered['previous_version'] = None
            elif change == 'prior-canonical':
                altered['previous_version'] = '01.25.0.0'
            else:
                altered['ready'] = True
            prior = self.parent / ('previous-' + change + '.json')
            prior.write_text(json.dumps(altered), encoding='utf-8')
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.run_handoff(output=self.parent / ('reject-' + change),
                    previous_inventory=prior, previous_inventory_sha256=digest(prior))
        with self.assertRaises(ValueError):
            self.run_handoff(output=self.parent / 'unbound', previous_inventory=previous)
        with self.assertRaises(ValueError):
            self.run_handoff(output=self.parent / 'wrong-prior-hash',
                previous_inventory=previous, previous_inventory_sha256='b' * 64)

        for prior_version, target, baseline in [('0.27.0', '1.27.0.0', '1.26.0.0'),
                                                 ('0.25.0', '1.25.0.0', '1.24.0.0')]:
            altered = copy.deepcopy(manifest)
            altered.update(version=prior_version, package_version=target, previous_version=baseline)
            for asset in altered['assets']:
                asset['file'] = asset['file'].replace('1.26.0.0', target)
            prior = self.parent / ('prior-' + prior_version + '.json')
            prior.write_text(json.dumps(altered), encoding='utf-8')
            with self.subTest(prior=prior_version), self.assertRaises(ValueError):
                self.run_handoff(output=self.parent / ('reject-prior-' + prior_version),
                    previous_inventory=prior, previous_inventory_sha256=digest(prior), previous_version='1.24.0.0')

    def test_source_and_workflow_registration_keep_common_and_signing_outputs_separate(self):
        root = Path(__file__).resolve().parents[1]
        policy = json.loads((root / 'scripts/package-files.json').read_bytes())
        for name in ['scripts/handoff_store_msix.py', 'scripts/test_store_msix_handoff.py']:
            self.assertIn(name, policy['source'])
        for name in ['ci.yml', 'release.yml']:
            workflow = (root / '.github/workflows' / name).read_text(encoding='utf-8')
            self.assertIn('python -B scripts/test_store_msix_handoff.py', workflow)
            self.assertIn('--selected-clang-cl', workflow)
            self.assertIn('--selected-cmake', workflow)
            self.assertIn('--selected-ninja', workflow)
            self.assertIn('a00cf47accde87400fad3f0fcfc5fe96fc4f5be1', workflow)
        release = (root / '.github/workflows/release.yml').read_text(encoding='utf-8')
        inspect = release.split('\n  inspect:\n', 1)[1].split('\n  sign:\n', 1)[0]
        self.assertLess(inspect.index('Require exact release invocation'),
                        inspect.index('Download the same-workflow Store MSIX candidate'))
        self.assertIn('name: unsigned-store-msix-candidate', inspect)
        self.assertIn('name: verified-store-msix-content', inspect)
        self.assertIn('name: verified-release-candidate', inspect)
        self.assertIn('if: inputs.build_store_msix', inspect)
        self.assertIn('steps.store_handoff.outputs.directory', inspect)
        self.assertIn('PRIVATE_PARENT=$(mktemp -d "$RUNNER_TEMP/peerbrush-store-handoff.XXXXXX")', inspect)
        self.assertIn('--inventory-sha256 "$INVENTORY_SHA" --portable-sha256 "$WINDOWS_SHA"', inspect)
        self.assertIn('--package-sha256 "$MSIX_SHA" --report-sha256 "$REPORT_SHA"', inspect)
        self.assertIn('Store handoff requires the canonical Cargo-to-Store version mapping', release)
        sign = release.split('\n  sign:\n', 1)[1]
        self.assertNotIn('store-msix', sign)
        self.assertNotIn('handoff_store_msix', sign)
        self.assertIn('inputs: dist/*.zip dist/*.deb dist/SHA256SUMS dist/release-inventory.json', sign)

    @unittest.skipUnless(os.name == 'nt', 'Actual MSIX SDK package requires Windows')
    def test_actual_sdk_store_package_uses_the_same_independent_handoff(self):
        sdk = self.fixture.output / 'sdk-store'
        report = handoff.preflight.store.msix.build(self.root, self.seed.fixture.portable, sdk,
            self.seed.fixture.report['identity'], previous_version='1.25.0.0',
            expected_sha256=self.seed.inputs['portable_sha256'],
            expected_package_family=handoff.preflight.store.FAMILY)
        package = sdk / report['package']
        producer = package.with_name(package.name + '.json')
        self.assertEqual(git(self.root, 'status', '--porcelain', '--untracked-files=normal'), b'')
        result = self.run_handoff(package=package, report=producer,
            package_sha256=digest(package), report_sha256=digest(producer))
        self.assertTrue(result['content_verified'])
        self.assertFalse(result['ready'])
        self.assertEqual(result['native_sdk_validation'], 'not-performed-by-consumer')

    def test_cli_failure_is_bounded_and_does_not_print_private_paths_or_exception_text(self):
        args = ['handoff', '--source-root', str(self.root), '--directory', str(self.directory),
                '--package', str(self.package), '--report', str(self.report), '--output', str(self.output)]
        for name, value in self.arguments().items():
            args.extend(['--' + name.replace('_', '-'), value])
        error = io.StringIO()
        with mock.patch.object(handoff.sys, 'argv', args), \
                mock.patch.object(handoff, 'handoff', side_effect=ValueError('PRIVATE_EXCEPTION_TOKEN')), \
                contextlib.redirect_stderr(error):
            self.assertEqual(handoff.main(), 1)
        self.assertNotIn('PRIVATE_EXCEPTION_TOKEN', error.getvalue())
        self.assertNotIn(str(self.root), error.getvalue())
        self.assertIn('no publication, authentication or installation', error.getvalue())


if __name__ == '__main__':
    unittest.main()
