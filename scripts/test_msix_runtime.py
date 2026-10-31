"""Read-only MCP pipe smoke test for an already installed/launched package.

Does not install, sign, enable aliases, change trust, or edit AI client config.
Start PeerBrush with a disposable --state-dir before running this check.
"""
import argparse
import json
import pathlib
import queue
import re
import subprocess
import sys
import threading
import time


def installed_alias(path):
    """Read the actual APPEXECLINK tag; a copied EXE with this name is not an alias."""
    import ctypes
    from ctypes import wintypes
    class Tag(ctypes.Structure):
        _fields_ = [('attributes', wintypes.DWORD), ('tag', wintypes.DWORD)]
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                                   wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.GetFileInformationByHandleEx.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    kernel.GetFileInformationByHandleEx.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    handle = kernel.CreateFileW(str(path), 0x80, 7, None, 3, 0x02200000, None)
    if handle in [None, ctypes.c_void_p(-1).value]:
        return False
    try:
        tag = Tag()
        return bool(kernel.GetFileInformationByHandleEx(handle, 9, ctypes.byref(tag), ctypes.sizeof(tag))
                    and tag.attributes & 0x400 and not tag.attributes & 0x10 and tag.tag == 0x8000001B)
    finally:
        kernel.CloseHandle(handle)


def validate_installation_observation(raw, *, platform, family=None, expected_route=None):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result: raise ValueError('Duplicate installation observation field')
            result[key] = value
        return result
    if not 0 < len(raw) <= 16 * 1024:
        raise ValueError('Installation observation exceeds its byte bound')
    record = json.loads(raw, object_pairs_hook=unique)
    if (not isinstance(record, dict) or set(record) != {'schema', 'format', 'platform', 'build', 'package',
            'installed_acceptance', 'update_eligible'} or type(record['schema']) is not int or record['schema'] != 1
            or record['format'] != 'PeerBrushInstallationObservationV1' or record['platform'] != platform
            or record['installed_acceptance'] != 'not-established' or record['update_eligible'] is not False):
        raise ValueError('Installation observation cannot grant installed acceptance or update eligibility')
    build = record['build']
    if (not isinstance(build, dict) or set(build) != {'schema', 'format', 'product', 'version', 'commit', 'tree', 'end'}
            or type(build['schema']) is not int or build['schema'] != 1 or build['format'] != 'PeerBrushBuildInfoV1'
            or build['product'] != 'PeerBrush' or build['end'] != 'PeerBrushBuildInfoEnd'
            or type(build['version']) is not str or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?', build['version'])
            or type(build['commit']) is not str or not re.fullmatch(r'(?:[0-9a-f]{40}|unknown)', build['commit'])
            or build['tree'] not in ['clean', 'dirty', 'unknown']):
        raise ValueError('Installation observation has invalid public build metadata')
    package = record['package']
    if package is None:
        if family is not None or expected_route is not None:
            raise ValueError('Installed alias launched an unpackaged process')
        return record
    if (platform != 'windows' or not isinstance(package, dict) or set(package) != {'name', 'publisher', 'family', 'full_name',
            'version', 'signature_kind', 'appinstaller_association', 'development_mode', 'non_main', 'candidate_route'}
            or any(type(package[k]) is not str or not 0 < len(package[k]) <= 1024 or '\0' in package[k]
                   for k in ['name', 'publisher', 'family', 'full_name'])
            or type(package['version']) is not list or len(package['version']) != 4
            or any(type(v) is not int or not 0 <= v <= 65535 for v in package['version'])
            or any(type(package[k]) is not bool for k in ['appinstaller_association', 'development_mode', 'non_main'])
            or package['signature_kind'] not in ['developer', 'enterprise', 'store', 'system', 'untrusted-or-unknown']):
        raise ValueError('Invalid native package observation')
    kind = package['signature_kind']
    route = ('ineligible' if package['development_mode'] or package['non_main'] else
             'microsoft-store' if kind == 'store' else
             ('legacy-appinstaller' if package['appinstaller_association'] else 'verified-local-candidate')
             if kind in ['developer', 'enterprise'] else 'ineligible')
    if (package['candidate_route'] != route or (family is not None and (package['family'] != family or route == 'ineligible'))
            or (expected_route is not None and route != expected_route)
            or (family is not None and (build['tree'] != 'clean' or build['commit'] == 'unknown'))):
        raise ValueError('Installed process identity or expected update route differs')
    return record


def observe_installation(executable, *, family=None, expected_route=None):
    try:
        process = subprocess.Popen([str(executable), '--installation-info'], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    except OSError:
        raise ValueError('Cannot launch the selected installation observation') from None
    output = queue.Queue(maxsize=1)
    def read():
        try: output.put(process.stdout.read(16 * 1024 + 1))
        except (OSError, ValueError): output.put(b'')
    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    try:
        try: raw = output.get(timeout=15)
        except queue.Empty: raise ValueError('Installation observation timed out') from None
        if len(raw) > 16 * 1024:
            raise ValueError('Installation observation exceeds its byte bound')
        try: status = process.wait(timeout=3)
        except subprocess.TimeoutExpired: raise ValueError('Installation observation did not exit') from None
        if status != 0:
            raise ValueError('Installation observation process failed')
        platform = {'win32': 'windows', 'darwin': 'macos', 'linux': 'linux'}.get(sys.platform)
        return validate_installation_observation(raw, platform=platform, family=family, expected_route=expected_route)
    finally:
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=3)
            except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=3)
        reader.join(timeout=1)
        # An unknown inherited writer can keep a pipe open after parent exit.
        # Never block closing a BufferedReader held by that read thread.
        if not reader.is_alive(): process.stdout.close()


