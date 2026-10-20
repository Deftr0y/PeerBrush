"""Build native release binaries without embedding personal checkout paths."""
import argparse
import hashlib
import os
import pathlib
import subprocess
from package import git
from jxl_build import configure, reject_overrides


def source_input_state(root):
    """Cargo invalidation only; build.rs independently obtains the source label."""
    try:
        top = pathlib.Path(git(root, 'rev-parse', '--show-toplevel').decode().strip()).resolve()
        if top != root.resolve():
            return 'unknown'
        parts = [git(root, *args) for args in [('rev-parse', 'HEAD'), ('status', '--porcelain=v1', '-z', '--untracked-files=normal'), ('ls-files', '-v', '-z')]]
        return hashlib.sha256(b'\0'.join(parts)).hexdigest()
    except (OSError, UnicodeError, subprocess.SubprocessError):
        return 'unknown'


def release_environment(root, inherited=None):
    environment = dict(os.environ if inherited is None else inherited)
    reject_overrides(environment)
    environment['PEERBRUSH_BUILD_INPUT_STATE'] = source_input_state(root)
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
    # Keep native source/macro paths subject to the same package privacy gate.
    for key in ['CFLAGS', 'CXXFLAGS']:
        native = [environment[key]] if environment.get(key) else []
        for prefix, replacement in prefixes:
            for spelling in dict.fromkeys([str(prefix), prefix.as_posix()]):
                if any(c in spelling for c in ['\"', '\r', '\n', '\0']):
                    raise ValueError('Native path remapping contains an unsupported character')
                flag = '-ffile-prefix-map=' + spelling + '=' + replacement
                native.append('\"' + ('/clang:' if os.name == 'nt' else '') + flag + '\"')
        environment[key] = ' '.join(native)
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
    environment, _ = configure(release_environment(root))
    subprocess.run(command, cwd=root, env=environment, check=True)
