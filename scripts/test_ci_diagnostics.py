"""Check the actual workflow summarizer with bounded synthetic failure logs."""
import pathlib
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKFLOW = (ROOT / '.github/workflows/ci.yml').read_text(encoding='utf-8')
INLINE = re.search(r"python - [^\r\n]*peerbrush-tests\.log[^\r\n]* <<'PY'\n(.*?)\n          PY", WORKFLOW, re.S)
if INLINE is None:
    raise RuntimeError('The actual regression summarizer must be available to its tests')
SCRIPT = textwrap.dedent(INLINE.group(1))
PRIVATE = 'PRIVATE_DIAGNOSTIC_SENTINEL'


class Diagnostics(unittest.TestCase):
    def summarize(self, log, status=101):
        with tempfile.TemporaryDirectory(prefix='peerbrush-ci-summary-') as temporary:
            base = pathlib.Path(temporary)
            source, summary = base / 'input.log', base / 'summary.md'
            source.write_text(log, encoding='utf-8')
            result = subprocess.run([sys.executable, '-c', SCRIPT, str(source), str(summary), str(status)],
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            output = result.stdout + summary.read_text(encoding='utf-8')
            self.assertNotIn(PRIVATE, output)
            return output

    def test_success_and_named_failures_preserve_existing_counts_and_locations(self):
        output = self.summarize('test result: ok. 12 passed; 0 failed; 3 ignored;\n' + PRIVATE, status=0)
        self.assertIn('12 passed; 0 failed; 3 ignored;', output)
        self.assertNotIn('Failure classification:', output)
        output = self.summarize("test source::bounded ... FAILED\nthread 'source::bounded' panicked at src/source.rs:12:3:\n" + PRIVATE)
        self.assertIn('source::bounded', output)
        self.assertIn('file=src/source.rs,line=12,col=3', output)
        self.assertNotIn('Regression test run failed', output)

    def test_worker_timeout_category_is_anchored_allowlisted_and_private(self):
        name = 'jpeg_xl::tests::abnormal_exit_abort_protocol_damage_and_timeout_preserve_parent_native_work'
        for code in ['FAILED', 'PROTOCOL', 'RESOURCE', 'OTHER']:
            output = self.summarize(
                'test ' + name + ' ... FAILED\n---- ' + name + ' stdout ----\n'
                'thread \'' + name + '\' panicked at src/jpeg_xl/tests.rs:365:9:\n'
                'JXL_TIMEOUT_ERROR_' + code + '\n' + PRIVATE)
            self.assertIn(name + ': JXL_WORKER_' + code, output)
        for marker in ['JXL_TIMEOUT_ERROR_' + PRIVATE, 'JXL_TIMEOUT_ERROR_RESOURCE ' + PRIVATE,
                       PRIVATE + ' JXL_TIMEOUT_ERROR_RESOURCE']:
            output = self.summarize('test ' + name + ' ... FAILED\n---- ' + name + ' stdout ----\n' + marker)
            self.assertNotIn('Regression failure category', output)

    def test_effect_color_history_record_reports_only_exact_fixed_fields(self):
        name = 'ui::effect_color::tests::effect_color_hex_is_live_provisional_and_cancels_on_escape_invalid_input_and_source_changes'
        header = 'test ' + name + ' ... FAILED\n---- ' + name + ' stdout ----\n'
        for depth in [8, 16]:
            for action in ['escape', 'invalid', 'source']:
                with self.subTest(depth=depth, action=action):
                    marker = f'PB_EFFECT_COLOR_HISTORY depth={depth} action={action} before=0 after=1'
                    output = self.summarize(header + marker + '\n' + PRIVATE)
                    self.assertIn(f'Effect color history diagnostic::depth={depth} action={action} before=0 after=1', output)
        output = self.summarize(header + 'PB_EFFECT_COLOR_HISTORY depth=16 action=source before=65535 after=0\n' + PRIVATE)
        self.assertIn('before=65535 after=0', output)

    def test_effect_color_history_record_rejects_unknown_private_malformed_and_out_of_bounds_data(self):
        name = 'ui::effect_color::tests::effect_color_hex_is_live_provisional_and_cancels_on_escape_invalid_input_and_source_changes'
        header = 'test ' + name + ' ... FAILED\n---- ' + name + ' stdout ----\n'
        valid = 'PB_EFFECT_COLOR_HISTORY depth=8 action=escape before=0 after=1'
        invalid = [
            valid.replace('depth=8', 'depth=32'),
            valid.replace('action=escape', 'action=enter'),
            valid.replace('action=escape', 'action=' + PRIVATE),
            valid.replace('before=0', 'before=-1'),
            valid.replace('before=0', 'before=00'),
            valid.replace('before=0', 'before=65536'),
            valid.replace('after=1', 'after=65536'),
            valid.replace('after=1', 'after=100000'),
            valid.replace('after=1', 'after=' + PRIVATE),
            valid.replace('after=1', 'after=0'),
            PRIVATE + valid,
            valid + ' ' + PRIVATE,
            valid + '\n' + valid,
        ]
        for marker in invalid:
            with self.subTest(marker=marker):
                output = self.summarize(header + marker + '\n' + PRIVATE)
                self.assertNotIn('Effect color history diagnostic', output)
        output = self.summarize(header.replace(name, 'other::fixture') + valid + '\n' + PRIVATE)
        self.assertNotIn('Effect color history diagnostic', output)

    def test_unknown_output_never_turns_warning_locations_into_compiler_errors(self):
        output = self.summarize('warning: example\n  --> src/gpu.rs:8:2\n' + PRIVATE)
        self.assertIn('UNKNOWN', output)
        self.assertNotIn('file=src/gpu.rs', output)
        self.assertNotIn('Compiler diagnostic location', output)

    def test_anchored_categories_never_echo_paths_or_arbitrary_native_details(self):
        cases = [
            ('error: test failed, to rerun pass --lib', 'CARGO_TEST_FAILURE'),
            ('\x1b[31merror: test failed, to rerun pass --lib\x1b[0m', 'CARGO_TEST_FAILURE'),
            ('error: could not compile peerbrush', 'CARGO_BUILD_FAILURE'),
            ('error: failed to run custom build command for package', 'BUILD_SCRIPT_FAILURE'),
            (f'error: linking with /{PRIVATE}/linker failed: exit status: 1', 'LINKER_FAILURE'),
            ('error: the compiler unexpectedly panicked', 'COMPILER_PANIC'),
            ("thread 'rustc' panicked at /" + PRIVATE, 'COMPILER_PANIC'),
            ('LLVM ERROR: out of memory', 'OUT_OF_MEMORY_DIAGNOSTIC'),
            ('memory allocation of 2097152 bytes failed', 'OUT_OF_MEMORY_DIAGNOSTIC'),
            ('error: failed to write: No space left on device', 'DISK_FULL_DIAGNOSTIC'),
            ('error: failed to write (os error 112)', 'DISK_FULL_DIAGNOSTIC'),
        ]
        for log, expected in cases:
            with self.subTest(category=expected):
                output = self.summarize(log + '\n' + PRIVATE)
                self.assertIn(expected, output)
                self.assertNotIn('Compiler diagnostic location', output)
        output = self.summarize('arbitrary text says error: test failed\n' + PRIVATE)
        self.assertIn('UNKNOWN', output)

    def test_child_termination_is_numeric_bounded_and_does_not_infer_out_of_memory(self):
        cases = [
            ('signal: 9, SIGKILL: kill', 'CHILD_SIGNAL_9'),
            ('signal: 11, SIGSEGV: invalid memory reference', 'CHILD_SIGNAL_11'),
            ('exit status: 139', 'CHILD_EXIT_139'),
            ('exit code: 0xc0000005', 'CHILD_EXIT_3221225477'),
            ('signal: 128', 'UNKNOWN'),
            ('signal: 0', 'UNKNOWN'),
            ('exit status: 9999999999', 'UNKNOWN'),
        ]
        for ending, expected in cases:
            with self.subTest(ending=ending):
                output = self.summarize(f"  process didn't exit successfully: /{PRIVATE}/rustc --argument ({ending})\n")
                self.assertIn(expected, output)
                self.assertNotIn('OUT_OF_MEMORY_DIAGNOSTIC', output)
        rows = ''.join(f"  process didn't exit successfully: /{PRIVATE}/child (exit status: {number})\n" for number in range(1, 41))
        output = self.summarize(rows)
        self.assertIn('CHILD_EXIT_8', output)
        self.assertNotIn('CHILD_EXIT_9', output)

    def test_compiler_codes_take_precedence_and_allow_only_repository_locations(self):
        output = self.summarize('error[E0433]: hidden detail\nerror[E0308]: hidden detail\n'
                                '  --> src/source.rs:45:6\n  --> /' + PRIVATE + '/file.rs:9:3\n'
                                'LLVM ERROR: out of memory\n')
        self.assertIn('E0308, E0433', output)
        self.assertIn('file=src/source.rs,line=45,col=6', output)
        self.assertNotIn('OUT_OF_MEMORY_DIAGNOSTIC', output)
        output = self.summarize('\n'.join(f'error[E{number:04}]: {PRIVATE}' for number in range(40)))
        self.assertIn('E0031', output)
        self.assertNotIn('E0032', output)


if __name__ == '__main__':
    unittest.main()