def read_lines(stream, lines):
    try:
        for line in iter(stream.readline, b''):
            lines.put(line)
    finally:
        lines.put(None)


def request(process, lines, identifier, method, params=None):
    message={'jsonrpc':'2.0','id':identifier,'method':method}
    if params is not None:message['params']=params
    process.stdin.write((json.dumps(message)+'\n').encode())
    process.stdin.flush()
    deadline=time.monotonic()+15
    while True:
        remaining=deadline-time.monotonic()
        if remaining<=0:raise ValueError(f'{method}: timed out waiting for redirected stdout')
        try:line=lines.get(timeout=remaining)
        except queue.Empty:raise ValueError(f'{method}: timed out waiting for redirected stdout') from None
        if line is None:raise ValueError(f'{method}: MCP process exited before replying')
        try:response=json.loads(line)
        except (ValueError,UnicodeError):raise ValueError('MCP stdout contains non-JSON output') from None
        if not isinstance(response,dict):raise ValueError('MCP stdout contains a non-object response')
        if response.get('id')!=identifier:continue
        if response.get('error') is not None:raise ValueError(f'{method}: MCP returned an error')
        result=response.get('result')
        if isinstance(result,dict) and result.get('isError'):
            raise ValueError(f'{method}: MCP tool call failed; no live application bridge was verified')
        return result


def verify_live_bridge(process, lines, identifier):
    # initialize/tools/list are intentionally served by the adapter while the
    # application is offline. Only a real tool call exercises authenticated RPC.
    result=request(process,lines,identifier,'tools/call',{
        'name':'peerbrush_projects','arguments':{'action':'list'},
    })
    if not isinstance(result,dict) or result.get('isError') is not False:
        raise ValueError('Project listing did not confirm a live application bridge')
    content=result.get('content')
    if not isinstance(content,list):raise ValueError('Project listing returned no content')
    text=next((part.get('text') for part in content if isinstance(part,dict) and part.get('type')=='text'),None)
    try:projects=json.loads(text)
    except (ValueError,TypeError):raise ValueError('Project listing returned invalid application data') from None
    if not isinstance(projects,dict) or not isinstance(projects.get('projects'),list) or not projects['projects'] or not projects.get('active_project_id'):
        raise ValueError('Project listing did not return a live workspace')


def smoke(executable, state_dir, allow_portable=False, expected_route=None):
    executable=executable.absolute()
    if not executable.exists():raise ValueError('Executable/alias is not present')
    if not allow_portable:
        if sys.platform!='win32':raise ValueError('Installed alias checks require Windows')
        if executable.name.lower()!='peerbrush-mcp.exe' or executable.parent.parent.name.lower()!='windowsapps':
            raise ValueError('Pass the absolute package-family-scoped MCP alias')
        if not installed_alias(executable):
            raise ValueError('Selected path is not a native Windows app execution alias')
    elif expected_route is not None:
        raise ValueError('An expected installed route cannot be qualified with portable mode')
    if not state_dir.is_absolute() or not (state_dir/'connection.json').is_file():
        raise ValueError('Pass the disposable state directory of an already running PeerBrush')
    observation = observe_installation(executable, family=None if allow_portable else executable.parent.name,
                                      expected_route=expected_route)
    processes=[]
    sessions=[]
    try:
        for index in range(2):
            process=subprocess.Popen([str(executable),'mcp','--state-dir',str(state_dir)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
            processes.append(process)
            lines=queue.Queue()
            sessions.append((process,lines))
            threading.Thread(target=read_lines,args=(process.stdout,lines),daemon=True).start()
            result=request(process,lines,index*10+1,'initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'PeerBrush package pipe test','version':'1'}})
            if not isinstance(result,dict) or result.get('serverInfo',{}).get('name')!='PeerBrush':
                raise ValueError('Unexpected initialize response')
            process.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            process.stdin.flush()
            tools=request(process,lines,index*10+2,'tools/list')
            if not isinstance(tools,dict) or not tools.get('tools'):
                raise ValueError('No PeerBrush tools were returned')
            verify_live_bridge(process,lines,index*10+3)
        # Both adapters must still reach the application once the second exists.
        verify_live_bridge(*sessions[0],21)
        verify_live_bridge(*sessions[1],22)
        if any(process.poll() is not None for process in processes):
            raise ValueError('An MCP session closed while a second client connected')
        return {'passed':True,'transport':'stdio','parallel_clients':2,'checks':['bounded read-only installation observation','redirected initialize','JSON-only stdout','tools/list','authenticated projects/list from both clients','first-client bridge call after second-client connection'],'live_application_bridge':True,'native_gui_verified':False,'installed_alias':not allow_portable,'installation_observation':observation,'installed_acceptance':'not-established'}
    finally:
        for process in processes:
            if process.stdin:process.stdin.close()
            try:process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:process.wait(timeout=3)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=3)
            if process.stdout:process.stdout.close()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--executable',type=pathlib.Path,required=True)
    parser.add_argument('--state-dir',type=pathlib.Path,required=True)
    parser.add_argument('--allow-portable',action='store_true',help='Test the ordinary executable; does not establish installed alias behavior')
    parser.add_argument('--expected-route', choices=['verified-local-candidate', 'legacy-appinstaller', 'microsoft-store'],
                        help='Require this observed installed route; does not grant update eligibility')
    args=parser.parse_args()
    try:print(json.dumps(smoke(args.executable,args.state_dir,args.allow_portable,args.expected_route),indent=2))
    except (ValueError,OSError,subprocess.SubprocessError) as error:parser.exit(1,'MCP smoke test failed: '+str(error)+'\n')


if __name__=='__main__':main()
