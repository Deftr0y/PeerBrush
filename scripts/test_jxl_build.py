"""Offline selection/privacy/notices; native build tools never execute."""
import importlib.util
import base64
import json
import os
import re
import runpy
import shlex
import sys
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
    def test_native_python_pin_reuses_one_existing_interpreter_before_tool_path_shadowing(self):
        import select_native_python
        import shutil
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            selected = root / 'selected Python'
            tools = root / 'native tools'
            selected.mkdir()
            tools.mkdir()
            executable = selected / 'python3.13'
            executable.write_bytes(b'synthetic selected interpreter; never executed')
            executable.chmod(0o755)
            for name in ['python', 'python3']:
                os.link(executable, selected / name)
            shadow = tools / 'python3'
            shadow.write_bytes(b'synthetic old interpreter; never executed')
            shadow.chmod(0o755)
            destination = root / 'github-path'
            destination.write_text('existing entry\n', encoding='utf-8')
            with mock.patch.object(select_native_python.sys, 'executable', str(executable)), \
                 mock.patch.object(select_native_python.sys, 'version_info', (3, 13, 2)), \
                 mock.patch.dict(os.environ, {'GITHUB_PATH': str(destination)}):
                select_native_python.main()
            self.assertEqual(destination.read_text(encoding='utf-8'), 'existing entry\n' + str(selected) + '\n')
            # GitHub prepends GITHUB_PATH entries for subsequent steps, ahead of exported PATH.
            changed = os.pathsep.join([str(tools), str(selected)])
            environment_destination = root / 'github-env'
            jxl_build.write_github_environment(environment_destination, dict(PATH=changed))
            exported = dict(row.split('=', 1) for row in environment_destination.read_text(encoding='utf-8').splitlines())['PATH']
            self.assertEqual(exported, changed)
            pinned = os.pathsep.join([*reversed(destination.read_text(encoding='utf-8').splitlines()), exported])
            with mock.patch.object(sys, 'platform', 'darwin'):
                self.assertEqual(Path(shutil.which('python3', path=changed)), shadow)
                self.assertTrue(Path(shutil.which('python3', path=pinned)).samefile(executable))
                self.assertTrue(Path(shutil.which('python', path=pinned)).samefile(executable))

    def test_native_python_pin_rejects_old_or_mismatched_runtime_before_environment_write(self):
        import select_native_python
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            executable = root / 'python'
            executable.write_bytes(b'synthetic interpreter')
            (root / 'python3').write_bytes(b'different interpreter')
            destination = root / 'github-path'
            destination.write_bytes(b'prior environment\n')
            with mock.patch.object(select_native_python.sys, 'executable', str(executable)), \
                 mock.patch.dict(os.environ, {'GITHUB_PATH': str(destination)}):
                for version in [(3, 10, 9), (3, 13, 2)]:
                    with self.subTest(version=version), \
                         mock.patch.object(select_native_python.sys, 'version_info', version), \
                         self.assertRaises(ValueError):
                        select_native_python.main()
                self.assertEqual(destination.read_bytes(), b'prior environment\n')

    def test_macos_native_python_pin_precedes_tool_setup_and_preserves_qualification_guards(self):
        root = Path(__file__).resolve().parents[1]
        pin = 'run: python -I scripts/select_native_python.py'
        for name in ['ci.yml', 'release.yml', 'macos-qualification.yml']:
            text = (root / '.github/workflows' / name).read_text(encoding='utf-8')
            with self.subTest(workflow=name):
                expected = 2 if name == 'macos-qualification.yml' else 1
                self.assertEqual(text.count(pin), expected)
                self.assertLess(text.index(pin), text.index('scripts/jxl_build.py --github-env'))
                if name == 'macos-qualification.yml':
                    for section in text.split('    steps:\n')[1:]:
                        self.assertLess(section.index('Require reviewed main invocation'), section.index('Checkout reviewed main source'))
                        self.assertLess(section.index('Checkout reviewed main source'), section.index(pin))
                        self.assertLess(section.index(pin), section.index('Require exact main source'))
                    self.assertLess(text.index('name: Qualify selected final DMG'), text.index('name: Attest generated native qualification'))

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
                with self.subTest(selection_override=name), self.assertRaisesRegex(ValueError, 'overrides'):
                    jxl_build.configure({name: value},
                        selected=dict.fromkeys(['clang-cl', 'cmake', 'ninja'], Path.cwd()))

    def test_native_shell_flags_match_locked_cc_comments_quotes_and_escapes(self):
        from build_release import native_shell_flags
        cases = [
            ('# ignored malformed quote "', []),
            ('foo#bar # ignored', ['foo#bar']),
            ("''#suffix \"\" '#quoted' \\#escaped", ['#suffix', '', '#quoted', '#escaped']),
            ("one' two'\" three\"", ['one two three']),
            ('"-DVALUE=\\$cash\\`tick"', ['-DVALUE=$cash`tick']),
            ('"back\\slash" "quote\\\"tail" "end\\\\"', ['back\\slash', 'quote"tail', 'end\\']),
            ('escaped\\ space plain\tword', ['escaped space', 'plain', 'word']),
        ]
        for flags, expected in cases:
            with self.subTest(case=flags):
                self.assertEqual(native_shell_flags(flags), expected)
        for flags in ["'unclosed", '"unclosed', 'trailing\\']:
            with self.subTest(malformed=flags), self.assertRaises(ValueError):
                native_shell_flags(flags)

    def test_native_flags_keep_prior_cc_mode_for_standard_and_target_families(self):
        for mode in ['', '0', 'false', 'no', '1', 'true', 'False', 'NO']:
            shell = mode not in ['', '0', 'false', 'no']
            expected = ['-DKEEP=C:\\synthetic\\path', '-DQUOTED="literal"']
            if shell:
                expected += ['-DSPACE=two words', "-DAPOSTROPHE=one'two"]
            raw = shlex.join(expected) if shell else ' '.join(expected)
            inherited = {'CC_SHELL_ESCAPED_FLAGS': mode}
            for family in ['CFLAGS', 'CXXFLAGS', 'ARFLAGS', 'RANLIBFLAGS']:
                for name in [family, 'HOST_' + family, 'TARGET_' + family,
                             family + '_x86_64-unknown-linux-gnu', family + '_x86_64_unknown_linux_gnu']:
                    inherited[name] = raw
            original = dict(inherited)
            with self.subTest(mode=mode), mock.patch('build_release.source_input_state', return_value='unknown'):
                result = release_environment(Path('synthetic checkout'), inherited)
            self.assertEqual(inherited, original)
            self.assertEqual(result['CC_SHELL_ESCAPED_FLAGS'], '1')
            for name in original:
                if name == 'CC_SHELL_ESCAPED_FLAGS':
                    continue
                arguments = shlex.split(result[name], comments=False, posix=True)
                self.assertEqual(arguments[:len(expected)], expected)
                if name not in ['CFLAGS', 'CXXFLAGS']:
                    self.assertEqual(arguments, expected)

    def test_native_flags_reject_malformed_shell_and_control_values_without_mutating_input(self):
        variants = [('CFLAGS', '"unterminated', 'true'),
                    ('TARGET_CXXFLAGS', "'unterminated", 'False')]
        variants += [(name, '-DINVALID=' + character, mode)
                     for name in ['CFLAGS', 'CXXFLAGS', 'HOST_ARFLAGS', 'RANLIBFLAGS_target']
                     for character in ['\r', '\n', '\0', '\x1b', '\x7f'] for mode in ['0', '1']]
        for name, flags, mode in variants:
            inherited = {name: flags, 'CC_SHELL_ESCAPED_FLAGS': mode}
            original = dict(inherited)
            with self.subTest(name=name, mode=mode), mock.patch('build_release.source_input_state', return_value='unknown'), self.assertRaises(ValueError):
                release_environment(Path('synthetic checkout'), inherited)
            self.assertEqual(inherited, original)

    def test_native_remapping_preserves_existing_flags_and_rejects_line_injection(self):
        with mock.patch('build_release.source_input_state', return_value='unknown'):
            environment = release_environment(Path('synthetic checkout'),
                {'CFLAGS': '-DREVIEWED=1', 'CXXFLAGS': '-DREVIEWED_CPP=1'})
            self.assertTrue(environment['CFLAGS'].startswith('-DREVIEWED=1 '))
            self.assertTrue(environment['CXXFLAGS'].startswith('-DREVIEWED_CPP=1 '))
            for name in ['CFLAGS', 'CXXFLAGS']:
                self.assertIn('-ffile-prefix-map=', environment[name])
                arguments = shlex.split(environment[name], comments=False, posix=True)
                self.assertTrue(any(argument.endswith('=peerbrush') for argument in arguments))
                self.assertEqual(environment['CC_SHELL_ESCAPED_FLAGS'], '1')
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

    @unittest.skipUnless(os.name == 'nt', 'Actual PowerShell application discovery requires Windows')
    def test_actual_powershell_capture_keeps_one_precedence_path_and_all_cli_flags(self):
        host = jxl_build.shutil.which('pwsh')
        if not host:
            self.skipTest('PowerShell 7 host is unavailable')
        root = Path(__file__).resolve().parents[1]
        workflow = (root / '.github/workflows/ci.yml').read_text(encoding='utf-8')
        begin = workflow.index('          $nativeTools = @{}')
        end = workflow.index('          $vswhere = ', begin)
        capture = '\n'.join(line[10:] for line in workflow[begin:end].splitlines())
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary).resolve()
            first, second = parent / 'first tools', parent / 'second tools'
            for directory in [first, second]:
                directory.mkdir()
                for name in ['clang-cl', 'cmake', 'ninja']:
                    (directory / (name + '.exe')).write_bytes(b'Never execute this discovery fixture')
            receipt = parent / 'argument-receipt.py'
            receipt.write_text('''import argparse, json, os
from pathlib import Path
p = argparse.ArgumentParser()
for name in ['clang-cl', 'cmake', 'ninja']:
    p.add_argument('--selected-' + name)
p.add_argument('cargo_args', nargs=argparse.REMAINDER)
a = p.parse_args()
print(json.dumps(dict(
    uncapped=json.loads(os.environ['FIXTURE_UNCAPPED']),
    scalar=json.loads(os.environ['FIXTURE_SCALARS']),
    first=[getattr(a, 'selected_' + name.replace('-', '_')) == str(Path(os.environ['FIXTURE_FIRST']) / (name + '.exe'))
           for name in ['clang-cl', 'cmake', 'ninja']],
    remainder=len(a.cargo_args))))
''', encoding='utf-8', newline='\n')
            script = """$ErrorActionPreference = 'Stop'
$env:PATH = $env:FIXTURE_NATIVE_PATH
$uncapped = @{}
foreach ($name in @('clang-cl', 'cmake', 'ninja')) {
    $uncapped[$name] = @(Get-Command $name -CommandType Application -ErrorAction Stop).Count
}
"""
            script += capture + '\n'
            script += """$env:FIXTURE_UNCAPPED = ($uncapped | ConvertTo-Json -Compress)
$env:FIXTURE_SCALARS = (@('clang-cl', 'cmake', 'ninja') | ForEach-Object { $nativeTools[$_] -is [string] } | ConvertTo-Json -Compress)
& $env:FIXTURE_PYTHON -I $env:FIXTURE_SCRIPT --selected-clang-cl ($nativeTools['clang-cl']) --selected-cmake ($nativeTools['cmake']) --selected-ninja ($nativeTools['ninja'])
exit $LASTEXITCODE
"""
            environment = dict(os.environ, FIXTURE_NATIVE_PATH=os.pathsep.join(map(str, [first, second])),
                FIXTURE_PYTHON=sys.executable, FIXTURE_SCRIPT=str(receipt), FIXTURE_FIRST=str(first))
            result = subprocess.run([str(Path(host).resolve()), '-NoLogo', '-NoProfile', '-NonInteractive',
                '-EncodedCommand', base64.b64encode(script.encode('utf-16le')).decode('ascii')],
                cwd=parent, env=environment, capture_output=True, check=False, timeout=20)
            self.assertEqual(result.returncode, 0, 'Isolated PowerShell capture/argv fixture failed')
            self.assertLessEqual(len(result.stdout), 4096)
            self.assertEqual(json.loads(result.stdout), dict(
                uncapped={'clang-cl': 2, 'cmake': 2, 'ninja': 2},
                scalar=[True, True, True], first=[True, True, True], remainder=0))

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

    def test_exact_captured_tools_survive_sdk_and_reciprocal_path_shadowing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            cmake_directory, ninja_directory, compiler_directory, sdk_directory = [
                root / name for name in ['selected cmake', 'selected ninja', 'selected compiler', 'sdk tools']]
            suffix = '.exe' if os.name == 'nt' else ''
            versions = {
                cmake_directory: {'cmake': 'cmake version 4.4.4\n'},
                ninja_directory: {'ninja': '1.13.2\n', 'cmake': 'cmake version 3.29.2\n'},
                compiler_directory: {'clang-cl': 'clang version 21.1.8\n', 'ninja': '1.12.0\n'},
                sdk_directory: {'clang-cl': 'clang version 19.0.0\n',
                                'cmake': 'cmake version 3.29.2\n', 'ninja': '1.12.0\n'},
            }
            for directory, tools in versions.items():
                directory.mkdir()
                for name, version in tools.items():
                    executable = directory / (name + suffix)
                    executable.write_bytes(version.encode())
                    executable.chmod(0o755)
            original = os.pathsep.join(map(str, [cmake_directory, ninja_directory, compiler_directory]))
            selected = {name: Path(jxl_build.shutil.which(name, path=original)).resolve()
                        for name in ['clang-cl', 'cmake', 'ninja']}
            self.assertEqual(selected, {'clang-cl': compiler_directory / ('clang-cl' + suffix),
                                        'cmake': cmake_directory / ('cmake' + suffix),
                                        'ninja': ninja_directory / ('ninja' + suffix)})
            clang_first = os.pathsep.join(map(str, [compiler_directory, cmake_directory, ninja_directory]))
            ninja_first = os.pathsep.join(map(str, [ninja_directory, compiler_directory, cmake_directory]))
            self.assertNotEqual(Path(jxl_build.shutil.which('ninja', path=clang_first)), selected['ninja'])
            self.assertNotEqual(Path(jxl_build.shutil.which('cmake', path=ninja_first)), selected['cmake'])
            inherited = {'PATH': str(sdk_directory) + os.pathsep + original,
                         'INCLUDE': 'synthetic headers', 'LIB': 'synthetic libraries'}
            for name in selected:
                self.assertEqual(Path(jxl_build.shutil.which(name, path=inherited['PATH'])),
                                 sdk_directory / (name + suffix))
            calls = []
            def version(arguments, **options):
                self.assertIn(Path(arguments[0]), selected.values())
                self.assertEqual(arguments[1:], ['--version'])
                self.assertEqual(set(options), {'env', 'capture_output', 'check', 'timeout'})
                self.assertTrue(options['capture_output'])
                self.assertTrue(options['check'])
                self.assertEqual(options['timeout'], 10)
                calls.append(arguments[0])
                return subprocess.CompletedProcess(arguments, 0, Path(arguments[0]).read_bytes(), b'')
            with mock.patch.object(jxl_build.platform, 'system', return_value='Windows'), \
                    mock.patch.object(jxl_build.subprocess, 'run', side_effect=version):
                environment, qualified = jxl_build.configure(inherited, selected=selected)
                arguments = [jxl_build.__file__]
                for name, path in selected.items():
                    arguments += ['--selected-' + name, str(path)]
                with mock.patch.dict(os.environ, inherited, clear=True), \
                        mock.patch.object(sys, 'argv', arguments), mock.patch('builtins.print'):
                    cli = runpy.run_path(jxl_build.__file__, run_name='__main__')
                self.assertEqual(cli['environment']['CMAKE_MAKE_PROGRAM'], str(selected['ninja']))
                with mock.patch('build_release.source_input_state', return_value='unknown'):
                    release = release_environment(root, environment)
                repeated, repeated_versions = jxl_build.configure(release)
            expected = [str(selected[name]) for name in ['clang-cl', 'clang-cl', 'cmake', 'ninja']]
            self.assertEqual(calls, expected * 3)
            self.assertEqual(qualified, {'compiler': '21.1.8', 'cmake': '4.4.4', 'ninja': '1.13.2'})
            self.assertEqual(repeated_versions, qualified)
            for configured in [environment, repeated]:
                self.assertEqual(configured['CC'], str(selected['clang-cl']))
                self.assertEqual(configured['CXX'], str(selected['clang-cl']))
                self.assertEqual(configured['CMAKE'], str(selected['cmake']))
                self.assertEqual(configured['CMAKE_MAKE_PROGRAM'], str(selected['ninja']))
                self.assertEqual(Path(jxl_build.shutil.which('ninja', path=configured['PATH'])),
                                 selected['ninja'])
                # CMake is deliberately shadowed by Ninja's directory; only its exact binding is safe.
                self.assertNotEqual(Path(jxl_build.shutil.which('cmake', path=configured['PATH'])),
                                    selected['cmake'])

    def test_repeated_qualification_keeps_separate_unix_compilers_and_all_exact_paths(self):
        cases = [('Linux', 'clang-18', 'clang++-18', '18.1.3', '3.31.6'),
                 ('Darwin', 'clang', 'clang++', '21.0.0', '4.4.4')]
        for system, cc_name, cxx_name, compiler_version, cmake_version in cases:
            with self.subTest(system=system), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                cmake_directory, ninja_directory, compiler_directory, sdk_directory = [
                    root / name for name in ['cmake', 'ninja', 'compiler', 'sdk']]
                suffix = '.exe' if os.name == 'nt' else ''
                versions = {
                    cmake_directory: {'cmake': 'cmake version ' + cmake_version + '\n'},
                    ninja_directory: {'ninja': '1.13.2\n', 'cmake': 'cmake version 3.29.2\n'},
                    compiler_directory: {cc_name: 'clang version ' + compiler_version + '\n',
                                         cxx_name: 'clang version ' + compiler_version + '\n',
                                         'ninja': '1.12.0\n'},
                    sdk_directory: {cc_name: 'clang version 19.0.0\n', cxx_name: 'clang version 19.0.0\n',
                                    'cmake': 'cmake version 3.29.2\n', 'ninja': '1.12.0\n'},
                }
                for directory, tools in versions.items():
                    directory.mkdir()
                    for name, version in tools.items():
                        path = directory / (name + suffix)
                        path.write_bytes(version.encode())
                        path.chmod(0o755)
                original = os.pathsep.join(map(str, [cmake_directory, ninja_directory, compiler_directory]))
                expected = [compiler_directory / (name + suffix) for name in [cc_name, cxx_name]]
                expected += [cmake_directory / ('cmake' + suffix), ninja_directory / ('ninja' + suffix)]
                self.assertEqual([Path(jxl_build.shutil.which(name, path=original)).resolve()
                                  for name in [cc_name, cxx_name, 'cmake', 'ninja']], expected)
                calls = []
                def version(arguments, **options):
                    self.assertIn(Path(arguments[0]), expected)
                    self.assertEqual(arguments[1:], ['--version'])
                    calls.append(arguments[0])
                    return subprocess.CompletedProcess(arguments, 0, Path(arguments[0]).read_bytes(), b'')
                with mock.patch.object(jxl_build.platform, 'system', return_value=system), \
                        mock.patch.object(jxl_build.subprocess, 'run', side_effect=version):
                    # A partial inherited selection must not be mixed with discovered tools.
                    environment, qualified = jxl_build.configure({'PATH': original, 'CC': 'ignored partial'})
                    environment['PATH'] = str(sdk_directory) + os.pathsep + environment['PATH']
                    for name in [cc_name, cxx_name, 'cmake', 'ninja']:
                        self.assertEqual(Path(jxl_build.shutil.which(name, path=environment['PATH'])),
                                         sdk_directory / (name + suffix))
                    with mock.patch('build_release.source_input_state', return_value='unknown'):
                        repeated, repeated_versions = jxl_build.configure(release_environment(root, environment))
                self.assertEqual(calls, list(map(str, expected)) * 2)
                self.assertEqual(qualified, repeated_versions)
                for key, path in zip(['CC', 'CXX', 'CMAKE', 'CMAKE_MAKE_PROGRAM'], expected):
                    self.assertEqual(repeated[key], str(path))
                self.assertNotEqual(repeated['CC'], repeated['CXX'])
                self.assertEqual(Path(jxl_build.shutil.which('ninja', path=repeated['PATH'])), expected[3])

    def test_explicit_tool_group_is_complete_absolute_windows_only_and_requalified(self):
        with mock.patch.object(jxl_build.platform, 'system', return_value='Windows'), \
                mock.patch.object(jxl_build.subprocess, 'run') as run:
            for selected in [{}, {'clang-cl': Path('relative')},
                             dict.fromkeys(['clang-cl', 'cmake', 'ninja']),
                             {'clang-cl': None, 'cmake': Path.cwd(), 'ninja': Path.cwd()},
                             {'clang-cl': Path.cwd(), 'cmake': Path.cwd(), 'unexpected': Path.cwd()},
                             {'clang-cl': Path('relative'), 'cmake': Path.cwd(), 'ninja': Path.cwd()}]:
                with self.subTest(selection=sorted(selected)), self.assertRaises(ValueError):
                    jxl_build.configure({}, selected=selected)
            with self.assertRaisesRegex(ValueError, 'absolute'):
                jxl_build.configure({'CC': 'one', 'CXX': 'two', 'CMAKE': 'three',
                                     'CMAKE_MAKE_PROGRAM': 'four'})
            with self.assertRaisesRegex(ValueError, 'selections differ'):
                jxl_build.configure({'CC': str(Path.cwd() / 'one.exe'),
                                     'CXX': str(Path.cwd() / 'two.exe'),
                                     'CMAKE': str(Path.cwd() / 'cmake.exe'),
                                     'CMAKE_MAKE_PROGRAM': str(Path.cwd() / 'ninja.exe')})
            run.assert_not_called()
        with mock.patch.object(jxl_build.platform, 'system', return_value='Linux'), \
                mock.patch.object(jxl_build.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'Windows'):
                jxl_build.configure({}, selected=dict.fromkeys(['clang-cl', 'cmake', 'ninja'], Path.cwd()))
            run.assert_not_called()

    def test_exact_tool_probe_failures_do_not_expose_private_paths_or_output(self):
        sentinel = 'PRIVATE_SELECTED_TOOL_PATH_STDERR'
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / sentinel
            executable.write_bytes(b'never executed')
            errors = [FileNotFoundError(sentinel),
                      subprocess.CalledProcessError(9, [str(executable)], stderr=sentinel),
                      subprocess.TimeoutExpired([str(executable)], 10, output=sentinel)]
            for error in errors:
                with self.subTest(error=type(error).__name__), \
                        mock.patch.object(jxl_build.subprocess, 'run', side_effect=error):
                    with self.assertRaises(ValueError) as caught:
                        jxl_build.tool('ninja', 'ninja', {'1.13.2'}, {}, selected=executable)
                    self.assertEqual(str(caught.exception), 'Native JPEG XL build tool unavailable: ninja')
                    self.assertNotIn(sentinel, str(caught.exception))
            with mock.patch.object(jxl_build.subprocess, 'run',
                                   return_value=subprocess.CompletedProcess([], 0, b'\xff', b'')):
                with self.assertRaisesRegex(ValueError, '^Native build tool version output is not UTF-8: ninja$'):
                    jxl_build.tool('ninja', 'ninja', {'1.13.2'}, {}, selected=executable)
            with mock.patch.object(jxl_build.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, '^Native JPEG XL build tool unavailable: ninja$'):
                    jxl_build.tool('ninja', 'ninja', {'1.13.2'}, {}, selected=executable.parent)
                run.assert_not_called()

    def test_github_export_contains_only_build_values_and_rejects_multiline(self):
        with tempfile.TemporaryDirectory() as temporary, mock.patch.object(jxl_build.platform, 'system', return_value='Windows'):
            destination = Path(temporary) / 'step.env'
            environment = {'CC': 'cc', 'CXX': 'cxx', 'CMAKE': 'cmake', 'CMAKE_MAKE_PROGRAM': 'selected ninja', 'PATH': 'fixture-path',
                           'INCLUDE': 'headers', 'LIB': 'libraries', 'LIBPATH': 'sdk',
                           'UNRELATED_FIXTURE': 'must not be exported'}
            jxl_build.write_github_environment(destination, environment)
            before = destination.read_bytes()
            self.assertNotIn(b'UNRELATED_FIXTURE', before)
            self.assertIn(b'INCLUDE=headers', before)
            self.assertIn(b'CMAKE_MAKE_PROGRAM=selected ninja\n', before)
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
            cmake_directory = root / 'vendor/cmake'
            cmake_directory.mkdir(parents=True)
            (cmake_directory / 'Cargo.toml').write_text('[package]\nname="cmake"\nversion="0.1.58"\nlicense="MIT OR Apache-2.0"\n', encoding='utf-8')
            for name in ['LICENSE-MIT', 'LICENSE-APACHE']:
                (cmake_directory / name).write_text('Synthetic CMake corresponding notice: ' + name, encoding='utf-8')
            policy['vendored'].append('vendor/cmake')
            policy['source'] += ['vendor/cmake/' + name for name in ['LICENSE-MIT', 'LICENSE-APACHE']]
            bundle = root / 'bundle'
            bundle.mkdir()
            packaging.dependency_notices(root, bundle, root / 'registry', policy, root)
            for name in names:
                self.assertEqual((bundle / 'licenses/jpegxl-src-0.12.0' / name).read_bytes(),
                                 (root / directory / name).read_bytes())
            self.assertEqual((bundle / 'licenses/jpegxl-src-0.12.0/GPL-3.0.txt').read_bytes(),
                             (root / 'LICENSE').read_bytes())
            for name in ['LICENSE-MIT', 'LICENSE-APACHE']:
                self.assertEqual((bundle / 'licenses/cmake-0.1.58' / name).read_bytes(), (cmake_directory / name).read_bytes())
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
                fixture = 'run: python -B scripts/test_jxl_build.py'
                self.assertEqual(text.count(fixture), 1)
                self.assertLess(text.index('run: python3 -B scripts/jxl_build.py --github-env'), text.index(fixture))
                self.assertLess(text.index(fixture), text.index('dtolnay/rust-toolchain@'))
                self.assertIn('-Arch amd64 -HostArch amd64 -SkipAutomaticLocation', text)
                capture = text.index("$nativeTools = @{}")
                sdk = text.index('& $launch -Arch amd64')
                self.assertLess(capture, sdk)
                self.assertLess(text.index("$nativeTools[$name] = (Get-Command $name -CommandType Application -TotalCount 1 -ErrorAction Stop).Source"), sdk)
                self.assertLess(sdk, setup)
                self.assertIn("@('clang-cl', 'cmake', 'ninja')", text)
                for name in ['clang-cl', 'cmake', 'ninja']:
                    self.assertIn("--selected-" + name + " ($nativeTools['" + name + "'])", text)
                self.assertNotIn('$nativeToolDirectories', text)
                self.assertIn('b725eda525362e7cf34af9a7243cac1e3f01a7ec', text)

    def test_ordinary_cargo_graph_requires_vendored_source_and_explicit_msrv(self):
        root = Path(__file__).resolve().parents[1]
        cargo = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))
        self.assertEqual(cargo['package']['rust-version'], '1.92')
        self.assertEqual(cargo['dependencies']['jpegxl-sys']['version'], '=0.13.0')
        self.assertEqual(cargo['dependencies']['jpegxl-sys']['features'], ['vendored'])
        self.assertEqual(cargo['patch']['crates-io']['jpegxl-src']['path'], 'vendor/jpegxl-src')
        self.assertEqual(cargo['patch']['crates-io']['cmake']['path'], 'vendor/cmake')
        cmake = tomllib.loads((root / 'vendor/cmake/Cargo.toml').read_text(encoding='utf-8'))
        self.assertEqual(cmake['package']['version'], '0.1.58')
        self.assertEqual((root / 'vendor/cmake/src/lib.rs').read_text(encoding='utf-8').count('push_quoted_flag(&mut flagsflag, arg);'), 2)
        toolchain = tomllib.loads((root / 'rust-toolchain.toml').read_text(encoding='utf-8'))
        self.assertEqual(toolchain['toolchain']['channel'], '1.92.0')
        helper = (root / 'vendor/jpegxl-src/src/lib.rs').read_text(encoding='utf-8')
        self.assertNotIn('DEP_JXL_PATH', helper)
        self.assertIn('Path::new(env!("CARGO_MANIFEST_DIR")).join("libjxl")', helper)
        self.assertIn('env::var("NUM_JOBS")', helper)
        self.assertIn('cargo:rerun-if-env-changed=CMAKE_MAKE_PROGRAM', helper)
        self.assertIn('if env::var("CMAKE_GENERATOR").as_deref() == Ok("Ninja")', helper)
        self.assertIn('env::var_os("CMAKE_MAKE_PROGRAM")', helper)
        self.assertIn('config.define("CMAKE_MAKE_PROGRAM", program)', helper)

    def test_headless_diagnostic_is_after_regressions_and_binds_exact_clean_source(self):
        text = (Path(__file__).resolve().parents[1] / '.github/workflows/ci.yml').read_text(encoding='utf-8')
        baseline = text.index('name: Record headless workspace CPU component baseline')
        self.assertGreater(baseline, text.index('name: Run regression tests and summarize failures'))
        self.assertIn('ui::input_performance_diagnostic::actual_workspace_control_and_navigation_components -- --exact --ignored --nocapture --test-threads=1', text)
        self.assertIn('"$(git rev-parse HEAD)"', text)
        self.assertIn("rows[0]['build'].get('tree') != 'clean'", text)
        self.assertIn('caches/scheduling uncontrolled', text)

    def test_ci_cache_saves_after_main_without_bypassing_tests_or_release_source(self):
        root = Path(__file__).resolve().parents[1] / '.github/workflows'
        text = (root / 'ci.yml').read_text(encoding='utf-8')
        pin = 'caa296126883cff596d87d8935842f9db880ef25'
        restore = text.index('      - name: Restore Rust dependencies and build outputs')
        build = text.index('      - name: Build source-matched JPEG XL worker for integration tests')
        save = text.index('      - name: Save CI build cache after main compilation')
        regression = text.index('      - name: Run regression tests and summarize failures')
        self.assertLess(restore, build)
        self.assertLess(build, save)
        self.assertLess(save, regression)
        restore_block = text[restore:text.index("      - if: runner.os == 'Linux'", restore)]
        save_block = text[save:regression]
        self.assertEqual(text.count('actions/cache/restore@' + pin), 1)
        self.assertEqual(text.count('actions/cache/save@' + pin), 1)
        self.assertNotIn('actions/cache@', text)
        self.assertIn('        id: cargo_cache\n', restore_block)
        self.assertIn("key: peerbrush-build-v1-${{ matrix.os }}-${{ runner.arch }}-${{ steps.compiler.outputs.version }}-${{ hashFiles('Cargo.lock', 'Cargo.toml', 'scripts/build_release.py') }}", restore_block)
        self.assertIn('restore-keys: |\n            peerbrush-build-v1-${{ matrix.os }}-${{ runner.arch }}-${{ steps.compiler.outputs.version }}-\n', restore_block)
        paths = ['~/.cargo/registry/index', '~/.cargo/registry/cache', '~/.cargo/registry/src', 'target']
        for block in [restore_block, save_block]:
            actual = re.search(r'(?m)^          path: \|\n((?:            [^\n]+\n)+)', block)
            self.assertIsNotNone(actual)
            self.assertEqual([line.strip() for line in actual[1].splitlines()], paths)
        self.assertIn("if: success() && steps.cargo_cache.outputs.cache-hit != 'true' && steps.cargo_cache.outputs.cache-primary-key != ''\n", save_block)
        self.assertIn('key: ${{ steps.cargo_cache.outputs.cache-primary-key }}\n', save_block)
        self.assertEqual(text.count('outputs.cache-hit'), 1, 'Cache hits never gate build or acceptance')
        self.assertIn('run: cargo build --locked --bin peerbrush\n', text[build:save])
        self.assertNotIn('        if:', text[build:save])
        self.assertNotIn('always()', save_block)
        self.assertNotIn('continue-on-error:', save_block)
        for name in ['release.yml', 'macos-qualification.yml']:
            with self.subTest(workflow=name):
                self.assertNotIn('actions/cache', (root / name).read_text(encoding='utf-8'))


if __name__ == '__main__':
    unittest.main()
