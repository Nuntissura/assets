"""Quiet test-only Chromium ownership with bounded CDP startup and process cleanup.

Use ``async with owned_browser(playwright, executable) as browser`` after importing
the selected Playwright runtime. The caller supplies the browser/runtime pair.
Only the owned Popen root and its captured private-profile lineage may be stopped. Its profile, stderr and
owner receipt remain in ignored project-local storage for diagnosis; no recursive
profile deletion or discovery of user browsers is performed.
"""
import asyncio
from contextlib import asynccontextmanager
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


async def _owned_blocking(function, *arguments):
    """Keep driver I/O responsive; cancellation must not abandon owned cleanup."""
    task = asyncio.create_task(asyncio.to_thread(function, *arguments))
    cancelled = False
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError:
            cancelled = True
    return task.result(), cancelled


def _windows_process_state(pid, stamp, terminate=False):
    """Query only a captured identity; a signaled OS handle proves actual exit."""
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    kernel.GetProcessTimes.restype = wintypes.BOOL
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.WaitForSingleObject.restype = wintypes.DWORD
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    kernel.TerminateProcess.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    rights = 0x00100000 | 0x1000 | (0x0001 if terminate else 0)
    handle = kernel.OpenProcess(rights, False, int(pid))  # SYNCHRONIZE | QUERY_LIMITED_INFORMATION [+ TERMINATE]
    if not handle:
        error = ctypes.get_last_error()
        if error == 87:  # PID no longer identifies a process.
            return 'absent'
        raise OSError(error, f'Cannot observe captured browser process PID {pid}')
    try:
        times = [wintypes.FILETIME() for _ in range(4)]
        if not kernel.GetProcessTimes(handle, *(ctypes.byref(value) for value in times)):
            raise ctypes.WinError(ctypes.get_last_error())
        birth = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        # FILETIME starts in 1601, .NET ticks in year 1; CIM retains microseconds.
        ticks = birth + 504911232000000000
        if ticks // 10 != int(stamp) // 10:
            return 'identity-replaced'
        state = kernel.WaitForSingleObject(handle, 0)
        if state == 0:
            return 'exited'
        if state == 258:
            if terminate:
                # Use the exact birth-checked handle, avoiding PID lookup/reuse at stop.
                if not kernel.TerminateProcess(handle, 1):
                    error = ctypes.get_last_error()
                    # Termination can already be in progress; error 5 alone proves
                    # neither liveness nor exit. Require this exact handle to signal.
                    state = kernel.WaitForSingleObject(handle, 5000)
                    if state == 0:
                        return f'exited-after-termination-error-{error}'
                    raise OSError(error, f'Cannot terminate captured browser process PID {pid}; same-handle exit wait returned {state}')
                state = kernel.WaitForSingleObject(handle, 5000)
                if state == 0:
                    return 'terminated'
                if state == 258:
                    raise TimeoutError(f'Captured browser process PID {pid} did not exit after handle termination')
                raise ctypes.WinError(ctypes.get_last_error())
            return 'alive'
        raise ctypes.WinError(ctypes.get_last_error())
    finally:
        if not kernel.CloseHandle(handle):
            raise ctypes.WinError(ctypes.get_last_error())


