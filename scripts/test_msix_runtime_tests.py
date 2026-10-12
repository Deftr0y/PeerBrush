"""Regression checks using a real PeerBrush binary and disposable headless state.

These verify adapter-to-application connectivity, not Windows installation or GUI.
Run after scripts/build_release.py; --executable can select an existing test build.
"""
import argparse
import contextlib
import io
import json
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
