"""Run a fixed small set of existing release-critical cases, never the full suite."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile

CASES = (
    ('core', 'failed_batch_is_atomic'),
    ('core', 'undo_redo_and_mask_paint_roundtrip'),
    ('core', 'psd_preserves_sources_and_actual_composite'),
    ('selection_system', 'selected_clear_preserves_native_samples_holes_preview_psd_and_one_step_undo'),
    ('psb', 'protocol_psb_save_open_external_conflict_and_failed_open_preserve_original_work'),
    ('file_safety', 'same_length_restored_timestamp_replacement_and_path_aliases_cannot_bypass_conflicts'),
    ('file_safety', 'failures_preserve_destination_and_work_and_successful_retention_is_bounded'),
    ('production_export', 'native_composite_presets_match_current_png_filters_depth_and_icc_without_editing'),
    ('production_export', 'lossless_webp_retains_rendered_alpha_icc_and_declares_derived_eight_bit_output'),
    ('lib', 'jpeg_xl::stream_parent::tests::actual_profile_staging_owned_mode1_worker_replay_and_isolated_decode_keep_native_samples'),
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='Validate the selection without running tests')
    parser.add_argument('--case', choices=[f'{target}:{name}' for target, name in CASES], help='Recheck only one failed case on a repaired candidate')
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    assert len(set(CASES)) == len(CASES)
    for target, name in CASES:
        source = root/'src/jpeg_xl/stream_parent.rs' if target == 'lib' else root/'tests'/f'{target}.rs'
        function = name.rsplit('::', 1)[-1]
        if not re.search(r'\bfn\s+' + re.escape(function) + r'\s*\(', source.read_text(encoding='utf-8')):
            parser.error(f'Essential case no longer exists: {target}:{name}')
    if args.check:
        print(f'Validated {len(CASES)} existing essential cases; no tests executed.')
        return
    selected = [(target, name) for target, name in CASES if not args.case or args.case == f'{target}:{name}']
    failures = []
    with tempfile.TemporaryDirectory(prefix='peerbrush-release-check-') as temporary:
        for index, (target, name) in enumerate(selected):
            command = ['cargo', 'test', '--locked']
            command += ['--lib'] if target == 'lib' else ['--test', target]
            command += [name, '--', '--exact', '--test-threads=1']
            result = subprocess.run(command, cwd=root, text=True, encoding='utf-8', errors='replace', capture_output=True, env={**os.environ, 'CARGO_TERM_COLOR':'never'})
            log = result.stdout + result.stderr
            Path(temporary, f'{index}.log').write_text(log, encoding='utf-8')
            # Exact names must execute one test: a missing/ignored case is not success.
            passed = result.returncode == 0 and re.search(r'test result: ok\. 1 passed; 0 failed; 0 ignored;', log)
            print(f'{"PASS" if passed else "FAIL"} {target}:{name}', flush=True)
            if not passed:
                failures.append(f'{target}:{name}')
    if failures:
        raise SystemExit('Release blocked. Repair and recheck: ' + ', '.join(failures))
    print('Essential cases passed for this source only; outstanding feature and signed-package acceptance still apply.')


if __name__ == '__main__':
    main()
