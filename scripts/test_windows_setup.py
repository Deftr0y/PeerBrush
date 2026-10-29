"""Source/byte fixtures plus explicit read-only real-binary smoke; never install."""
import copy
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import build_release
import build_windows_setup as setup
import test_windows_qualification as fixtures
import test_windows_version as resource
import verify_macos_qualification as shared
import prepare_site_release as website

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/release' / ('peerbrush-setup.exe' if sys.platform == 'win32' else 'peerbrush-setup')


def executable(libraries=('KERNEL32.dll',), *, text='0.2.0', overlay=b''):
    fields = setup.windows_version.strings(text, setup=True)
    value = resource.version_value(text, fields=fields)
    data = bytearray(resource.resource_executable(text, value=value, overlay=overlay))
    pointer = (len(data) + 511) & ~511
    data.extend(b'\0' * (pointer - len(data)))
    entries = bytearray(20 * (len(libraries) + 1))
    for index, library in enumerate(libraries):
        name = 0x3000 + len(entries)
        entries.extend(library.encode('ascii') + b'\0')
        struct.pack_into('<5I', entries, 20 * index, 0, 0, 0, name, 0x3010)
    size = (len(entries) + 511) & ~511
    struct.pack_into('<H', data, 134, 3)
    data[472:480] = b'.idata\0\0'
    struct.pack_into('<IIII', data, 480, len(entries), 0x3000, size, pointer)
    struct.pack_into('<II', data, 272, 0x3000, 20 * (len(libraries) + 1))
    data.extend(entries + b'\0' * (size - len(entries)))
    return bytes(data)


