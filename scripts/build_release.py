"""Build native release binaries without embedding personal checkout paths."""
import argparse
import os
import pathlib
import subprocess


def release_environment(root, inherited=None):
    environment = dict(os.environ if inherited is None else inherited)
    if environment.get('RUSTFLAGS') and not environment.get('CARGO_ENCODED_RUSTFLAGS'):
        raise ValueError('Use CARGO_ENCODED_RUSTFLAGS to preserve custom compiler options')
    home = pathlib.Path.home()
    # rustc uses the last matching prefix: keep the most specific one last.
    prefixes = [(home, 'build-home'),
                (pathlib.Path(environment.get('RUSTUP_HOME', home / '.rustup')).resolve(), 'rustup'),
                (pathlib.Path(environment.get('CARGO_HOME', home / '.cargo')).resolve(), 'cargo'),
                (root.resolve(), 'peerbrush')]
    flags = [environment['CARGO_ENCODED_RUSTFLAGS']] if environment.get('CARGO_ENCODED_RUSTFLAGS') else []
    for prefix, replacement in prefixes:
        for spelling in dict.fromkeys([str(prefix), prefix.as_posix()]):
            flags.append(f'--remap-path-prefix={spelling}={replacement}')
    environment['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(flags)
    return environment


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--target-dir', type=pathlib.Path)
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--jobs', type=int)
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    command = ['cargo', 'build', '--release', '--locked']
    if args.target_dir:
        command += ['--target-dir', str(args.target_dir)]
    if args.offline:
        command += ['--offline']
    if args.jobs:
        command += ['--jobs', str(args.jobs)]
    subprocess.run(command, cwd=root, env=release_environment(root), check=True)
