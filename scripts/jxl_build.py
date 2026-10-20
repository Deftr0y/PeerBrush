"""Select existing reviewed native tools; never download or install build tools."""
import argparse
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess

# Official hosted-image inventories plus the privately tested Windows tools.
# A changed image needs an explicit reviewed adjustment, never a latest fetch.
VERSIONS = {
    'Windows': {'clang': {'20.1.8', '21.1.8'}, 'cmake': {'3.31.6', '3.31.12', '4.4.4'}},
    'Linux': {'clang': {'18.1.3'}, 'cmake': {'3.31.6'}},
    'Darwin': {'clang': {'21.0.0'}, 'cmake': {'4.4.2', '4.4.4'}},
}


def reject_overrides(environment):
    forbidden = {'DEP_JXL_PATH', 'DEP_JXL_LIB'}
    if any(key.upper() in forbidden for key in environment):
        raise ValueError('Native JPEG XL source/library overrides are not permitted')


def tool(name, kind, allowed, environment):
    found = shutil.which(name, path=environment.get('PATH', ''))
    if not found:
        raise ValueError('Missing native JPEG XL build tool: ' + name)
    path = Path(found).resolve(strict=True)
    if not path.is_file():
        raise ValueError('Native JPEG XL build tool is not a file')
    result = subprocess.run([str(path), '--version'], env=environment, capture_output=True,
                            check=True, timeout=10)
    if len(result.stdout) > 16384 or len(result.stderr) > 16384:
        raise ValueError('Native build tool version output exceeds bound')
    output = result.stdout.decode('utf-8', errors='strict')
    pattern = {'clang': r'(?:Apple )?clang version ([0-9]+\.[0-9]+\.[0-9]+)',
               'cmake': r'^cmake version ([0-9]+\.[0-9]+\.[0-9]+)\s',
               'ninja': r'^([0-9]+\.[0-9]+\.[0-9]+)\s*$'}[kind]
    match = re.search(pattern, output)
    if not match or match[1] not in allowed:
        prefix = {'clang': r'(?:Apple )?clang version ', 'cmake': r'^cmake version ', 'ninja': r'^'}[kind]
        candidate = re.search(prefix + r'([0-9]{1,5}\.[0-9]{1,5}\.[0-9]{1,5})(?![0-9.])', output)
        numeric = candidate[1] if candidate else 'unparsed'
        raise ValueError('Unqualified native JPEG XL build tool version: ' + name + ' (numeric=' + numeric + ')')
    return str(path), match[1]


def configure(inherited=None):
    environment = dict(os.environ if inherited is None else inherited)
    reject_overrides(environment)
    system = platform.system()
    if system not in VERSIONS:
        raise ValueError('Native JPEG XL build is not configured on this platform')
    compiler, cpp = {'Windows': ('clang-cl', 'clang-cl'),
                     'Linux': ('clang-18', 'clang++-18'),
                     'Darwin': ('clang', 'clang++')}[system]
    cc, cc_version = tool(compiler, 'clang', VERSIONS[system]['clang'], environment)
    cxx, cxx_version = tool(cpp, 'clang', VERSIONS[system]['clang'], environment)
    if cc_version != cxx_version:
        raise ValueError('Native C/C++ compiler versions differ')
    cmake, cmake_version = tool('cmake', 'cmake', VERSIONS[system]['cmake'], environment)
    ninja, ninja_version = tool('ninja', 'ninja', {'1.13.2'}, environment)
    if system == 'Windows' and not all(environment.get(key) for key in ['INCLUDE', 'LIB']):
        raise ValueError('Initialize the existing MSVC/SDK developer environment first')
    path = os.pathsep.join(dict.fromkeys([str(Path(n).parent) for n in [cc, cmake, ninja]]))
    environment.update(CC=cc, CXX=cxx, CMAKE=cmake, CMAKE_GENERATOR='Ninja',
                       CARGO_BUILD_JOBS='2', CMAKE_BUILD_PARALLEL_LEVEL='2',
                       PATH=path + os.pathsep + environment.get('PATH', ''))
    return environment, {'compiler': cc_version, 'cmake': cmake_version, 'ninja': ninja_version}


def write_github_environment(path, environment):
    names = ['CC', 'CXX', 'CMAKE', 'CMAKE_GENERATOR', 'CARGO_BUILD_JOBS',
             'CMAKE_BUILD_PARALLEL_LEVEL', 'PATH']
    if platform.system() == 'Windows':
        names += ['INCLUDE', 'LIB', 'LIBPATH']
    rows = []
    for key in names:
        value = environment.get(key, '')
        if '\n' in value or '\r' in value or '\0' in value:
            raise ValueError('Native build environment contains a multiline value')
        rows.append(key + '=' + value + '\n')
    with Path(path).open('a', encoding='utf-8', newline='\n') as target:
        target.writelines(rows)


def run_cargo(arguments, environment):
    arguments = list(arguments)
    if arguments[:1] == ['--']:
        arguments = arguments[1:]
    if not arguments or arguments[0] not in ['build', 'check', 'test', 'run', 'clippy', 'fmt']:
        raise ValueError('Use build, check, test, run, clippy or fmt after --')
    subprocess.run(['cargo', *arguments], cwd=Path(__file__).resolve().parents[1],
                   env=environment, check=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--github-env', action='store_true')
    parser.add_argument('cargo_args', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    environment, versions = configure()
    if args.github_env:
        path = os.environ.get('GITHUB_ENV')
        if not path:
            raise SystemExit('Missing GitHub step environment destination')
        write_github_environment(path, environment)
    print('Native JPEG XL tools: compiler=' + versions['compiler']
          + ' cmake=' + versions['cmake'] + ' ninja=' + versions['ninja'])

    if args.cargo_args:
        run_cargo(args.cargo_args, environment)
