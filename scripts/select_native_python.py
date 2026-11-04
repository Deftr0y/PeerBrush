"""Pin the already installed macOS packaging interpreter before tool PATH changes."""
import os
from pathlib import Path
import sys


def selected_directory():
    if sys.version_info < (3, 11):
        raise ValueError('Native packaging requires Python 3.11 or newer')
    executable = Path(sys.executable)
    if not executable.is_absolute() or not executable.is_file():
        raise ValueError('Native packaging requires an existing absolute Python executable')
    directory = executable.parent
    if any(character in str(directory) for character in ['\r', '\n', '\0']):
        raise ValueError('Native packaging Python path is not one environment-file entry')
    for name in ['python', 'python3']:
        alias = directory / name
        if not alias.is_file() or not alias.samefile(executable):
            raise ValueError('Native packaging Python aliases must select the same interpreter')
    return directory


def main():
    directory = selected_directory()
    destination = os.environ.get('GITHUB_PATH')
    if not destination:
        raise ValueError('Missing GitHub step path destination')
    with Path(destination).open('a', encoding='utf-8', newline='\n') as stream:
        stream.write(str(directory) + '\n')
    print('Native packaging Python: ' + '.'.join(map(str, sys.version_info[:3])))


if __name__ == '__main__':
    main()
