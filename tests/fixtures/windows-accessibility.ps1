param([int]$ProbePid, [string]$Executable, [string]$Sound)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

function Get-Nodes([IntPtr]$WindowHandle) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($WindowHandle)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition)
}

if ($ProbePid) {
    $windowHandle = (Get-Process -Id $ProbePid).MainWindowHandle
    if ($windowHandle -eq [IntPtr]::Zero) { throw "Process $ProbePid has no window." }
    Write-Host "Accessibility nodes returned: $(@(Get-Nodes $windowHandle).Count)"
    return
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class SpkAccessibilityFixture {
    [DllImport("winmm.dll")] public static extern uint waveOutGetNumDevs();
    delegate bool EnumCallback(IntPtr hwnd, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out int pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int size);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr SendMessage(IntPtr hwnd, uint message, IntPtr wParam, string text);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);
    [StructLayout(LayoutKind.Sequential)] struct Point { public int X, Y; }
    [DllImport("user32.dll")] static extern bool ScreenToClient(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);

    static string ClassName(IntPtr hwnd) {
        var name = new StringBuilder(64);
        GetClassName(hwnd, name, name.Capacity);
        return name.ToString();
    }

    public static bool ChooseFile(int pid, string path) {
        IntPtr dialog = IntPtr.Zero;
        EnumWindows((hwnd, _) => {
            int owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != pid || ClassName(hwnd) != "#32770" || !IsWindowVisible(hwnd)) return true;
            dialog = hwnd;
            return false;
        }, IntPtr.Zero);
        if (dialog == IntPtr.Zero) return false;

        IntPtr filename = IntPtr.Zero, open = IntPtr.Zero;
        EnumChildWindows(dialog, (hwnd, _) => {
            int id = GetDlgCtrlID(hwnd);
            if (id == 1148 && ClassName(hwnd) == "Edit") filename = hwnd;
            if (id == 1 && ClassName(hwnd) == "Button") open = hwnd;
            return true;
        }, IntPtr.Zero);
        if (filename == IntPtr.Zero || open == IntPtr.Zero) return false;
        SendMessage(filename, 0x000C, IntPtr.Zero, path);
        return PostMessage(open, 0x00F5, IntPtr.Zero, IntPtr.Zero);
    }

    public static void Click(IntPtr hwnd, int x, int y) {
        var point = new Point { X = x, Y = y };
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        bool converted = ScreenToClient(hwnd, ref point);
        if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous);
        if (!converted) throw new InvalidOperationException("Window closed before click.");
        var position = new IntPtr((point.Y << 16) | (point.X & 65535));
        PostMessage(hwnd, 0x0200, IntPtr.Zero, position);
        PostMessage(hwnd, 0x0201, new IntPtr(1), position);
        PostMessage(hwnd, 0x0202, IntPtr.Zero, position);
    }

    public static void Type(IntPtr hwnd, string text) {
        foreach (char key in text) PostMessage(hwnd, 0x0102, new IntPtr(key), new IntPtr(1));
    }
}
'@

$run = Join-Path ([IO.Path]::GetTempPath()) ("spk-a11y-run-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $run | Out-Null
$stderr = Join-Path $run 'stderr.log'
$env:RUST_BACKTRACE = '1'
$env:SPK_TEST_ACCESSIBILITY = '1'
$launch = @{
    FilePath = $Executable
    WorkingDirectory = $run
    PassThru = $true
    RedirectStandardOutput = Join-Path $run 'stdout.log'
    RedirectStandardError = $stderr
}
$client = Start-Process @launch

function Wait-For([string]$Description, [scriptblock]$Condition) {
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        $client.Refresh()
        if ($client.HasExited) { throw "SPK exited while waiting for $Description. See $stderr." }
        try {
            $result = & $Condition
            if ($result) { return $result }
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            # The accessibility provider can be replaced during a redraw.
        }
        Start-Sleep -Milliseconds 100
    }
    throw "Timed out waiting for $Description. See $stderr."
}

function Get-Control([string]$Name, $Type) {
    Wait-For $Name {
        Get-Nodes $windowHandle | Where-Object {
            $_.Current.Name -eq $Name -and $_.Current.ControlType -eq $Type -and $_.Current.IsEnabled
        } | Select-Object -First 1
    }
}