class SourceAndBytes(unittest.TestCase):
    def test_cargo_snapshot_refuses_late_source_changes_and_retains_both_files(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-setup-cargo-fault-') as temporary:
            work = Path(temporary).resolve()
            source = work/'target/x86_64-pc-windows-msvc/release/peerbrush-setup.exe'
            source.parent.mkdir(parents=True); original = executable(); source.write_bytes(original)
            with mock.patch.object(setup.os, 'fsync', side_effect=lambda _:source.write_bytes(b'foreign late output')):
                with self.assertRaisesRegex(ValueError, 'changed during snapshot'): setup.cargo_snapshot(work)
            self.assertEqual(source.read_bytes(), b'foreign late output')
            self.assertEqual((work/'peerbrush-setup.exe').read_bytes(), original)

    def test_actual_cargo_aliases_get_an_exclusive_snapshot_without_relaxing_native_input_guards(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-setup-cargo-') as temporary:
            work = Path(temporary).resolve()
            directory = work/'target/x86_64-pc-windows-msvc/release'
            (directory/'deps').mkdir(parents=True)
            source = directory/'peerbrush-setup.exe'; source.write_bytes(executable())
            alias = directory/'deps/peerbrush_setup.exe'
            os.link(source, alias)
            self.assertEqual(source.stat().st_nlink, 2)
            with self.assertRaises(ValueError): setup.bounded_file(source, setup.LIMIT, executable=True)
            snapshot = setup.cargo_snapshot(work)
            self.assertEqual(snapshot.read_bytes(), source.read_bytes())
            self.assertEqual(snapshot.stat().st_nlink, 1)
            self.assertEqual(source.stat().st_nlink, 2)
            setup.bounded_file(snapshot, setup.LIMIT, executable=True)
            snapshot.write_bytes(b'foreign replacement')
            with self.assertRaises(FileExistsError): setup.cargo_snapshot(work)
            self.assertEqual(snapshot.read_bytes(), b'foreign replacement')
            alias.rename(directory/'unexpected.exe')
            with self.assertRaises(ValueError): setup.cargo_snapshot(work)
            self.assertEqual(source.read_bytes(), (directory/'unexpected.exe').read_bytes())

    def test_native_setup_check_preserves_the_main_cache_and_unfiltered_regression_boundary(self):
        workflow = (ROOT/'.github/workflows/ci.yml').read_text()
        save = workflow.index('      - name: Save CI build cache after main compilation')
        setup_check = workflow.index('      - name: Verify dormant native setup without a VC runtime prerequisite')
        regression = workflow.index('      - name: Run regression tests and summarize failures')
        self.assertLess(save, setup_check); self.assertLess(setup_check, regression)
        block = workflow[setup_check:regression]
        self.assertIn("        if: runner.os == 'Windows'\n", block)
        self.assertNotIn('cache-hit', block)
        self.assertNotIn('continue-on-error', block)
        self.assertNotIn('always()', block)

    def test_failed_cli_reports_only_fixed_public_refusal_codes(self):
        for error, code in [(ValueError('PRIVATE_CANARY/path'), 'UNAVAILABLE'),
                            (ValueError('Setup has invalid imports or requires the runtime it must install'), 'CRT_OR_DLL_IMPORTS')]:
            output = io.StringIO()
            with mock.patch.object(sys, 'argv', ['setup', '--source-root', '.', '--commit', 'a'*40,
                    '--work', 'unopened-work', '--output', 'unopened-output']), \
                    mock.patch.object(setup, 'build', side_effect=error), contextlib.redirect_stderr(output):
                self.assertEqual(setup.main(), 1)
            self.assertIn('('+code+')', output.getvalue())
            self.assertNotIn('PRIVATE_CANARY', output.getvalue())
            self.assertNotIn('unopened-work', output.getvalue())

    def test_windows_verifier_child_has_private_state_no_inherited_auth_and_no_visible_window(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-setup-verifier-') as directory:
            root = Path(directory)
            for name in ['home','cache','config']: (root/name).mkdir()
            observed = {}
            class Child:
                def __init__(self, command, **options):
                    observed.update(options)
                    options['stdout'].write(b'bounded verifier response')
                def poll(self): return 0
            with mock.patch.dict(os.environ, {'GH_TOKEN':'PRIVATE_CANARY','GITHUB_TOKEN':'PRIVATE_CANARY'}), \
                    mock.patch.object(shared.subprocess,'Popen',Child), \
                    mock.patch.object(shared.subprocess,'CREATE_NO_WINDOW',0x08000000,create=True):
                self.assertEqual(shared._command(Path('gh.exe'),['--version'],root,'windows',_windows_host=True),b'bounded verifier response')
            self.assertEqual({p.name for p in root.iterdir()}, {'home','cache','config','windows.stdout','windows.stderr'})
            self.assertEqual((root/'windows.stderr').read_bytes(), b'')
            environment = observed['env']
            self.assertNotIn('GH_TOKEN',environment); self.assertNotIn('GITHUB_TOKEN',environment)
            self.assertNotIn('PRIVATE_CANARY',json.dumps(environment))
            self.assertEqual(environment['USERPROFILE'],str(root/'home'))
            self.assertEqual(environment['GH_CONFIG_DIR'],str(root/'config'))
            self.assertEqual(observed['creationflags'],0x08000000)

    def test_crt_imports_delay_tables_and_malformed_names_cannot_pass_as_bootstrapper(self):
        data = executable()
        self.assertEqual(setup.imported_libraries(data), ['kernel32.dll'])
        setup.windows_version.inspect(data, '0.2.0', setup=True)
        with self.assertRaises(ValueError): setup.windows_version.inspect(data, '0.2.0')
        for name in ['VCRUNTIME140.dll', 'ucrtbase.dll', 'MSVCP140.dll', 'api-ms-win-crt-heap-l1-1-0.dll',
                     'ext-ms-win-crt-runtime-l1-1-0.dll', 'foreign/path.dll']:
            with self.subTest(library=name), self.assertRaises(ValueError): setup.imported_libraries(executable((name,)))
        for libraries in [(), ('kernel32.dll', 'KERNEL32.dll')]:
            with self.assertRaises(ValueError): setup.imported_libraries(executable(libraries))
        for offset, values in [(272, (0xfffffff0, 40)), (272, (0x3000, 20)), (368, (0x3000, 32))]:
            changed = bytearray(data); struct.pack_into('<II', changed, offset, *values)
            with self.assertRaises(ValueError): setup.imported_libraries(bytes(changed))
        with self.assertRaises(ValueError): setup.imported_libraries(data[:511])

    def test_ordinary_builds_reject_setup_offer_inheritance(self):
        with self.assertRaisesRegex(ValueError, 'separate reviewed setup producer'):
            build_release.release_environment(ROOT, {'PEERBRUSH_SETUP_OFFER':'PRIVATE_CANARY'})
        with self.assertRaises(ValueError): setup.PreparedOffer(ROOT, {}, [], object())
        with self.assertRaises(ValueError): setup.BuiltSetup(ROOT, Path('foreign.exe'), 'a'*64, None, {}, object())


class BoundOffer(unittest.TestCase):
    def setUp(self):
        self.facts = fixtures.WindowsFacts(methodName='runTest')
        self.addCleanup(self.facts.doCleanups)
        self.facts.setUp()
        self.facts.qualify()
        self.service, self.native = self.facts.trust()
        f = self.facts.f
        portable = next(row for row in f.inventory['assets'] if row.get('platform') == 'Windows')
        self.selection = dict(commit=f.fixture.commit, version='0.2.0', portable=f.directory / portable['file'],
            portable_sha256=portable['sha256'], windows=dict(f.windows, native=self.native),
            discovery='https://downloads.example.invalid/stable/PeerBrush-Windows-x64.appinstaller',
            archive_root='https://downloads.example.invalid/immutable/')
        self.root, self.base = f.root, f.fixture.base

    def prepare(self, selection=None, *, host='linux'):
        def command(executable, arguments, work, label, **options):
            self.assertEqual(options, {'_windows_host':True} if host == 'win32' else {})
            if host == 'win32': self.assertEqual(executable.name, 'gh.exe')
            return self.facts.crypto_command(executable, arguments, work, label)
        with mock.patch.object(shared.sys, 'platform', host), \
                mock.patch.object(shared.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(shared, '_command', side_effect=command), \
                mock.patch.object(website, 'public_get', side_effect=self.service.get):
            return setup.prepare_offer(self.root, self.selection if selection is None else selection)

    def test_fixed_windows_host_retains_crypto_job_and_cli_pins_without_relaxing_aggregate_host(self):
        cli = Path(self.native['trust']['cli']).parent/'gh.exe'
        cli.write_bytes(executable())
        self.native['trust'].update(cli=cli, cli_sha256=hashlib.sha256(cli.read_bytes()).hexdigest())
        prepared = self.prepare(host='win32')
        prepared.verify()
        self.assertEqual(len(self.service.calls),2)
        import verify_windows_qualification as native
        raw = self.facts.report_path.read_bytes()
        inputs = dict(qualification_sha256=self.native['qualification_sha256'], commit=self.selection['commit'],
                      version='0.2.0', source_root=self.root, **self.native['trust'])
        with mock.patch.object(shared.sys,'platform','win32'), mock.patch.object(shared.platform,'machine',return_value='x86_64'):
            with self.assertRaisesRegex(ValueError,'Linux x64 aggregate'): native.authenticate(raw, **inputs)
            with self.assertRaises(ValueError): native.authenticate_setup(raw, **dict(inputs,_setup_host=True))
        selected = copy.deepcopy(self.selection)
        selected['windows']['native']['trust']['work'] = self.base/'rejected-windows-host'
        cli.write_bytes(b'not a PE executable')
        selected['windows']['native']['trust']['cli_sha256'] = hashlib.sha256(cli.read_bytes()).hexdigest()
        with self.assertRaises(ValueError): self.prepare(selected,host='win32')

    def test_actual_byte_native_and_service_consumers_construct_only_fixed_unready_offer(self):
        prepared = self.prepare()
        prepared.verify()
        offer = json.loads(prepared.data)
        self.assertEqual(offer['format'], 'PeerBrushWindowsSetupOfferV1')
        self.assertEqual(offer['target']['build'], self.selection['commit'])
        self.assertEqual(offer['package']['sha256'], self.selection['windows']['package_sha256'])
        self.assertEqual(offer['policy']['certificate'], 'a'*64)
        self.assertEqual(offer['policy']['runtime_certificate'], 'c'*64)
        self.assertEqual(len(self.service.calls), 2)
        self.assertNotIn(str(self.base).encode(), prepared.data)
        prepared.data += b'\n'
        with self.assertRaisesRegex(ValueError, 'offer changed'): prepared.verify()

    def test_raw_ready_reports_missing_origin_and_changed_selected_media_cannot_build(self):
        for change in ['origin', 'archive', 'package']:
            selected = copy.deepcopy(self.selection)
            if change == 'origin': selected['windows'].pop('native')
            elif change == 'archive': selected['archive_root'] = 'https://foreign.example.invalid/immutable/'
            else: selected['windows']['package_sha256'] = 'd'*64
            with self.subTest(change=change), self.assertRaises(ValueError): self.prepare(selected)
        self.service.run['conclusion'] = 'failure'
        with self.assertRaises(ValueError): self.prepare()

    def build_fixture(self, prepared):
        work, output = self.base/'setup-build-work', self.base/'setup-build-output'
        f = self.facts.f.fixture.debian.fixture
        build_info = setup.packaging.binary_build_info(self.facts.f.original_executable, '0.2.0', self.selection['commit'])
        data = executable(overlay=json.dumps(build_info, separators=(',', ':')).encode())
        calls = []
        real_run = setup.subprocess.run
        def cargo(command, **options):
            if command[:2] != ['cargo', 'build']: return real_run(command, **options)
            calls.append(command)
            self.assertIn('--locked', command); self.assertIn('--features', command)
            self.assertIn('-Ctarget-feature=+crt-static', options['env']['CARGO_ENCODED_RUSTFLAGS'])
            self.assertEqual(options['env']['PEERBRUSH_SETUP_OFFER'].encode(), prepared.data)
            path = work/'target/x86_64-pc-windows-msvc/release/peerbrush-setup.exe'
            path.parent.mkdir(parents=True); path.write_bytes(data)
            return subprocess.CompletedProcess(command, 0)
        def info(binary, arguments, directory, label):
            self.assertEqual(arguments, ['--setup-info'])
            value = dict(schema=1, format='PeerBrushWindowsSetupInfoV1', build=build_info,
                offer_sha256=hashlib.sha256(prepared.data).hexdigest(), installation_enabled=False,
                installed_acceptance='not-established', update_eligible=False, ready=False)
            raw = setup.encode(value)
            (directory/(label+'.stdout')).write_bytes(raw)
            (directory/(label+'.stderr')).write_bytes(b'')
            return raw
        with mock.patch.object(setup.sys, 'platform', 'win32'), \
                mock.patch.object(setup.jxl_build, 'configure', side_effect=lambda env:(env, {})), \
                mock.patch.object(setup.subprocess, 'run', side_effect=cargo), \
                mock.patch.object(setup.final.signed, '_command', side_effect=info):
            built = setup.build(self.root, work, output, commit=self.selection['commit'], prepared=prepared,
                registry=f.root/'registry', rust_docs=f.rust_docs)
        self.assertEqual(len(calls), 1)
        built.verify()
        return built

    def test_real_source_notice_and_pe_consumers_bound_build_and_fresh_identity_reply(self):
        prepared = self.prepare()
        built = self.build_fixture(prepared)
        self.assertFalse(built.report['production_ready'])
        self.assertEqual(built.report['static_crt_import_check'], 'no-dynamic-crt-imports')
        offer = json.loads(prepared.data)
        expected = dict(schema=1, format='PeerBrushWindowsInstalledIdentityV1',
            offer_sha256=hashlib.sha256(prepared.data).hexdigest(),
            **{name:offer[name] for name in ['commit','version','package','runtime','target']},
            installed_identity_verified=True, installed_acceptance='not-established', update_eligible=False, ready=False)
        with mock.patch.object(setup.final.signed, '_command', return_value=setup.encode(expected)) as native:
            observed = setup.verify_installed(built, self.base/'identity-observation')
            self.assertEqual(native.call_args.args[1], ['--verify-installed'])
        self.assertTrue(observed['installed_identity_verified']); self.assertFalse(observed['ready'])
        for index, (key, value) in enumerate([('ready',True),('update_eligible',True),('installed_acceptance','passed'),('installed_identity_verified',1),('offer_sha256','d'*64),('unknown',True)]):
            changed = dict(expected, **{key:value})
            with mock.patch.object(setup.final.signed, '_command', return_value=setup.encode(changed)):
                with self.assertRaises(ValueError): setup.verify_installed(built, self.base/('bad-observation-'+str(index)))
        (built.executable.parent/'foreign').write_bytes(b'foreign bytes')
        with self.assertRaises(ValueError): built.verify()
        self.assertEqual((built.executable.parent/'foreign').read_bytes(), b'foreign bytes')


class RuntimeSmoke(unittest.TestCase):
    def test_unconfigured_actual_executable_reports_disabled_without_opening_profile_or_accepting_runtime_config(self):
        if not BINARY.is_file(): raise RuntimeError('Build the source-matched peerbrush-setup feature target first; never silently skip runtime checks')
        with tempfile.TemporaryDirectory(prefix='peerbrush-setup-runtime-') as directory:
            root = Path(directory)
            environment = os.environ.copy()
            for name in ['HOME','USERPROFILE','LOCALAPPDATA','APPDATA','XDG_CONFIG_HOME','XDG_CACHE_HOME']:
                environment[name] = str(root/name)
            result = subprocess.run([str(BINARY),'--setup-info'], cwd=root, env=environment, capture_output=True, timeout=15, check=True)
            self.assertLess(len(result.stdout), setup.SMALL)
            value = json.loads(result.stdout)
            self.assertEqual(set(value), {'schema','format','build','offer_sha256','installation_enabled','installed_acceptance','update_eligible','ready'})
            self.assertEqual(value['format'], 'PeerBrushWindowsSetupInfoV1')
            self.assertIsNone(value['offer_sha256']); self.assertIs(value['installation_enabled'],False)
            self.assertIs(value['update_eligible'],False); self.assertIs(value['ready'],False)
            self.assertEqual(list(root.iterdir()), [])
            for args in [['--verify-installed'], ['--setup-info','https://foreign.example.invalid/'], ['--offer','foreign.json']]:
                failed = subprocess.run([str(BINARY),*args], cwd=root, env=environment, capture_output=True, timeout=15)
                self.assertNotEqual(failed.returncode,0); self.assertEqual(list(root.iterdir()), [])


if __name__ == '__main__': unittest.main()
