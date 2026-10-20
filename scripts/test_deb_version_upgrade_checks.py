"""Offline contract regressions; never execute the synthetic ELF or install a package."""
import copy
import hashlib
import json
import os
from pathlib import Path
import signal
import struct
import subprocess
import tempfile
import unittest
from unittest import mock
import zipfile
import zlib

import test_deb_version_upgrade as upgrade
import test_package
from test_package import packaging


class HistoricalAssets(unittest.TestCase):
    def setUp(self):
        self.fixture = test_package.Packages('test_platform_architecture_source_license_and_macos_bundle_match')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root.resolve()
        self.archive = self.fixture.build('Linux')
        self.commit = packaging.git(self.root, 'rev-parse', 'HEAD').decode().strip()
        self.tag = 'v0.2.0'
        self.name = self.archive.name
        self.source = 'PeerBrush-0.2.0-source.zip'
        self.body = {self.name: self.archive.read_bytes(),
                     self.source: (self.archive.parent / self.source).read_bytes()}
        self.body['SHA256SUMS'] = ''.join(hashlib.sha256(self.body[name]).hexdigest() + '  ' + name + '\n'
                                        for name in [self.name, self.source]).encode()
        self.release = dict(tag_name=self.tag, draft=False, prerelease=False,
                            html_url='https://github.com/Deftr0y/PeerBrush/releases/tag/' + self.tag,
                            published_at='2026-01-02T03:04:05Z', assets=[])
        for name, body in self.body.items():
            self.release['assets'].append(dict(name=name, state='uploaded', size=len(body),
                digest='sha256:' + hashlib.sha256(body).hexdigest(),
                browser_download_url=upgrade.DOWNLOAD + self.tag + '/' + name))
        self.resolved = dict(sha=self.commit)
        self.raw_release = None
        self.patch = mock.patch.multiple(upgrade, OLD_VERSION='0.2.0', OLD_TAG=self.tag,
            OLD_COMMIT=self.commit, OLD_NAME=self.name, OLD_SOURCE=self.source,
            OLD_SHA256=hashlib.sha256(self.body[self.name]).hexdigest(), OLD_BYTES=len(self.body[self.name]))
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def get(self, url, maximum, *, api=False, destination=None):
        if api:
            self.assertIsNone(destination)
            if url == upgrade.API + 'releases/tags/' + self.tag:
                data = self.raw_release or json.dumps(self.release).encode()
            elif url == upgrade.API + 'commits/' + self.tag:
                data = json.dumps(self.resolved).encode()
            else:
                raise AssertionError('Unexpected public metadata route')
            self.assertLessEqual(len(data), maximum)
            return data
        self.assertIsNotNone(destination)
        name = next((name for name in self.body if url == upgrade.DOWNLOAD + self.tag + '/' + name), None)
        if name is None:
            raise AssertionError('Unexpected public asset route')
        data = self.body[name]
        if len(data) > maximum:
            raise ValueError('Synthetic public asset exceeds budget')
        with destination.open('xb') as stream:
            stream.write(data)

    def inspect(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-old-assets-') as temporary:
            return upgrade.historical_inputs(self.root, Path(temporary).resolve(), get=self.get)

    def test_exact_public_bytes_and_committed_corresponding_source_without_execution(self):
        _, checked = self.inspect()
        self.assertEqual(checked['record']['binary_source_commit'], self.commit)
        self.assertEqual(checked['manifest']['platform'], 'Linux')
        self.assertFalse(checked['packaging_correction'])

    def test_unavailable_ambiguous_or_wrong_public_identity_rejects(self):
        original = copy.deepcopy(self.release)
        cases = [('draft', True), ('prerelease', True), ('tag_name', 'v0.1.0'),
                 ('published_at', '2026-02-31T00:00:00Z'), ('published_at', 1),
                 ('assets', [False]), ('assets', original['assets'] + [original['assets'][0]])]
        for key, value in cases:
            with self.subTest(key=key, value=str(value)[:40]):
                self.release = copy.deepcopy(original)
                self.release[key] = value
                with self.assertRaises(ValueError):
                    self.inspect()
        self.release = original
        self.resolved['sha'] = '0' * 40
        with self.assertRaises(ValueError):
            self.inspect()
        self.resolved['sha'] = self.commit
        self.raw_release = b'{"tag_name":"v0.2.0","tag_name":"v0.2.0"}'
        with self.assertRaises(ValueError):
            self.inspect()

    def test_asset_origin_size_digest_and_missing_asset_rejects(self):
        original = copy.deepcopy(self.release)
        for key, value in [('browser_download_url', 'https://example.invalid/asset.zip'),
                           ('size', True), ('size', 0), ('size', len(self.body[self.name]) + 1),
                           ('digest', 'sha256:' + '0' * 64), ('state', 'new')]:
            with self.subTest(field=key):
                self.release = copy.deepcopy(original)
                self.release['assets'][0][key] = value
                with self.assertRaises(ValueError):
                    self.inspect()
        self.release = original
        self.release['assets'] = self.release['assets'][1:]
        with self.assertRaises(ValueError):
            self.inspect()

    def test_tampered_source_checksums_portable_and_dirty_source_rejects(self):
        original_body, original_release = copy.deepcopy(self.body), copy.deepcopy(self.release)
        for name in [self.name, self.source, 'SHA256SUMS']:
            with self.subTest(asset=name):
                self.body = copy.deepcopy(original_body)
                self.release = copy.deepcopy(original_release)
                # Keep public metadata consistent with damaged bytes. The independent
                # portable pin, source comparison or actual checksum list must reject.
                data = self.body[name]
                self.body[name] = bytes([data[0] ^ 1]) + data[1:]
                row = next(row for row in self.release['assets'] if row['name'] == name)
                row['digest'] = 'sha256:' + hashlib.sha256(self.body[name]).hexdigest()
                with self.assertRaises(ValueError):
                    self.inspect()
        self.body, self.release = original_body, original_release
        (self.root / 'src/lib.rs').write_text('// changed committed source\n')
        with self.assertRaisesRegex(ValueError, 'clean exact'):
            self.inspect()


def png(depth=16, *, bad_filter=False):
    width, height = upgrade.PROJECT_SIZE
    pixel = b'\x27\x11\x4e\x23\x75\x37\xff\xff' if depth == 16 else bytes([39, 78, 117, 255])
    rows, previous = bytearray(), bytes(width * len(pixel))
    for y in range(height):
        raw = pixel * width
        kind = y % 5
        encoded = bytearray()
        for x, value in enumerate(raw):
            left = raw[x - len(pixel)] if x >= len(pixel) else 0
            above = previous[x]
            upper_left = previous[x - len(pixel)] if x >= len(pixel) else 0
            p = left + above - upper_left
            choices = [left, above, upper_left]
            paeth = choices[min(range(3), key=lambda i: abs(p - choices[i]))]
            predictor = [0, left, above, (left + above) // 2, paeth][kind]
            encoded.append((value - predictor) & 255)
        rows.extend(bytes([5 if bad_filter and y == 0 else kind]) + encoded)
        previous = raw
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    header = struct.pack('>IIBBBBB', width, height, depth, 6, 0, 0, 0)
    image = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b'')
    return image, pixel * width * height


class FixtureContract(unittest.TestCase):
    def test_failure_diagnostic_emits_only_fixed_types_allowlisted_locations_and_status(self):
        secret = 'PRIVATE_TOKEN_AND_RPC_ARGUMENTS'
        def captured(source, filename):
            try:
                exec(compile(source, str(filename), 'exec'), {'subprocess': subprocess})
            except Exception as error:
                return error
            self.fail('Synthetic exception was not raised')
        trusted = Path(upgrade.__file__).resolve()
        error = captured("raise ValueError('" + secret + "')", trusted)
        detail = upgrade.failure_diagnostic(error)
        self.assertEqual(detail, 'Compatibility diagnostic: ValueError at test_deb_version_upgrade.py:1')
        self.assertNotIn(secret, detail)
        self.assertNotIn(str(trusted.parent), detail)
        error = captured("raise KeyError('" + secret + "')", Path('/private') / secret / trusted.name)
        self.assertEqual(upgrade.failure_diagnostic(error), 'Compatibility diagnostic: KeyError')
        error = captured("raise subprocess.CalledProcessError(127, '" + secret + "', output=b'private', stderr=b'private')", trusted)
        self.assertEqual(upgrade.failure_diagnostic(error), 'Compatibility diagnostic: CalledProcessError at test_deb_version_upgrade.py:1 exit=127')
        unknown = type(secret, (Exception,), {})(secret)
        self.assertEqual(upgrade.failure_diagnostic(unknown), 'Compatibility diagnostic: OtherError')
        error = captured("raise ValueError('outer') from subprocess.CalledProcessError(1, '" + secret + "')", trusted)
        detail = upgrade.failure_diagnostic(error)
        self.assertEqual(detail, 'Compatibility diagnostic: ValueError at test_deb_version_upgrade.py:1 <- CalledProcessError exit=1')
        self.assertNotIn(secret, detail)
        error.__cause__ = error
        self.assertLessEqual(len(upgrade.failure_diagnostic(error)), 256)

    def test_nonzero_rpc_and_readiness_timeout_keep_sanitizable_causes(self):
        session = upgrade.Session(Path('/bin/peerbrush'), {}, Path('/private'), None)
        reply = subprocess.CompletedProcess([], 23, b'PRIVATE_RPC_REPLY', b'PRIVATE_STDERR')
        with mock.patch.object(upgrade.subprocess, 'run', return_value=reply):
            with self.assertRaises(ValueError) as caught:
                session.rpc('observe', {'secret': 'PRIVATE_ARGUMENTS'})
        cause = caught.exception.__cause__
        self.assertIs(type(cause), subprocess.CalledProcessError)
        self.assertEqual(cause.returncode, 23)
        self.assertEqual(cause.cmd, 'fixture-rpc')
        self.assertIsNone(cause.stdout)
        self.assertIsNone(cause.stderr)
        detail = upgrade.failure_diagnostic(caught.exception)
        self.assertIn('CalledProcessError exit=23', detail)
        self.assertNotIn('PRIVATE', detail)
        with tempfile.TemporaryDirectory(prefix='peerbrush-readiness-') as temporary:
            session = upgrade.Session(Path('/bin/peerbrush'), {}, Path(temporary).resolve(), None)
            process = mock.Mock(pid=1234)
            process.poll.return_value = None
            process.wait.return_value = None
            failed = ValueError('PRIVATE_READINESS_REASON')
            with mock.patch.object(upgrade.subprocess, 'Popen', return_value=process), \
                    mock.patch.object(session, 'rpc', side_effect=failed), \
                    mock.patch.object(upgrade.time, 'monotonic', side_effect=[0, 0, 31]), \
                    mock.patch.object(upgrade.time, 'sleep'), \
                    mock.patch.object(os, 'killpg', create=True):
                with self.assertRaises(ValueError) as caught:
                    session.__enter__()
            self.assertIs(caught.exception.__cause__, failed)
            self.assertTrue(session.output.closed)
            self.assertNotIn('PRIVATE', upgrade.failure_diagnostic(caught.exception))

    def test_native8_and16_png_samples_all_filters_and_invalid_rasters(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-png-') as temporary:
            path = Path(temporary).resolve() / 'fixture.png'
            for depth in [8, 16]:
                image, raster = png(depth)
                path.write_bytes(image)
                self.assertEqual(upgrade.png_samples(path), (*upgrade.PROJECT_SIZE, depth, raster))
                for invalid in [image[:-1], image[:-5] + bytes([image[-5] ^ 1]) + image[-4:],
                                png(depth, bad_filter=True)[0], image + b'trailing',
                                image + bytes(1024 * 1024)]:
                    path.write_bytes(invalid)
                    with self.assertRaises(ValueError):
                        upgrade.png_samples(path)

    def test_host_and_missing_extra_opt_in_stop_before_state_network_or_package_actions(self):
        with mock.patch.object(upgrade, 'require_disposable', side_effect=ValueError('host blocked')), \
                mock.patch.object(upgrade, 'historical_inputs') as old, \
                mock.patch.object(upgrade, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'host blocked'):
                upgrade.check(None, None, None, None)
            old.assert_not_called()
            run.assert_not_called()
        with mock.patch.object(upgrade, 'require_disposable'), \
                mock.patch.dict(os.environ, {}, clear=True), \
                mock.patch.object(upgrade, 'validate_expected') as selected:
            with self.assertRaisesRegex(ValueError, 'additional explicit'):
                upgrade.check(None, None, None, None)
            selected.assert_not_called()

    def test_only_canonical_selected_dependencies_are_installed_before_old_portable(self):
        from package_deb import RUNTIME
        selected = 'libc6 (>= 2.39), ' + ', '.join(RUNTIME)
        with mock.patch.object(upgrade, 'run', side_effect=[selected, '']) as run:
            upgrade.runtime_dependencies(Path('/private/current.deb'))
            self.assertEqual(run.call_args.args[0], ['apt-get', 'satisfy', '-y', selected])
        for invalid in ['Conflicts: libc6', 'libc6; touch /tmp/unsafe', 'libc6']:
            with self.subTest(dependencies=invalid), mock.patch.object(upgrade, 'run', return_value=invalid) as run:
                with self.assertRaises(ValueError):
                    upgrade.runtime_dependencies(Path('/private/current.deb'))
                self.assertEqual(run.call_count, 1)

    def test_explicit_default_commands_rpc_actor_and_guarded_revision(self):
        session = upgrade.Session(Path('/bin/peerbrush'), {'HOME': '/isolated'}, Path('/private'), Path('/legacy'))
        self.assertEqual(session.command('--headless'), ['runuser', '-u', 'peerbrush-upgrade-qa',
            '--', str(Path('/bin/peerbrush')), '--state-dir', str(Path('/legacy')), '--headless'])
        session.state = None
        self.assertNotIn('--state-dir', session.command('--headless'))
        reply = subprocess.CompletedProcess([], 0, json.dumps({'ok': True, 'result': {'profile': {}}}).encode(), b'')
        with mock.patch.object(upgrade.subprocess, 'run', return_value=reply) as run:
            self.assertEqual(session.rpc('tool_settings', dict(action='get', tool='brush')), {'profile': {}})
            command = run.call_args.args[0]
            self.assertEqual(json.loads(command[-1]), dict(actor='human', action='get', tool='brush'))
            self.assertEqual(run.call_args.kwargs['timeout'], 10)
            self.assertEqual(run.call_args.kwargs['stdin'], subprocess.DEVNULL)
        with mock.patch.object(session, 'rpc', side_effect=[{'document': {'id': 'doc', 'revision': 7}}, {'saved': True}]) as rpc:
            session.guarded('save', path='/artwork/file.psd', accept_editability_loss=True)
            self.assertEqual(rpc.call_args.args, ('document', dict(action='save', document_id='doc',
                expected_revision=7, path='/artwork/file.psd', accept_editability_loss=True)))


    def test_editable_shape_snapshot_preserves_depth_bounds_kind_and_definition(self):
        state = dict(document=dict(id='volatile', revision=4, width=32, height=24, bit_depth=16, read_only=False),
            layers=[dict(id='layer', kind='paint', bounds=[0, 0, 32, 24], source=dict(width=32, height=24,
                matrix=[1., 0., 0., 1., 0., 0.], content=dict(kind='shape', shape='rectangle', bounds=[2., 3., 22., 18.],
                    points=[], closed=True, fill=[10001, 20003, 30007, 65535], stroke=[0, 0, 0, 0], stroke_width=0.)))])
        expected = upgrade.editable_snapshot(state)
        reopened = copy.deepcopy(state)
        reopened['document'].update(id='new-runtime-id', revision=0)
        reopened['layers'][0]['id'] = 'new-layer-id'
        self.assertEqual(upgrade.editable_snapshot(reopened), expected)
        for field, value in [('kind', 'fill'), ('bounds', [1, 0, 33, 24])]:
            changed = copy.deepcopy(reopened)
            changed['layers'][0][field] = value
            self.assertNotEqual(upgrade.editable_snapshot(changed), expected)
        changed = copy.deepcopy(reopened)
        changed['document']['bit_depth'] = 8
        self.assertNotEqual(upgrade.editable_snapshot(changed), expected)
        changed = copy.deepcopy(reopened)
        changed['layers'][0]['source']['content']['fill'][0] = 10000
        self.assertNotEqual(upgrade.editable_snapshot(changed), expected)
        for source in [None, {}, {'content': []}]:
            changed = copy.deepcopy(reopened)
            changed['layers'][0]['source'] = source
            with self.assertRaises(ValueError):
                upgrade.editable_snapshot(changed)

    def test_private_headless_child_group_is_reaped_and_log_closed_after_failure(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-session-') as temporary:
            session = upgrade.Session(Path('/bin/peerbrush'), {}, Path(temporary).resolve(), None)
            process = mock.Mock(pid=1234)
            process.poll.return_value = None
            process.wait.side_effect = [subprocess.TimeoutExpired('synthetic', 10), None]
            session.process = process
            session.output = (session.work / 'private.log').open('xb')
            with mock.patch.object(os, 'killpg', create=True) as kill, \
                    mock.patch.object(signal, 'SIGKILL', 9, create=True):
                session.__exit__(None, None, None)
                self.assertEqual(kill.call_args_list, [mock.call(1234, signal.SIGTERM), mock.call(1234, 9)])
                self.assertEqual(process.wait.call_args_list, [mock.call(timeout=10), mock.call(timeout=10)])
            self.assertTrue(session.output.closed)
            with mock.patch.object(upgrade.subprocess, 'Popen', side_effect=OSError('synthetic spawn failure')):
                with self.assertRaises(OSError):
                    session = upgrade.Session(Path('/bin/peerbrush'), {}, Path(temporary).resolve(), None)
                    session.__enter__()
            self.assertTrue(session.output.closed)


if __name__ == '__main__':
    unittest.main()
