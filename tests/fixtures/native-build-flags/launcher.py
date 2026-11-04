import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(os.environ['PB_FLAGS_ROOT'])
for language in ['c', 'cpp']:
    if any(argument.replace('\\', '/').endswith('/native.' + language) for argument in sys.argv[1:]):
        with (root / ('cmake-' + language + '.args.json')).open('x', encoding='utf-8') as stream:
            json.dump(sys.argv[1:], stream)
        break
raise SystemExit(subprocess.run(sys.argv[1:], check=False).returncode)