def _windows_owned(child, profile, known, handle_observations=None):
    """Read only this private-profile lineage; creation stamps fence PID reuse."""
    environment=os.environ.copy()
    environment['BB_BROWSER_PROBE']=json.dumps({'pid':child.pid,'profile':str(profile),'known':known})
    script=r'''
$probe=$env:BB_BROWSER_PROBE | ConvertFrom-Json
$all=@(Get-CimInstance Win32_Process -ErrorAction Stop)
$owned=@{}
$parents=@{}
foreach($prior in $probe.known.PSObject.Properties){
  $current=@($all | Where-Object {$_.ProcessId.ToString() -eq $prior.Name})
  if($current.Count -gt 0 -and $current[0].CreationDate.ToUniversalTime().Ticks.ToString() -eq $prior.Value){$parents[$prior.Name]=$true}
}
foreach($item in $all){
  $stamp=$item.CreationDate.ToUniversalTime().Ticks.ToString()
  $key=$item.ProcessId.ToString()
  $prior=$probe.known.PSObject.Properties[$key]
  if(($item.ProcessId -eq $probe.pid -and $item.CommandLine -and $item.CommandLine.Contains($probe.profile)) -or ($prior -and $prior.Value -eq $stamp)){$owned[$key]=$stamp;$parents[$key]=$true}
}
do{
  $added=$false
  foreach($item in $all){
    $key=$item.ProcessId.ToString()
    if(!$owned.ContainsKey($key) -and $parents.ContainsKey($item.ParentProcessId.ToString())){$owned[$key]=$item.CreationDate.ToUniversalTime().Ticks.ToString();$parents[$key]=$true;$added=$true}
  }
}while($added)
ConvertTo-Json -InputObject $owned -Compress
'''
    result=subprocess.run(['powershell.exe','-NoProfile','-NonInteractive','-Command',script],
        env=environment,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,
        creationflags=subprocess.CREATE_NO_WINDOW,timeout=10)
    if result.returncode:
        raise RuntimeError('Owned-browser identity observation failed: '+result.stderr.strip())
    observed=json.loads(result.stdout)
    # CIM discovers the private lineage, but its cached rows do not prove liveness.
    # Probe captured identities independently through OS handles before targeting them.
    captured = {**known, **observed}
    if child.poll() is not None:
        captured.pop(str(child.pid),None)
    live = {}
    for pid, stamp in captured.items():
        state = _windows_process_state(pid, stamp)
        if state == 'alive':
            live[pid] = stamp
        elif handle_observations is not None:
            handle_observations.append({'pid':pid, 'creation_ticks':stamp, 'state':state})
    return live


def _stop_owned(child, profile, known):
    """Verify actual owned-process absence, retaining taskkill failure diagnostics."""
    if os.name == 'nt':
        diagnostics=[]
        handle_observations=[]
        def terminate(pids):
            arguments=['taskkill']
            for pid in pids:arguments.extend(['/PID',str(pid)])
            arguments.extend(['/T','/F'])
            try:
                result=subprocess.run(arguments,stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,text=True,creationflags=subprocess.CREATE_NO_WINDOW,timeout=10)
                if result.returncode:diagnostics.append({'pids':pids,'error':result.stderr.strip(),'code':result.returncode})
            except subprocess.TimeoutExpired:
                diagnostics.append({'pids':pids,'error':'taskkill deadline exceeded'})
        if child.poll() is None:
            try:known.update(_windows_owned(child,profile,known,handle_observations))
            except Exception as error:diagnostics.append({'error':'identity observation failed before own-root stop: '+repr(error)})
            terminate([child.pid])
            if child.poll() is None:child.kill()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError(f'Owned browser root PID {child.pid} remains alive') from error
        survivors=_windows_owned(child,profile,known,handle_observations)
        known.update(survivors)
        if survivors:
            # Fresh creation-stamp check before descendant stop prevents PID reuse.
            current=_windows_owned(child,profile,known,handle_observations)
            targets=[int(pid) for pid,stamp in survivors.items() if current.get(pid)==stamp]
            for pid in targets:
                state = _windows_process_state(pid, current[str(pid)], terminate=True)
                handle_observations.append({'pid':str(pid),'creation_ticks':current[str(pid)],'state':state})
        remaining=_windows_owned(child,profile,known,handle_observations)
        deadline=time.monotonic()+5
        while remaining and time.monotonic()<deadline:
            time.sleep(0.1)
            remaining=_windows_owned(child,profile,known,handle_observations)
        if remaining:
            raise RuntimeError('Owned browser descendants remain alive: '+json.dumps({'processes':remaining,'taskkill':diagnostics,'handle_observations':handle_observations}))
        return {'status':'verified-owned-processes-absent','observed_pids':sorted(known),'taskkill_diagnostics':diagnostics,'handle_observations':handle_observations}
    else:
        # The child starts a dedicated session; this process group belongs to it.
        if child.poll() is None:
            try:os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:pass
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f'Owned browser PID {child.pid} remains alive after cleanup') from error
    return 'forced-owned-tree'


