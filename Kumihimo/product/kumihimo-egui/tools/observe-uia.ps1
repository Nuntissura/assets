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
    if (-not $before.exists -or -not $before.owned_pid -or $before.visible -or $before.foreground_owned) { throw 'Owned hidden HWND admission failed' }
    $element = [System.Windows.Automation.AutomationElement]::FromHandle($target)
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
                    ([System.Windows.Automation.SelectionItemPattern]$pattern).Select()
                    $actionPattern='SelectionItem.Select'
                } elseif ($node.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) {
                    ([System.Windows.Automation.InvokePattern]$pattern).Invoke()
                    $actionPattern='Invoke.Invoke'
                } elseif ($node.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern,[ref]$pattern)) {
                    ([System.Windows.Automation.TogglePattern]$pattern).Toggle()
                    $actionPattern='Toggle.Toggle'
                } else { throw 'Exact target exposes no supported select/invoke/toggle action' }
                $invoked=$current.Name
                for ($settle=0; $settle -lt 20; $settle++) {
                    $actionAfter=Read-Patterns $node
                    if ($actionAfter.selected -eq $true) { break }
                    Start-Sleep -Milliseconds 80
                }
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
} catch { $failure = $_.Exception.Message }
$after = Read-OwnedObservation
if ($after.visible -or $after.foreground_owned -or -not $after.owned_pid) { $failure = 'Quiet ownership condition changed during UIA observation' }
$result = [ordered]@{ schema='kumihimo.uia-observation.v1'; hwnd=$Hwnd; pid=$ExpectedPid; nonce=$Nonce; before=$before; after=$after; controls=@($records.ToArray()); invoked=$invoked; target_view=$TargetView; target_control_id="view/$TargetView/tab"; action_pattern=$actionPattern; action_before=$actionBefore; action_after=$actionAfter; error=$failure; scope='Owned hidden HWND exact tab query and supported action only; no global input/focus' }
$json = $result | ConvertTo-Json -Depth 12
[System.IO.File]::WriteAllText($Output,$json,[System.Text.UTF8Encoding]::new($false))
if ($null -ne $failure) { Write-Error $failure; exit 1 }
