"""Public-byte-bound Nix definition regressions; no Nix evaluation or build."""
import base64
import json
import os
import pathlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import zipfile

from package import portable_files, source_files
import prepare_nix_package as nix
import prepare_site_release as release
from test_nix_profile_upgrade import DESKTOP, ICON, MIME, verify_desktop_integration
import test_prepare_site_release


class NixDefinition(unittest.TestCase):
    def setUp(self):
        self.fixture = test_prepare_site_release.Published(methodName='runTest')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        f = self.fixture
        root = pathlib.Path(__file__).resolve().parents[1]
        for name in [nix.TEMPLATE, 'rust-toolchain.toml', 'scripts/jxl_build.py', 'scripts/package.py',
                     'packaging/linux/com.peerbrush.PeerBrush.desktop', 'packaging/linux/com.peerbrush.PeerBrush.xml']:
            path = f.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((root / name).read_bytes())
            f.fixture.policy['source'].append(name)
        f.fixture.save_policy()
        f.installed_policy()
        self.temporary = tempfile.TemporaryDirectory(prefix='peerbrush-nix-definition-')
        self.addCleanup(self.temporary.cleanup)
        self.output = pathlib.Path(self.temporary.name).resolve() / 'candidate'

    def prepare(self, **changes):
        f = self.fixture
        return nix.prepare(f.root, f.inventory, 'v0.2.0', f.commit, self.output,
                           **dict(dict(nixpkgs_commit='d' * 40, get=f.get), **changes))

    def test_actual_public_source_lockfile_and_support_bytes_define_the_package(self):
        f = self.fixture
        report = self.prepare()
        sources = source_files(f.root, f.commit)
        inputs = {name: data for name, data, _ in sources}
        source = next(row for row in f.inventory['assets'] if row['file'] == 'PeerBrush-0.2.0-source.zip')
        expression = (self.output / 'package.nix').read_text(encoding='utf-8')
        expected_hash = 'sha256-' + base64.b64encode(bytes.fromhex(source['sha256'])).decode()
        self.assertIn('hash = "' + expected_hash + '"', expression)
        self.assertIn('url = "' + release.DOWNLOAD + 'v0.2.0/' + source['file'] + '"', expression)
        self.assertIn('version = "0.2.0"', expression)
        self.assertNotRegex(expression, '@[A-Z_]+@')
        self.assertIn('cargoLock.lockFile = ./Cargo.lock', expression)
        self.assertIn('scripts/jxl_build.py --github-env', expression)
        for hook in ('CmakeConfigure', 'NinjaBuild', 'NinjaInstall', 'NinjaCheck'):
            self.assertIn(f'dontUse{hook} = true;', expression)
        self.assertIn('lib.getOutput "doc" rustc', expression)
        self.assertNotIn('PEERBRUSH_BUILD_COMMIT', expression)
        self.assertNotIn('fakeHash', expression)
        self.assertEqual((self.output / 'Cargo.lock').read_bytes(), inputs['Cargo.lock'])
        for name in ['README.md', 'LICENSE', 'assets/fonts/LICENCE.txt', 'docs/usage.md']:
            self.assertEqual((self.output / 'support' / name).read_bytes(), inputs[name])
        default = (self.output / 'default.nix').read_text(encoding='utf-8')
        self.assertIn('pkgs = importPinned "' + 'd' * 40 + '"', default)
        self.assertIn('config = {}; overlays = []', default)
        self.assertNotIn('<nixpkgs>', default)
        flake = (self.output / 'flake.nix').read_text(encoding='utf-8')
        self.assertIn('packages.x86_64-linux = rec {', flake)
        self.assertIn('peerbrush = import ./default.nix;', flake)
        self.assertIn('default = peerbrush;', flake)
        self.assertNotIn('inputs.', flake)
        self.assertEqual(report['files']['flake.nix'], release.sha(flake.encode('utf-8')))
        self.assertEqual(report['executable_files'], sorted('support/' + name for name, _, mode in portable_files(sources)
                                                           if mode & 0o111))
        self.assertEqual(report['inputs']['source_sha256'], source['sha256'])
        self.assertEqual(report['inputs']['native_tool_nixpkgs_commits'], dict.fromkeys(nix.TOOL_ATTRIBUTES, 'd' * 40))
        self.assertEqual(report['inputs']['embedded_git_provenance'], 'unknown-for-source-archive-build')
        for name, checksum in report['files'].items():
            self.assertEqual(release.sha((self.output / name).read_bytes()), checksum)
        self.assertFalse(report['release_ready'])
        self.assertEqual(report['nixpkgs_fetch_verification'], 'not-performed')
        self.assertEqual(report['evaluation'], 'not-performed')
        self.assertEqual(report['build'], 'not-performed')
        self.assertEqual(report['upgrade_rollback_acceptance'], 'not-performed')
        self.assertEqual(f.published()[0]['platforms'], [], 'Nix definitions must not promote website ZIPs')

    def test_changed_public_source_or_mutable_nixpkgs_ref_cannot_write_a_definition(self):
        for selected in ['nixos-unstable', 'latest', '', 'D' * 40, 'd' * 39]:
            with self.subTest(selected=selected), self.assertRaises(ValueError):
                self.prepare(nixpkgs_commit=selected)
            self.assertFalse(self.output.exists())
        for selected in [{'clang': 'nixos-unstable'}, {'rustc': 'e' * 40}, {'ninja': 'E' * 40}]:
            with self.subTest(selected=selected), self.assertRaises(ValueError):
                self.prepare(tool_nixpkgs_commits=selected)
            self.assertFalse(self.output.exists())
        f = self.fixture
        f.assets['PeerBrush-0.2.0-source.zip'] += b'changed'
        f.metadata()
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_native_tool_overrides_only_change_the_selected_fixed_package_inputs(self):
        report = self.prepare(tool_nixpkgs_commits={'clang': 'a' * 40, 'cmake': 'b' * 40})
        default = (self.output / 'default.nix').read_text(encoding='utf-8')
        self.assertIn(f'pkgs = importPinned "{"d" * 40}";', default)
        self.assertIn(f'llvmPackages_18 = (importPinned "{"a" * 40}").llvmPackages_18;', default)
        self.assertIn(f'cmake = (importPinned "{"b" * 40}").cmake;', default)
        self.assertNotIn('ninja =', default)
        pins = {'clang': 'a' * 40, 'cmake': 'b' * 40, 'ninja': 'd' * 40}
        self.assertEqual(report['inputs']['native_tool_nixpkgs_commits'], pins)
        self.assertEqual(json.loads((self.output / 'source-inputs.json').read_bytes())['native_tool_nixpkgs_commits'], pins)

    @unittest.skipUnless(sys.platform == 'linux', 'Native Linux shell contract; synthetic version launchers only')
    def test_build_hook_exports_qualified_paths_and_stops_on_unqualified_tools(self):
        bash = shutil.which('bash')
        self.assertIsNotNone(bash, 'Linux native qualification needs the existing Bash shell')
        self.prepare()
        expression = (self.output / 'package.nix').read_text(encoding='utf-8')
        hook = expression.split("  preBuild = ''\n", 1)[1].split("\n  '';", 1)[0]
        hook = hook.replace('${python3.interpreter}', shlex.quote(sys.executable))
        self.assertNotIn('${', hook, 'All Nix inputs to the tested hook must be bound')
        directory = self.output.parent / 'native tools with spaces'
        directory.mkdir()
        tools = {}
        for key, name, version in [('CC', 'clang', 'clang version 18.1.3'),
                                   ('CXX', 'clang++', 'clang version 18.1.3'),
                                   ('CMAKE', 'cmake', 'cmake version 3.31.6'),
                                   ('CMAKE_MAKE_PROGRAM', 'ninja', '1.13.2')]:
            path = directory / name
            path.write_text('#!/bin/sh\ntest "$#" = 1 && test "$1" = --version || exit 49\n'
                            + 'printf "%s\\n" ' + shlex.quote(version) + '\n', encoding='utf-8')
            path.chmod(0o755)
            tools[key] = str(path)
        environment = {name: value for name, value in os.environ.items()
                       if name.upper() not in {'DEP_JXL_PATH', 'DEP_JXL_LIB'}}
        environment.update(tools, TMPDIR=str(directory), GITHUB_ENV=str(directory / 'ambient-environment'))
        observe = ('import json,os; print(json.dumps({key:os.environ.get(key) for key in '
                   + repr([*tools, 'CMAKE_GENERATOR', 'CARGO_BUILD_JOBS', 'CMAKE_BUILD_PARALLEL_LEVEL', 'PATH']) + '}))')
        command = hook + '\n' + shlex.quote(sys.executable) + ' -c ' + shlex.quote(observe)
        result = subprocess.run([bash, '-e', '-c', command], cwd=self.fixture.root,
                                env=environment, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        exported = json.loads(result.stdout.splitlines()[-1])
        for key, path in tools.items():
            self.assertEqual(exported[key], path)
        self.assertEqual(exported['CMAKE_GENERATOR'], 'Ninja')
        self.assertEqual(exported['CARGO_BUILD_JOBS'], '2')
        self.assertEqual(exported['CMAKE_BUILD_PARALLEL_LEVEL'], '2')
        self.assertEqual(exported['PATH'], str(directory) + os.pathsep + environment['PATH'])
        self.assertFalse(pathlib.Path(environment['GITHUB_ENV']).exists(), 'Hook output must not use an inherited destination')
        self.assertEqual(sorted(p.name for p in directory.iterdir()), sorted(pathlib.Path(p).name for p in tools.values()))
        pathlib.Path(tools['CC']).write_text('#!/bin/sh\nprintf "clang version 0.0.0\\n"\n', encoding='utf-8')
        refused = subprocess.run([bash, '-e', '-c', command], cwd=self.fixture.root,
                                 env=environment, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('Unqualified native JPEG XL build tool version', refused.stderr)
        self.assertNotIn('"CMAKE_GENERATOR"', refused.stdout, 'Failed qualification must stop the build hook')
        self.assertFalse(pathlib.Path(environment['GITHUB_ENV']).exists())

    def test_git_registry_and_missing_or_duplicate_crate_hashes_are_not_silently_unpinned(self):
        good = ('version=4\n[[package]]\nname="sample"\nversion="1.0.0"\n'
                f'source="{nix.REGISTRY}"\nchecksum="' + 'a' * 64 + '"\n').encode()
        self.assertEqual(nix.validate_lock(good), {'sample-1.0.0': 'a' * 64})
        for invalid in [good.replace(nix.REGISTRY.encode(), b'git+https://example.invalid/source'),
                        good.replace(nix.REGISTRY.encode(), b'registry+https://example.invalid/index'),
                        good.replace(b'checksum=', b'ignored='),
                        good.replace(b'name="sample"', b'name="../sample"'),
                        good.replace(b'version="1.0.0"', b'version="1.0.0/extra"'),
                        good + good[good.index(b'[[package]]'):], b'version=4\n']:
            with self.subTest(invalid=invalid[:32]), self.assertRaises(ValueError):
                nix.validate_lock(invalid)

    def test_existing_output_and_cli_digest_selection_preserve_earlier_bytes(self):
        self.prepare()
        before = {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob('*') if p.is_file()}
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertEqual(before, {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob('*') if p.is_file()})
        selected = self.output.parent / 'inventory.json'
        raw = release.encode(self.fixture.inventory)
        selected.write_bytes(raw)
        args = ['--inventory', str(selected), '--inventory-sha256', release.sha(raw),
                '--tag', 'v0.2.0', '--commit', self.fixture.commit, '--nixpkgs-commit', 'd' * 40,
                '--output', str(self.output)]
        with mock.patch.object(nix, 'prepare') as prepare:
            self.assertEqual(nix.main(args), 0)
        self.assertEqual(prepare.call_args.kwargs['nixpkgs_commit'], 'd' * 40)
        selected.write_bytes(raw + b' ')
        with mock.patch.object(nix, 'prepare') as prepare, self.assertRaises(ValueError):
            nix.main(args)
        prepare.assert_not_called()


@unittest.skipUnless(sys.platform == 'linux', 'Nix profiles use Linux store paths and symlinks')
class NixProfileDesktop(unittest.TestCase):
    def test_desktop_assets_require_exact_source_bytes_and_selected_profile_targets(self):
        root = pathlib.Path(__file__).resolve().parents[1]
        desktop = root / 'packaging/linux/com.peerbrush.PeerBrush.desktop'
        mime = root / 'packaging/linux/com.peerbrush.PeerBrush.xml'
        icon = root / 'assets/peerbrush-logo.png'
        with tempfile.TemporaryDirectory() as temporary:
            base = pathlib.Path(temporary).resolve()
            output = base / 'peerbrush-output'
            generation = base / 'profile-1-link'
            profile = base / 'profile'
            generation.mkdir()
            profile.symlink_to(generation)
            source = base / 'source.zip'
            with zipfile.ZipFile(source, 'w') as archive:
                for asset in (desktop, mime, icon):
                    archive.write(asset, asset.relative_to(root))
            data = {
                DESKTOP: (desktop.read_bytes()
                          .replace(b'/usr/bin/peerbrush', os.fsencode(output / 'bin/peerbrush'))
                          .replace(b'/usr/share/peerbrush/peerbrush-logo.png', os.fsencode(output / ICON))),
                MIME: mime.read_bytes(),
                ICON: icon.read_bytes(),
            }
            for asset, contents in data.items():
                path = output / asset
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(contents)
                if asset != ICON:
                    link = generation / asset
                    link.parent.mkdir(parents=True, exist_ok=True)
                    link.symlink_to(path)
            verify_desktop_integration(profile, output, source)

            (output / ICON).write_bytes(b'wrong image')
            with self.assertRaisesRegex(ValueError, 'differs from the pinned source'):
                verify_desktop_integration(profile, output, source)
            (output / ICON).write_bytes(data[ICON])

            other = base / 'other.xml'
            other.write_bytes(data[MIME])
            (generation / MIME).unlink()
            (generation / MIME).symlink_to(other)
            with self.assertRaisesRegex(ValueError, 'does not resolve to the selected output'):
                verify_desktop_integration(profile, output, source)


if __name__ == '__main__':
    unittest.main()
