"""Package a historical application rebuilt with remapped compiler paths."""
import hashlib,json,os,pathlib,subprocess,tempfile,tomllib,zipfile
import package as p
from verify_release import verify
root=pathlib.Path(__file__).resolve().parents[1]
metadata=json.loads((root/'scripts/checkpoint.json').read_text())
allowed={'v0.1.2':'f1c3476b0ec098a5d772f62a35042f21c2fc8dd8','v0.1.4':'8adadb8f6471214dc96a1c79d2c834b07f8f7ebc','development-d70f64e':'d70f64eca57083bf4bf43c39cd74eaad6fb8298a'}
assert allowed.get(metadata['tag'])==metadata['original_commit']
platform={'win32':'Windows','darwin':'macOS'}.get(__import__('sys').platform,'Linux')
assert metadata['tag']=='development-d70f64e' or platform=='Windows'
version=tomllib.loads((root/'Cargo.toml').read_text())['package']['version']
assert version==metadata['version']
binary=root/'target/release'/('peerbrush.exe' if platform=='Windows' else 'peerbrush')
p.validate_binary(binary.read_bytes())
p.equivalent_build_inputs(root,p.source_files(root),metadata['original_commit'])
request={'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'package-smoke','version':'1'}}}
with tempfile.TemporaryDirectory(prefix='peerbrush-package-smoke-') as state:
    smoke=subprocess.run([str(binary),'mcp','--state-dir',state],input=json.dumps(request)+'\n',capture_output=True,text=True,timeout=30,check=True)
    replies=[json.loads(line) for line in smoke.stdout.splitlines()]
    response=next(row for row in replies if row.get('id')==1)
    assert response['result']['serverInfo']['version']==version
out=root/'dist'
archive=p.package(root,binary,out,pathlib.Path(os.environ.get('CARGO_HOME',str(pathlib.Path.home()/'.cargo')))/'registry/src',platform,verify_binary=False,rust_docs=pathlib.Path(subprocess.check_output(['rustc','--print','sysroot'],text=True).strip())/'share/doc/rust',binary_source_commit=metadata['original_commit'])
with zipfile.ZipFile(archive) as old:
    files=[]
    for item in old.infolist():
        data=old.read(item)
        if item.filename.endswith('/release.json'):
            manifest=json.loads(data);manifest['packaging_correction']='2026-10-10; rebuilt historical application with path remapping'
            data=(json.dumps(manifest,indent=2)+'\n').encode()
        files.append((item.filename,data,0o755 if item.filename.endswith('/peerbrush.exe') or item.filename.endswith('/peerbrush') else 0o644))
corrected=archive.with_name(archive.stem+'-rebuilt-'+metadata['correction_date']+'.zip')
p.write_zip(corrected,files,int(p.git(root,'show','-s','--format=%ct','HEAD')))
archive.unlink()
if metadata['tag']!='development-d70f64e':
    verify(out,p.git(root,'rev-parse','HEAD').decode().strip(),version,root/'verified',root,required_platforms={'Windows'})
print(json.dumps({'platform':platform,'application_version':version,'original_application_commit':metadata['original_commit'],'headless_mcp_version_smoke':'passed','binary_paths':'remapped'}))
