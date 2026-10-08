"""Package a native release, project source, and dependency/font notices."""
import argparse, os, pathlib, shutil, sys, tomllib, zipfile

parser=argparse.ArgumentParser()
parser.add_argument('--output',type=pathlib.Path)
parser.add_argument('--registry',type=pathlib.Path)
parser.add_argument('--binary',type=pathlib.Path,help='Native release binary to package; defaults to target/release')
args=parser.parse_args()
root=pathlib.Path(__file__).resolve().parents[1]
output=(args.output or root/'dist').resolve();output.mkdir(parents=True,exist_ok=True)
platform={'win32':'Windows','darwin':'macOS'}.get(sys.platform,'Linux')
bundle=output/f'PeerBrush-{platform}';bundle.mkdir(exist_ok=True)
exe='peerbrush.exe' if sys.platform=='win32' else 'peerbrush'
shutil.copy2(args.binary or root/'target'/'release'/exe,bundle/exe)
if sys.platform=='win32':shutil.copy2(root/'scripts'/'start-windows.cmd',bundle/'Start PeerBrush.cmd')
for name in ['LICENSE','README.md','FOLLOWUPS.MD']:shutil.copy2(root/name,bundle/name)
shutil.copytree(root/'docs',bundle/'docs',dirs_exist_ok=True,ignore=shutil.ignore_patterns('development-handoff.md'))
shutil.copytree(root/'assets',bundle/'assets',dirs_exist_ok=True)
source_files=[root/name for name in ['Cargo.toml','Cargo.lock','LICENSE','README.md','AGENTS.md','FOLLOWUPS.MD','.gitignore']]
for directory in ['src','tests','scripts','docs','assets','.github','vendor','examples']:
    source_files.extend(p for p in (root/directory).rglob('*') if p.is_file() and '__pycache__' not in p.parts and p.name!='development-handoff.md')
source_zip=output/'PeerBrush-source.zip'
with zipfile.ZipFile(source_zip,'w',zipfile.ZIP_DEFLATED,strict_timestamps=False) as archive:
    for path in source_files:archive.write(path,path.relative_to(root))
shutil.copy2(source_zip,bundle/source_zip.name)
cargo_home=pathlib.Path(os.environ.get('CARGO_HOME',str(pathlib.Path.home()/'.cargo')))
registry=args.registry or cargo_home/'registry'/'src'
notices=bundle/'licenses';notices.mkdir(exist_ok=True)
rows=['# Third-party notices','', 'Dependencies retain their original licenses. Sources and checksums are pinned by Cargo.lock in the included project source archive. This list includes downloaded platform dependencies; not every crate is linked into this binary.', '']
lock=tomllib.loads((root/'Cargo.lock').read_text())
for package in lock['package']:
    if 'source' not in package:continue
    key=f"{package['name']}-{package['version']}"
    matches=list(registry.glob('*/'+key))
    if not matches:continue
    crate=matches[0]
    meta=tomllib.loads((crate/'Cargo.toml').read_text(encoding='utf-8'))['package']
    rows.append(f"- {key}: {meta.get('license',meta.get('license-file','See source license'))}")
    candidates=[p for p in crate.iterdir() if p.is_file() and p.name.upper().startswith(('LICENSE','COPYING','NOTICE','UNLICENSE'))]
    if (crate/'fonts').exists():candidates.extend(p for p in (crate/'fonts').rglob('*.txt'))
    if meta.get('license-file'):candidates.append(crate/meta['license-file'])
    for path in set(candidates):
        if path.is_file():
            dest=notices/key/path.relative_to(crate);dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
for crate in (root/'vendor').iterdir():
    meta=tomllib.loads((crate/'Cargo.toml').read_text(encoding='utf-8'))['package']
    key=f"{meta['name']}-{meta['version']}"
    rows.append(f"- {key} (vendored input patch): {meta.get('license','See source license')}")
    for path in crate.glob('LICENSE*'):
        dest=notices/key/path.name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
shutil.copytree(root/'assets'/'fonts',notices/'ubuntu-sans',dirs_exist_ok=True)
rust_notices=root/'.dev-tools'/'rust'/'share'/'doc'/'rust'
if rust_notices.exists():shutil.copytree(rust_notices,notices/'rust-runtime',dirs_exist_ok=True)
(bundle/'THIRD_PARTY_NOTICES.md').write_text('\n'.join(rows)+'\n',encoding='utf-8')
package_zip=output/f'PeerBrush-{platform}.zip'
with zipfile.ZipFile(package_zip,'w',zipfile.ZIP_DEFLATED,strict_timestamps=False) as archive:
    for path in bundle.rglob('*'):
        if path.is_file():archive.write(path,path.relative_to(output))
print(f'Portable package: {package_zip}')
print(f'Project source: {source_zip}')
