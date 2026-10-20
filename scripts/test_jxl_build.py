"""Offline build-selection/privacy/source-notice regressions. No tool executes."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest import mock

from build_release import release_environment
import jxl_build

spec = importlib.util.spec_from_file_location('packaging', Path(__file__).with_name('package.py'))
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)


class NativeJxlBuild(unittest.TestCase):
    def test_cargo_child_preserves_exact_argv_environment_and_isolated_state(self):
        environment = {'PATH': 'synthetic native tools', 'CXXFLAGS': '-DKEEP=1'}
        arguments = ['--', 'run', '--locked', '--', '--state-dir', 'C:/synthetic state with spaces']
        with mock.patch.object(jxl_build.subprocess, 'run') as run:
            jxl_build.run_cargo(arguments, environment)
            run.assert_called_once_with(
                ['cargo', *arguments[1:]], cwd=Path(__file__).resolve().parents[1],
                env=environment, check=True)
        for invalid in [[], ['--'], ['publish'], ['--', '--help']]:
            with self.subTest(arguments=invalid), mock.patch.object(jxl_build.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'Use build'):
                    jxl_build.run_cargo(invalid, environment)
                run.assert_not_called()

    def test_source_and_library_overrides_are_rejected_even_if_empty(self):
        for name in ['DEP_JXL_PATH', 'DEP_JXL_LIB', 'dep_jxl_path']:
            for value in ['', 'synthetic external input']:
                with self.subTest(name=name, empty=not value), self.assertRaisesRegex(ValueError, 'overrides'):
                    release_environment(Path('synthetic source'), {name: value})

    def test_native_remapping_preserves_existing_flags_and_rejects_line_injection(self):
        with mock.patch('build_release.source_input_state', return_value='unknown'):
            environment = release_environment(Path('synthetic checkout'),
                {'CFLAGS': '-DREVIEWED=1', 'CXXFLAGS': '-DREVIEWED_CPP=1'})
            self.assertTrue(environment['CFLAGS'].startswith('-DREVIEWED=1 '))
            self.assertTrue(environment['CXXFLAGS'].startswith('-DREVIEWED_CPP=1 '))
            for name in ['CFLAGS', 'CXXFLAGS']:
                self.assertIn('-ffile-prefix-map=', environment[name])
                self.assertIn('=peerbrush"', environment[name])
                if os.name == 'nt':
                    self.assertIn('/clang:-ffile-prefix-map=', environment[name])
            with self.assertRaisesRegex(ValueError, 'unsupported character'):
                release_environment(Path('synthetic\ncheckout'), {})

    def test_version_discovery_is_exact_and_never_executes_a_shell_or_fallback(self):
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / 'synthetic-tool'
            executable.write_text('fixture never executed', encoding='utf-8')
            response = subprocess.CompletedProcess([], 0, b'clang version 21.1.8\n', b'')
            with mock.patch.object(jxl_build.shutil, 'which', return_value=str(executable)), mock.patch.object(jxl_build.subprocess, 'run', return_value=response) as run:
                path, version = jxl_build.tool('clang-cl', 'clang', {'21.1.8'}, {'PATH': 'fixture'})
                self.assertEqual((path, version), (str(executable.resolve()), '21.1.8'))
                self.assertEqual(run.call_args.args[0], [str(executable.resolve()), '--version'])
                self.assertEqual(run.call_args.kwargs['timeout'], 10)
                self.assertNotIn('shell', run.call_args.kwargs)
                for output in [b'clang version 22.0.0\n', b'not a version', b'x' * 16385]:
                    response.stdout = output
                    with self.assertRaises(ValueError):
                        jxl_build.tool('clang-cl', 'clang', {'21.1.8'}, {'PATH': 'fixture'})
            with mock.patch.object(jxl_build.shutil, 'which', return_value=None), mock.patch.object(jxl_build.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'Missing'):
                    jxl_build.tool('clang-cl', 'clang', {'21.1.8'}, {})
                run.assert_not_called()

    def test_numeric_version_diagnostic_is_bounded_private_and_does_not_relax_suffixes(self):
        secret = 'PRIVATE_TOOL_PATH_OUTPUT_AND_STDERR'
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / secret
            executable.write_text('fixture never executed', encoding='utf-8')
            response = subprocess.CompletedProcess([], 0, b'cmake version 4.4.4\n', secret.encode())
            with mock.patch.object(jxl_build.shutil, 'which', return_value=str(executable)), \
                    mock.patch.object(jxl_build.subprocess, 'run', return_value=response):
                for system in ['Windows', 'Darwin']:
                    with self.subTest(platform=system):
                        self.assertEqual(jxl_build.tool('cmake', 'cmake', jxl_build.VERSIONS[system]['cmake'], {}),
                                         (str(executable.resolve()), '4.4.4'))
                allowed = jxl_build.VERSIONS['Windows']['cmake']
                cases = [
                    ('cmake', 'cmake', allowed, b'cmake version 4.4.3\n', '4.4.3'),
                    ('cmake', 'cmake', allowed, b'cmake version 4.4.4-msvc\n', '4.4.4'),
                    ('cmake', 'cmake', allowed, b'cmake version 3.31.6-msvc\n', '3.31.6'),
                    ('cmake', 'cmake', allowed, b'cmake version 4.4.4', '4.4.4'),
                    ('cmake', 'cmake', allowed, b'cmake version 123456.4.4\n', 'unparsed'),
                    ('cmake', 'cmake', allowed, secret.encode(), 'unparsed'),
                    ('ninja', 'ninja', {'1.13.2'}, b'1.13.3 ' + secret.encode(), '1.13.3'),
                    ('clang-cl', 'clang', {'21.1.8'}, b'clang version 22.0.0 ' + secret.encode(), '22.0.0'),
                ]
                for name, kind, versions, output, numeric in cases:
                    response.stdout = output
                    with self.subTest(tool=name, numeric=numeric), self.assertRaises(ValueError) as caught:
                        jxl_build.tool(name, kind, versions, {})
                    self.assertEqual(str(caught.exception),
                        'Unqualified native JPEG XL build tool version: ' + name + ' (numeric=' + numeric + ')')
                    self.assertNotIn(secret, str(caught.exception))
                    self.assertNotIn(str(executable), str(caught.exception))
                    self.assertLess(len(str(caught.exception)), 100)

    def test_configure_selects_two_jobs_and_matching_native_compilers(self):
        with mock.patch.object(jxl_build.platform, 'system', return_value='Windows'), mock.patch.object(jxl_build, 'tool', side_effect=[
                ('C:/fixture/clang-cl.exe', '21.1.8'), ('C:/fixture/clang-cl.exe', '21.1.8'),
                ('C:/fixture/cmake.exe', '3.31.12'), ('C:/fixture/ninja.exe', '1.13.2')]):
            environment, versions = jxl_build.configure({'PATH': 'original', 'INCLUDE': 'fixture headers', 'LIB': 'fixture libraries'})
        self.assertEqual(environment['CC'], environment['CXX'])
        self.assertEqual(environment['CMAKE_GENERATOR'], 'Ninja')
        self.assertEqual(environment['CARGO_BUILD_JOBS'], '2')
        self.assertEqual(environment['CMAKE_BUILD_PARALLEL_LEVEL'], '2')
        self.assertEqual(versions['compiler'], '21.1.8')
        with mock.patch.object(jxl_build.platform, 'system', return_value='Windows'), mock.patch.object(jxl_build, 'tool', side_effect=[
                ('cc', '20.1.8'), ('cxx', '21.1.8')]):
            with self.assertRaisesRegex(ValueError, 'differ'):
                jxl_build.configure({})
        with mock.patch.object(jxl_build.platform, 'system', return_value='unsupported'):
            with self.assertRaisesRegex(ValueError, 'not configured'):
                jxl_build.configure({})

    def test_selected_ninja_stays_discoverable_when_other_tool_directories_shadow_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            ninja_directory, compiler_directory, cmake_directory = [root / name for name in
                ['selected ninja', 'compiler tools', 'cmake tools']]
            suffix = '.exe' if os.name == 'nt' else ''
            for directory in [ninja_directory, compiler_directory, cmake_directory]:
                directory.mkdir()
                executable = directory / ('ninja' + suffix)
                executable.write_text('1.13.2' if directory == ninja_directory else '1.12.0', encoding='utf-8')
                executable.chmod(0o755)
            selected = ninja_directory / ('ninja' + suffix)
            cc, cmake = compiler_directory / ('clang-cl' + suffix), cmake_directory / ('cmake' + suffix)
            original = os.pathsep.join(map(str, [ninja_directory, compiler_directory, cmake_directory]))
            self.assertEqual(Path(jxl_build.shutil.which('ninja', path=original)), selected)
            shadowed = os.pathsep.join(map(str, [compiler_directory, cmake_directory, ninja_directory]))
            self.assertEqual(Path(jxl_build.shutil.which('ninja', path=shadowed)), compiler_directory / ('ninja' + suffix))
            with mock.patch.object(jxl_build.platform, 'system', return_value='Windows'), \
                    mock.patch.object(jxl_build, 'tool', side_effect=[
                        (str(cc), '20.1.8'), (str(cc), '20.1.8'),
                        (str(cmake), '4.4.4'), (str(selected), '1.13.2')]), \
                    mock.patch.object(jxl_build.subprocess, 'run', side_effect=AssertionError('No tool execution')):
                environment, versions = jxl_build.configure({'PATH': original, 'INCLUDE': 'fixture', 'LIB': 'fixture'})
            self.assertEqual(Path(jxl_build.shutil.which('ninja', path=environment['PATH'])), selected)
            self.assertEqual(environment['CC'], str(cc))
            self.assertEqual(environment['CXX'], str(cc))
            self.assertEqual(environment['CMAKE'], str(cmake))
            self.assertEqual(versions['ninja'], '1.13.2')

    def test_github_export_contains_only_build_values_and_rejects_multiline(self):
        with tempfile.TemporaryDirectory() as temporary, mock.patch.object(jxl_build.platform, 'system', return_value='Windows'):
            destination = Path(temporary) / 'step.env'
            environment = {'CC': 'cc', 'CXX': 'cxx', 'CMAKE': 'cmake', 'PATH': 'fixture-path',
                           'INCLUDE': 'headers', 'LIB': 'libraries', 'LIBPATH': 'sdk',
                           'UNRELATED_FIXTURE': 'must not be exported'}
            jxl_build.write_github_environment(destination, environment)
            before = destination.read_bytes()
            self.assertNotIn(b'UNRELATED_FIXTURE', before)
            self.assertIn(b'INCLUDE=headers', before)
            environment['PATH'] = 'line\nINJECTED=1'
            with self.assertRaisesRegex(ValueError, 'multiline'):
                jxl_build.write_github_environment(destination, environment)
            self.assertEqual(destination.read_bytes(), before)

    def test_exact_nested_native_notices_and_gpl_text_are_copied_from_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            directory = 'vendor/jpegxl-src'
            names = ['LICENSE', 'libjxl/LICENSE', 'libjxl/third_party/highway/LICENSE',
                     'libjxl/third_party/highway/LICENSE-BSD3', 'libjxl/third_party/brotli/LICENSE',
                     'libjxl/third_party/skcms/LICENSE']
            for name in names:
                file = root / directory / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text('Synthetic corresponding notice: ' + name, encoding='utf-8')
            (root / directory / 'Cargo.toml').write_text('[package]\nname="jpegxl-src"\nversion="0.12.0"\nlicense="BSD-3-Clause"\n', encoding='utf-8')
            (root / 'Cargo.lock').write_text('package=[]\n', encoding='utf-8')
            (root / 'LICENSE').write_text('Synthetic GPL-3.0 text', encoding='utf-8')
            (root / 'assets/fonts').mkdir(parents=True)
            (root / 'assets/fonts/LICENCE.txt').write_text('Synthetic font notice', encoding='utf-8')
            policy = {'vendored': [directory], 'source': [directory + '/' + name for name in names], 'rust_notices': []}
            bundle = root / 'bundle'
            bundle.mkdir()
            packaging.dependency_notices(root, bundle, root / 'registry', policy, root)
            for name in names:
                self.assertEqual((bundle / 'licenses/jpegxl-src-0.12.0' / name).read_bytes(),
                                 (root / directory / name).read_bytes())
            self.assertEqual((bundle / 'licenses/jpegxl-src-0.12.0/GPL-3.0.txt').read_bytes(),
                             (root / 'LICENSE').read_bytes())
            policy['source'].remove(directory + '/libjxl/third_party/skcms/LICENSE')
            with self.assertRaisesRegex(ValueError, 'Missing committed JPEG XL'):
                packaging.dependency_notices(root, bundle, root / 'registry', policy, root)

    def test_candidate_workflows_pin_rust_and_native_setup_before_compilation(self):
        root = Path(__file__).resolve().parents[1]
        for name in ['ci.yml', 'release.yml', 'macos-qualification.yml']:
            text = (root / '.github/workflows' / name).read_text(encoding='utf-8')
            with self.subTest(workflow=name):
                self.assertIn('toolchain: 1.92.0', text)
                self.assertNotIn('toolchain: stable', text)
                setup = text.index('scripts/jxl_build.py --github-env')
                self.assertLess(setup, text.index('dtolnay/rust-toolchain@'))
                self.assertIn('-Arch amd64 -HostArch amd64 -SkipAutomaticLocation', text)
                self.assertIn("@('ninja', 'clang-cl', 'cmake')", text)
                self.assertIn('b725eda525362e7cf34af9a7243cac1e3f01a7ec', text)

    def test_ordinary_cargo_graph_requires_vendored_source_and_explicit_msrv(self):
        root = Path(__file__).resolve().parents[1]
        cargo = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))
        self.assertEqual(cargo['package']['rust-version'], '1.92')
        self.assertEqual(cargo['dependencies']['jpegxl-sys']['version'], '=0.13.0')
        self.assertEqual(cargo['dependencies']['jpegxl-sys']['features'], ['vendored'])
        self.assertEqual(cargo['patch']['crates-io']['jpegxl-src']['path'], 'vendor/jpegxl-src')
        toolchain = tomllib.loads((root / 'rust-toolchain.toml').read_text(encoding='utf-8'))
        self.assertEqual(toolchain['toolchain']['channel'], '1.92.0')
        helper = (root / 'vendor/jpegxl-src/src/lib.rs').read_text(encoding='utf-8')
        self.assertNotIn('DEP_JXL_PATH', helper)
        self.assertIn('Path::new(env!("CARGO_MANIFEST_DIR")).join("libjxl")', helper)
        self.assertIn('env::var("NUM_JOBS")', helper)

    def test_headless_diagnostic_is_after_regressions_and_binds_exact_clean_source(self):
        text = (Path(__file__).resolve().parents[1] / '.github/workflows/ci.yml').read_text(encoding='utf-8')
        baseline = text.index('name: Record headless workspace CPU component baseline')
        self.assertGreater(baseline, text.index('name: Run regression tests and summarize failures'))
        self.assertIn('ui::input_performance_diagnostic::actual_workspace_control_and_navigation_components -- --exact --ignored --nocapture --test-threads=1', text)
        self.assertIn('"$(git rev-parse HEAD)"', text)
        self.assertIn("rows[0]['build'].get('tree') != 'clean'", text)
        self.assertIn('caches/scheduling uncontrolled', text)


if __name__ == '__main__':
    unittest.main()
