"""Build native release binaries without embedding personal checkout paths."""
import argparse
import hashlib
import os
import pathlib
import re
import shlex
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


def native_shell_flags(value):
    """Match locked cc's shlex parser; newlines/control input is rejected by the caller."""
    arguments, word = [], []
    quote, active = None, False
    index = 0
    while index < len(value):
        character = value[index]
        index += 1
        if quote == "'":
            if character == "'":
                quote = None
            else:
                word.append(character)
        elif quote == '"':
            if character == '"':
                quote = None
            elif character == '\\':
                if index == len(value):
                    raise ValueError('Malformed native shell compiler flags')
                following = value[index]
                index += 1
                if following not in '$`"\\':
                    word.append('\\')
                word.append(following)
            else:
                word.append(character)
        elif character in ' \t':
            if active:
                arguments.append(''.join(word))
                word, active = [], False
        elif character == '#' and not active:
            break
        elif character in "'\"":
            quote, active = character, True
        elif character == '\\':
            if index == len(value):
                raise ValueError('Malformed native shell compiler flags')
            word.append(value[index])
            index += 1
            active = True
        else:
            word.append(character)
            active = True
    if quote is not None:
        raise ValueError('Malformed native shell compiler flags')
    if active:
        arguments.append(''.join(word))
    return arguments


def release_environment(root, inherited=None):
    environment = dict(os.environ if inherited is None else inherited)
    if 'PEERBRUSH_SETUP_OFFER' in environment:
        raise ValueError('Setup offers are selected only by the separate reviewed setup producer')
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
    # cc defaults to ASCII whitespace; preserve inherited argv before enabling shell parsing.
    shell_flags = environment.get('CC_SHELL_ESCAPED_FLAGS', '') not in ['', '0', 'false', 'no']
    native_flags = {}
    for key, value in environment.items():
        if re.fullmatch(r'(?:(?:HOST|TARGET)_)?(?:CFLAGS|CXXFLAGS|ARFLAGS|RANLIBFLAGS)(?:_.+)?', key):
            if any((ord(character) < 32 and character != '\t') or ord(character) == 127 for character in value):
                raise ValueError('Native compiler flags contain an unsupported control character')
            native_flags[key] = native_shell_flags(value) if shell_flags else re.findall(r'[^ \t\n\r\v\f]+', value)
    for key in ['CFLAGS', 'CXXFLAGS']:
        native = native_flags.setdefault(key, [])
        for prefix, replacement in prefixes:
            for spelling in dict.fromkeys([str(prefix), prefix.as_posix()]):
                if any(c in spelling for c in ['\"', '\r', '\n', '\0']):
                    raise ValueError('Native path remapping contains an unsupported character')
                native.append(('/clang:' if os.name == 'nt' else '') + '-ffile-prefix-map=' + spelling + '=' + replacement)
    environment.update({key: shlex.join(flags) for key, flags in native_flags.items()})
    environment['CC_SHELL_ESCAPED_FLAGS'] = '1'
    return environment


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--target-dir', type=pathlib.Path)
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--jobs', type=int)
    parser.add_argument('--setup-entry', action='store_true', help='Build only the separate dormant setup entry')
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    command = ['cargo', 'build', '--release', '--locked']
    command += ['--bin', 'peerbrush-setup', '--features', 'windows-setup'] if args.setup_entry else ['--bin', 'peerbrush']
    if args.target_dir:
        command += ['--target-dir', str(args.target_dir)]
    if args.offline:
        command += ['--offline']
    if args.jobs:
        command += ['--jobs', str(args.jobs)]
    environment, _ = configure(release_environment(root))
    subprocess.run(command, cwd=root, env=environment, check=True)
