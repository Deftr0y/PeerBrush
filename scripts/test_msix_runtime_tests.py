"""Regression checks using a real PeerBrush binary and disposable headless state.

These verify adapter-to-application connectivity, not Windows installation or GUI.
Run after scripts/build_release.py; --executable can select an existing test build.
"""
import argparse
import contextlib
import io
import json
import os
import copy
import pathlib
import queue
import subprocess
import sys
import tempfile
import time
import types
import unittest

import test_msix_runtime as runtime

ROOT=pathlib.Path(__file__).resolve().parents[1]
BINARY=ROOT/'target/release'/('peerbrush.exe' if sys.platform=='win32' else 'peerbrush')


class InstallationObservation(unittest.TestCase):
    def record(self):
        return dict(schema=1, format='PeerBrushInstallationObservationV1', platform='windows',
            build=dict(schema=1, format='PeerBrushBuildInfoV1', product='PeerBrush', version='0.30.0',
                commit='a' * 40, tree='clean', end='PeerBrushBuildInfoEnd'),
            package=dict(name='PeerBrush.Fixture', publisher='CN=PeerBrush Fixture', family='PeerBrush.Fixture_123',
                full_name='PeerBrush.Fixture_1.30.0.0_x64__123', version=[1, 30, 0, 0], signature_kind='developer',
                appinstaller_association=False, development_mode=False, non_main=False,
                candidate_route='verified-local-candidate'), installed_acceptance='not-established', update_eligible=False)

    def test_installed_route_is_fresh_observation_only_and_legacy_store_are_distinct(self):
        record = self.record()
        def check(value, route='verified-local-candidate'):
            return runtime.validate_installation_observation(json.dumps(value).encode(), platform='windows',
                family='PeerBrush.Fixture_123', expected_route=route)
        self.assertEqual(check(record), record)
        for route, kind, associated in [('legacy-appinstaller', 'developer', True), ('microsoft-store', 'store', False)]:
            selected = copy.deepcopy(record)
            selected['package'].update(candidate_route=route, signature_kind=kind, appinstaller_association=associated)
            self.assertEqual(check(selected, route), selected)
            with self.assertRaisesRegex(ValueError, 'route differs'): check(selected)
        for change in ['unpackaged', 'family', 'development', 'non-main', 'signature', 'route', 'dirty',
                       'unknown-build', 'eligible', 'acceptance', 'private-uri', 'version-bool']:
            selected = copy.deepcopy(record)
            if change == 'unpackaged': selected['package'] = None
            elif change == 'family': selected['package']['family'] = 'Different.Family'
            elif change == 'development': selected['package']['development_mode'] = True
            elif change == 'non-main': selected['package']['non_main'] = True
            elif change == 'signature': selected['package']['signature_kind'] = 'untrusted-or-unknown'
            elif change == 'route': selected['package']['candidate_route'] = 'legacy-appinstaller'
            elif change == 'dirty': selected['build']['tree'] = 'dirty'
            elif change == 'unknown-build': selected['build']['commit'] = 'unknown'
            elif change == 'eligible': selected['update_eligible'] = True
            elif change == 'acceptance': selected['installed_acceptance'] = 'passed'
            elif change == 'private-uri': selected['package']['association_uri'] = 'PRIVATE_CANARY'
            else: selected['package']['version'][0] = True
            with self.subTest(change=change), self.assertRaises(ValueError): check(selected)
        with self.assertRaisesRegex(ValueError, 'Duplicate'):
            runtime.validate_installation_observation(b'{"schema":1,' + json.dumps(record).encode()[1:], platform='windows')
        with self.assertRaisesRegex(ValueError, 'byte bound'):
            runtime.validate_installation_observation(b' ' * (16 * 1024 + 1), platform='windows')

    @unittest.skipUnless(sys.platform == 'win32', 'Native Windows alias metadata only')
    def test_an_ordinary_file_in_a_fake_family_alias_directory_is_not_an_installed_alias(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-native-alias-') as directory:
            path = pathlib.Path(directory) / 'WindowsApps/Fixture.Family/peerbrush-mcp.exe'
            path.parent.mkdir(parents=True)
            path.write_bytes(b'ordinary fixture file; never executed')
            self.assertFalse(runtime.installed_alias(path))
            with self.assertRaisesRegex(ValueError, 'not a native Windows app execution alias'):
                runtime.smoke(path, pathlib.Path(directory))


class RuntimeSmoke(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not BINARY.is_file():
            raise RuntimeError('Build PeerBrush first or provide --executable; runtime tests are not silently skipped')

    def setUp(self):
        self.directory=tempfile.TemporaryDirectory(prefix='PeerBrush runtime QA 作品 ')
        self.addCleanup(self.directory.cleanup)
        self.root=pathlib.Path(self.directory.name)

    @contextlib.contextmanager
    def running(self,state):
        process=subprocess.Popen([str(BINARY),'--headless','--state-dir',str(state)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        try:
            deadline=time.monotonic()+20
            while not (state/'connection.json').is_file():
                if process.poll() is not None:raise AssertionError('Headless application exited during startup')
                if time.monotonic()>=deadline:raise AssertionError('Headless startup timed out')
                time.sleep(.05)
            yield process
        finally:
            if process.poll() is None:
                process.terminate()
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

    def test_two_clients_must_reach_live_application(self):
        state=self.root/'canvas state'
        with self.running(state):
            report=runtime.smoke(BINARY,state,allow_portable=True)
        self.assertTrue(report['passed'])
        self.assertTrue(report['live_application_bridge'])
        self.assertFalse(report['native_gui_verified'])
        self.assertEqual(report['parallel_clients'],2)

    def test_installation_observation_is_read_only_and_cannot_claim_update_eligibility(self):
        environment = os.environ.copy()
        for name in ['APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'XDG_CONFIG_HOME']:
            environment[name] = str(self.root / name)
        result = subprocess.run([str(BINARY), '--installation-info'], cwd=self.root,
            env=environment, capture_output=True, timeout=15, check=True)
        self.assertLess(len(result.stdout), 16 * 1024)
        record = json.loads(result.stdout)
        self.assertEqual(set(record), {'schema', 'format', 'platform', 'build', 'package',
                                      'installed_acceptance', 'update_eligible'})
        self.assertEqual(record['schema'], 1)
        self.assertEqual(record['format'], 'PeerBrushInstallationObservationV1')
        self.assertEqual(record['build']['product'], 'PeerBrush')
        self.assertIsNone(record['package']) # This smoke binary is unpackaged.
        self.assertEqual(record['installed_acceptance'], 'not-established')
        self.assertIs(record['update_eligible'], False)
        self.assertEqual(list(self.root.iterdir()), [])
        invalid = subprocess.run([str(BINARY), '--installation-info', '--state-dir', str(self.root / 'state')],
            cwd=self.root, env=environment, capture_output=True, timeout=15)
        self.assertNotEqual(invalid.returncode, 0)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_static_initialize_and_tools_list_do_not_validate_invalid_connection(self):
        state=self.root/'invalid connection'
        state.mkdir()
        (state/'connection.json').write_text('{}')
        with self.assertRaisesRegex(ValueError,'tool call failed'):
            runtime.smoke(BINARY,state,allow_portable=True)

    def test_stale_connection_after_application_exits_is_rejected(self):
        state=self.root/'stale connection'
        with self.running(state):
            stale=(state/'connection.json').read_bytes()
        # Retain the old endpoint and token, as after an interrupted app session.
        (state/'connection.json').write_bytes(stale)
        with self.assertRaisesRegex(ValueError,'tool call failed'):
            runtime.smoke(BINARY,state,allow_portable=True)

    def test_wrong_token_cannot_pass_using_static_adapter_responses(self):
        state=self.root/'real application'
        fake=self.root/'invalid authentication'
        fake.mkdir()
        with self.running(state):
            connection=json.loads((state/'connection.json').read_text())
            connection['token']='invalid-synthetic-test-token'
            (fake/'connection.json').write_text(json.dumps(connection))
            with self.assertRaisesRegex(ValueError,'tool call failed'):
                runtime.smoke(BINARY,fake,allow_portable=True)

    def test_request_rejects_mcp_tool_errors_inside_successful_jsonrpc_response(self):
        process=types.SimpleNamespace(stdin=io.BytesIO())
        lines=queue.Queue()
        lines.put(json.dumps({'jsonrpc':'2.0','id':1,'result':{'isError':True,'content':[]}}).encode())
        with self.assertRaisesRegex(ValueError,'tool call failed'):
            runtime.request(process,lines,1,'tools/call',{'name':'peerbrush_projects'})


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--executable',type=pathlib.Path,default=BINARY)
    args,remaining=parser.parse_known_args()
    BINARY=args.executable.absolute()
    unittest.main(argv=[sys.argv[0],*remaining])
