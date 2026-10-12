param(
    [Parameter(Mandatory=$true)][Int64]$Hwnd,
    [Parameter(Mandatory=$true)][UInt32]$ExpectedPid,
    [Parameter(Mandatory=$true)][string]$Nonce,
    [Parameter(Mandatory=$true)][string]$Output,
    [Parameter(Mandatory=$true)][UInt64]$TargetView,
    [Parameter(Mandatory=$true)][string]$TargetLabel
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class KumihimoOwnedWindowObservation {
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@
$target = [IntPtr]$Hwnd
function Read-OwnedObservation {
    [UInt32]$owner = 0
    [void][KumihimoOwnedWindowObservation]::GetWindowThreadProcessId($target,[ref]$owner)
    $foreground = [KumihimoOwnedWindowObservation]::GetForegroundWindow()
    [UInt32]$foregroundOwner = 0
    [void][KumihimoOwnedWindowObservation]::GetWindowThreadProcessId($foreground,[ref]$foregroundOwner)
    return [ordered]@{ hwnd=$Hwnd; exists=[KumihimoOwnedWindowObservation]::IsWindow($target); visible=[KumihimoOwnedWindowObservation]::IsWindowVisible($target); pid=$owner; owned_pid=($owner -eq $ExpectedPid); foreground=$foreground.ToInt64(); foreground_pid=$foregroundOwner; foreground_owned=($foregroundOwner -eq $ExpectedPid) }
}
$before = Read-OwnedObservation
$records = [System.Collections.Generic.List[object]]::new()
$invoked = $null
$actionBefore = $null
$actionAfter = $null
$actionPattern = $null
$failure = $null
$tracePath = Join-Path ([System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($Output))) 'native-uia.trace.json'
$checkpoints = [System.Collections.Generic.List[object]]::new()
$targetDetails = $null
function Save-Checkpoint([string]$Phase, [object]$Details = $null, [string]$ExceptionMessage = $null) {
    # One observer writer; persist each bounded checkpoint before crossing the action boundary.
    if ($checkpoints.Count -ge 64) { $checkpoints.RemoveAt(0) }
    $checkpoints.Add([ordered]@{ phase=$Phase; timestamp_utc=[DateTime]::UtcNow.ToString('o'); timestamp_unix_millis=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds().ToString(); action_pattern=$actionPattern; target=$targetDetails; details=$Details; exception=$ExceptionMessage; owned_window=(Read-OwnedObservation) })
    $trace = [ordered]@{ schema='kumihimo.uia-action-trace.v1'; writer='powershell-uia-observer'; nonce=$Nonce; expected_owned_pid=$ExpectedPid; hwnd=$Hwnd; target_view=$TargetView; target_control_id="view/$TargetView/tab"; target_label=$TargetLabel; checkpoints=@($checkpoints.ToArray()) }
    $traceBytes = [System.Text.UTF8Encoding]::new($false).GetBytes(($trace | ConvertTo-Json -Depth 16))
    if ($traceBytes.Length -gt 1048576) { throw 'UIA trace byte budget exceeded' }
    # Keep the last complete trace intact if owned helper cleanup interrupts this rewrite.
    $traceTempPath = "$tracePath.tmp"
    $traceStream = [System.IO.FileStream]::new($traceTempPath,[System.IO.FileMode]::Create,[System.IO.FileAccess]::Write,[System.IO.FileShare]::None)
    try { $traceStream.Write($traceBytes,0,$traceBytes.Length); $traceStream.Flush($true) } finally { $traceStream.Dispose() }
    if ([System.IO.File]::Exists($tracePath)) { [System.IO.File]::Replace($traceTempPath,$tracePath,$null) }
    else { [System.IO.File]::Move($traceTempPath,$tracePath) }
}
function Read-Patterns($node) {
    $selection = $null
    $toggle = $null
    $range = $null
    $selected = $null
    $value = $null
    if ($node.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern,[ref]$selection)) { $selected=$selection.Current.IsSelected }
    if ($node.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern,[ref]$toggle)) { $selected=($toggle.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On); $value=$toggle.Current.ToggleState.ToString() }
    if ($node.TryGetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern,[ref]$range)) { $value=$range.Current.Value }
    return [ordered]@{ selected=$selected; value=$value; actions=@($node.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName }) }
}
try {
    Save-Checkpoint 'observer-start'
    if (-not $before.exists -or -not $before.owned_pid -or $before.visible -or $before.foreground_owned) { throw 'Owned hidden HWND admission failed' }
    Save-Checkpoint 'tree-request-before'
    $element = [System.Windows.Automation.AutomationElement]::FromHandle($target)
    Save-Checkpoint 'tree-request-after'
    if ($element.Current.ProcessId -ne $ExpectedPid) { throw 'UIA target PID mismatch' }
    # Request the owned AccessKit tree while the native event loop pumps. No desktop/global query.
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    for ($attempt=0; $attempt -lt 25 -and $null -eq $invoked; $attempt++) {
        $records.Clear()
        $queue = [System.Collections.Generic.Queue[object]]::new()
        $queue.Enqueue(@($element,0))
        while ($queue.Count -gt 0 -and $records.Count -lt 128) {
            $entry = $queue.Dequeue()
            $node = $entry[0]
            $depth = [int]$entry[1]
            $current = $node.Current
            $patterns=Read-Patterns $node
            # Text-editor values are deliberately never queried; names refer only to demo controls.
            $records.Add([ordered]@{ name=$current.Name; automation_id=$current.AutomationId; role=$current.ControlType.ProgrammaticName; enabled=$current.IsEnabled; offscreen=$current.IsOffscreen; focusable=$current.IsKeyboardFocusable; focused=$current.HasKeyboardFocus; depth=$depth; selected=$patterns.selected; value=$patterns.value; actions=$patterns.actions })
            if ($null -eq $invoked -and $current.IsEnabled -and $current.Name -eq $TargetLabel) {
                $pattern = $null
                $actionBefore=$patterns
                if ($node.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern,[ref]$pattern)) {
                    $actionPattern='SelectionItem.Select'
                } elseif ($node.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) {
                    $actionPattern='Invoke.Invoke'
                } elseif ($node.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern,[ref]$pattern)) {
                    $actionPattern='Toggle.Toggle'
                } else { throw 'Exact target exposes no supported select/invoke/toggle action' }
                $targetDetails=[ordered]@{ name=$current.Name; automation_id=$current.AutomationId; runtime_id=@($node.GetRuntimeId()); role=$current.ControlType.ProgrammaticName; process_id=$current.ProcessId; target_view=$TargetView; pattern_type=$pattern.GetType().FullName }
                Save-Checkpoint 'action-call-before' $actionBefore
                try {
                    switch ($actionPattern) {
                        'SelectionItem.Select' { ([System.Windows.Automation.SelectionItemPattern]$pattern).Select() }
                        'Invoke.Invoke' { ([System.Windows.Automation.InvokePattern]$pattern).Invoke() }
                        'Toggle.Toggle' { ([System.Windows.Automation.TogglePattern]$pattern).Toggle() }
                    }
                    Save-Checkpoint 'action-call-after'
                } catch {
                    Save-Checkpoint 'action-call-exception' $null $_.Exception.Message
                    throw
                }
                $invoked=$current.Name
                for ($settle=0; $settle -lt 20; $settle++) {
                    $actionAfter=Read-Patterns $node
                    Save-Checkpoint 'action-settle' ([ordered]@{ attempt=$settle; selected=$actionAfter.selected; value=$actionAfter.value; actions=$actionAfter.actions })
                    if ($actionAfter.selected -eq $true) { break }
                    Start-Sleep -Milliseconds 80
                }
                Save-Checkpoint 'action-settled' $actionAfter
            }
            if ($depth -lt 8) {
                $child = $walker.GetFirstChild($node)
                while ($null -ne $child -and $queue.Count -lt 128) { $queue.Enqueue(@($child,($depth+1))); $child=$walker.GetNextSibling($child) }
            }
        }
        if ($null -eq $invoked) { Start-Sleep -Milliseconds 100 }
    }
    if ($records.Count -lt 2) { throw 'Owned UIA tree did not expose controls before bounded deadline' }
    if ($null -eq $invoked) { throw 'Exact demo tab target was not found in owned UIA tree' }
} catch { $failure = $_.Exception.Message; Save-Checkpoint 'observer-exception' $null $failure }
$after = Read-OwnedObservation
if ($after.visible -or $after.foreground_owned -or -not $after.owned_pid) { $failure = 'Quiet ownership condition changed during UIA observation' }
$result = [ordered]@{ schema='kumihimo.uia-observation.v1'; hwnd=$Hwnd; pid=$ExpectedPid; nonce=$Nonce; before=$before; after=$after; controls=@($records.ToArray()); invoked=$invoked; target_view=$TargetView; target_control_id="view/$TargetView/tab"; action_pattern=$actionPattern; action_before=$actionBefore; action_after=$actionAfter; error=$failure; scope='Owned hidden HWND exact tab query and supported action only; no global input/focus' }
$json = $result | ConvertTo-Json -Depth 12
[System.IO.File]::WriteAllText($Output,$json,[System.Text.UTF8Encoding]::new($false))
if ($null -ne $failure) { Write-Error $failure; exit 1 }
