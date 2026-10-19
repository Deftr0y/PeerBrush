"""Read-only MCP pipe smoke test for an already installed/launched package.

Does not install, sign, enable aliases, change trust, or edit AI client config.
Start PeerBrush with a disposable --state-dir before running this check.
"""
import argparse
import json
import pathlib
import queue
import subprocess
import sys
import threading
import time


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


def smoke(executable, state_dir, allow_portable=False):
    executable=executable.absolute()
    if not executable.exists():raise ValueError('Executable/alias is not present')
    if not allow_portable:
        if sys.platform!='win32':raise ValueError('Installed alias checks require Windows')
        if executable.name.lower()!='peerbrush-mcp.exe' or executable.parent.parent.name.lower()!='windowsapps':
            raise ValueError('Pass the absolute package-family-scoped MCP alias')
    if not state_dir.is_absolute() or not (state_dir/'connection.json').is_file():
        raise ValueError('Pass the disposable state directory of an already running PeerBrush')
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
        return {'passed':True,'transport':'stdio','parallel_clients':2,'checks':['redirected initialize','JSON-only stdout','tools/list','authenticated projects/list from both clients','first-client bridge call after second-client connection'],'live_application_bridge':True,'native_gui_verified':False,'installed_alias':not allow_portable}
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
    args=parser.parse_args()
    try:print(json.dumps(smoke(args.executable,args.state_dir,args.allow_portable),indent=2))
    except (ValueError,OSError,subprocess.SubprocessError) as error:parser.exit(1,'MCP smoke test failed: '+str(error)+'\n')


if __name__=='__main__':main()