function Invoke-Button([string]$Name) {
    $button = Get-Control $Name ([System.Windows.Automation.ControlType]::Button)
    $invoke = $null
    if ($button.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$invoke)) {
        $invoke.Invoke()
    } else {
        Click-Control $button
    }
}

function Click-Control($Control) {
    $bounds = $Control.Current.BoundingRectangle
    [SpkAccessibilityFixture]::Click($windowHandle, ($bounds.X + $bounds.Width / 2),
        ($bounds.Y + $bounds.Height / 2))
}

function Set-Input([string]$Name, [string]$Value) {
    Click-Control (Get-Control $Name ([System.Windows.Automation.ControlType]::Edit))
    Start-Sleep -Milliseconds 120
    [SpkAccessibilityFixture]::Type($windowHandle, $Value)
}

function Assert-Rows([string[]]$Expected) {
    $null = Wait-For ($Expected -join ', ') {
        $names = @(Get-Nodes $windowHandle | Where-Object {
            $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Text
        } | ForEach-Object { $_.Current.Name })
        @($Expected | Where-Object { $_ -notin $names }).Count -eq 0
    }
}

try {
    $windowHandle = Wait-For 'the SPK window' { $client.MainWindowHandle.ToInt64() }
    Set-Input 'Email' 'alice@example.test'
    Set-Input 'Password' 'fixture'
    Invoke-Button 'Sign in'
    Invoke-Button 'Settings'
    Invoke-Button 'Shortcuts'
    Assert-Rows @('Navigation', 'Messages', 'Search', 'Formatting', 'Voice and video',
        'Quick Switcher', 'Previous or next channel', 'Previous or next unread channel',
        'Show or hide sidebar', 'Show keyboard shortcuts',
        'Edit your last message (empty composer)', 'Reply to focused message',
        'Cancel edit or reply, or mark as read', 'Send message', 'New line',
        'Open message search', 'Search everything', 'Select a search result',
        'Search or open selected result', 'Close search',
        'Bold', 'Italic', 'Strikethrough', 'Inline code', 'Code block', 'Quote',
        'Camera on or off', 'Start or stop Screen Share', 'Cycle noise suppression',
        'Echo cancellation on or off', 'Auto gain on or off',
        'Theater mode (focused or hovered tile)', 'Fullscreen (focused or hovered tile)',
        'Watch or stop watching (focused or hovered tile)')
    Write-Host 'Shortcuts accessibility passed.'

    Invoke-Button 'Changelog'
    $null = Get-Control 'Desktop release notes' ([System.Windows.Automation.ControlType]::Group)
    Write-Host 'Changelog accessibility passed.'

    Invoke-Button 'Soundboard'
    Invoke-Button 'Choose audio file'
    $null = Wait-For 'the audio picker' { [SpkAccessibilityFixture]::ChooseFile($client.Id, $Sound) }
    Assert-Rows @('Start (seconds)', 'End (seconds)', 'Upload gain (%)')
    Write-Host 'Soundboard upload accessibility passed.'

    Invoke-Button 'Close settings'
    $null = Wait-For 'Settings to close' {
        -not (Get-Nodes $windowHandle | Where-Object { $_.Current.Name -eq 'Close settings' } |
            Select-Object -First 1)
    }
    if ([SpkAccessibilityFixture]::waveOutGetNumDevs() -gt 0) {
        Invoke-Button 'Join voice'
        Invoke-Button 'Voice connection diagnostics'
        Assert-Rows @('Connection quality', 'Media round-trip', 'Audio jitter (worst track)',
            'Incoming audio packet loss', 'Audio upload', 'Audio download')
        Write-Host 'Voice connection diagnostics accessibility passed.'
    } else {
        Write-Host '::warning::Voice connection diagnostics requires a Windows playback endpoint. The other accessibility checks passed.'
    }

    Invoke-Button 'Settings'
    Invoke-Button 'Sign out'
    $null = Get-Control 'Email' ([System.Windows.Automation.ControlType]::Edit)
} catch {
    if (Test-Path -LiteralPath $stderr) { Get-Content -LiteralPath $stderr }
    throw
} finally {
    $client.Refresh()
    if (!$client.HasExited -and (!$client.CloseMainWindow() -or !$client.WaitForExit(3000))) {
        Stop-Process -InputObject $client
    }
}