@asynccontextmanager
async def owned_browser(playwright, executable, launch_timeout=60, close_timeout=5):
    executable = Path(executable).resolve(strict=True)
    if not executable.is_file() or not 0 < launch_timeout <= 120 or not 0 < close_timeout <= 15:
        raise ValueError('Supply an existing browser and bounded startup/close deadlines')
    profiles = ROOT / '.local/browser-runtime-profiles'
    profiles.mkdir(parents=True, exist_ok=True)
    profile = Path(tempfile.mkdtemp(prefix='workflow-', dir=profiles))
    receipt = {'executable':str(executable), 'profile':str(profile), 'status':'starting'}
    receipt_path = profile / 'owner.json'
    def record():
        receipt_path.write_text(json.dumps(receipt, indent=2), encoding='utf-8')
    browser = None
    child = None
    with (profile / 'browser.log').open('wb') as log:
        try:
            child = subprocess.Popen([str(executable), '--headless=new',
                '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0',
                '--user-data-dir='+str(profile), '--no-first-run', '--no-default-browser-check',
                '--disable-background-networking', 'about:blank'],
                stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0,
                start_new_session=os.name != 'nt')
            receipt['pid'] = child.pid
            record()
            deadline = time.monotonic() + launch_timeout
            endpoint_file = profile / 'DevToolsActivePort'
            while True:
                if child.poll() is not None:
                    raise RuntimeError(f'Owned browser exited before CDP startup: {child.returncode}; diagnostics {profile}')
                try:
                    lines = endpoint_file.read_text(encoding='utf-8').splitlines()
                    port = int(lines[0])
                    path = lines[1]
                    if not 0 < port <= 65535 or not path.startswith('/devtools/browser/'):
                        raise ValueError('Malformed owned CDP endpoint')
                    break
                except (OSError, ValueError, IndexError):
                    if time.monotonic() >= deadline:
                        raise TimeoutError(f'Owned browser did not publish CDP endpoint in {launch_timeout}s; diagnostics {profile}')
                    await asyncio.sleep(0.1)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('Owned browser startup deadline exhausted before CDP connection')
            endpoint = f'ws://127.0.0.1:{port}{path}'
            browser = await asyncio.wait_for(playwright.chromium.connect_over_cdp(endpoint,
                timeout=int(remaining * 1000)), timeout=remaining)
            receipt.update(status='connected', endpoint=endpoint)
            record()
            yield browser
        except BaseException as error:
            receipt.update(status='failed', error=repr(error))
            raise
        finally:
            close_error = None
            close_cancelled = False
            known={}
            if child is not None and os.name=='nt':
                try:
                    known, census_cancelled = await _owned_blocking(_windows_owned,child,profile,known)
                    close_cancelled |= census_cancelled
                    receipt['captured_owned_processes']=known.copy()
                except Exception as error:
                    receipt['identity_observation_error']=repr(error)
            if browser is not None:
                try:
                    session=await asyncio.wait_for(browser.new_browser_cdp_session(),timeout=close_timeout)
                    await asyncio.wait_for(session.send('Browser.close'),timeout=close_timeout)
                except BaseException as error:
                    receipt['server_close_error']=repr(error)
                    close_cancelled |= isinstance(error,asyncio.CancelledError)
                if child is not None:
                    deadline=time.monotonic()+close_timeout
                    try:
                        while child.poll() is None and time.monotonic()<deadline:
                            await asyncio.sleep(0.1)
                    except asyncio.CancelledError:
                        close_cancelled = True
                try:
                    await asyncio.wait_for(browser.close(), timeout=close_timeout)
                except BaseException as error:
                    close_error = repr(error)
                    close_cancelled |= isinstance(error, asyncio.CancelledError)
            try:
                if child is not None:
                    # Retain the cleanup thread through cancellation while driver I/O runs.
                    receipt['cleanup'], cleanup_cancelled = await _owned_blocking(_stop_owned,child,profile,known)
                    close_cancelled |= cleanup_cancelled
                    receipt['exit_code'] = child.poll()
                    if child.poll() is None:
                        raise RuntimeError(f'Owned browser PID {child.pid} still alive')
                if close_error:
                    receipt['graceful_close_error'] = close_error
                if close_cancelled:
                    receipt['status'] = 'failed'
                    raise asyncio.CancelledError()
                if receipt['status'] != 'failed':
                    receipt['status'] = 'closed'
            except BaseException as error:
                receipt.update(status='cleanup-failed', cleanup_error=repr(error))
                raise
            finally:
                record()
