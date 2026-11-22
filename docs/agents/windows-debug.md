# Windows client debugging

Build and launch the Windows executable in the interactive Windows session so the reproduction uses native devices and an actual GPUI window. Use the deployment endpoints and Windows prerequisites from the [build guide](../../README.md#build-and-sign-in).

## Build the debug executable

Build from a short checkout path in an x64 Developer PowerShell.

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'spk-w'
mise exec -- cargo build --locked
```

### From a checkout in WSL

Stage the current working files onto the Windows filesystem. This includes uncommitted edits and unignored new files. Copy only the two deployment URLs into the Windows copy of `.env`. A WSL-specific `CC=clang-21` or `CXX=clang++-21` in the Windows build environment makes native dependencies fail to compile.

Run the staging command from the repository root in WSL. It uses `rsync` and Windows interop.

```sh
spk_win_temp=$(powershell.exe -NoProfile -Command '$env:TEMP' | tr -d '\r')
spk_work=$(mktemp -d "$(wslpath -u "$spk_win_temp")/spk-src.XXXXXX")
git ls-files -z --cached --others --exclude-standard |
  rsync -a --from0 --files-from=- --ignore-missing-args ./ "$spk_work/"
awk '/^[[:space:]]*(SPK_URL|LIVEKIT_URL)[[:space:]]*=/' .env >"$spk_work/.env"
wslpath -w "$spk_work"
```

Open the printed directory in an x64 Developer PowerShell and run the build above. For an unattended build from WSL, save this as `build-native.cmd` in the staged directory. Adjust the Visual Studio path if the C++ workload is installed in a different edition.

```bat
@echo off
cd /d "%~dp0"
call "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -no_logo -arch=x64 -host_arch=x64
if errorlevel 1 exit /b %errorlevel%
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "CARGO_TARGET_DIR=%TEMP%\spk-w"
set "CC="
set "CXX="
set "RUSTUP_HOME="
set "RUSTUP_TOOLCHAIN="
set "CARGO_HOME=%USERPROFILE%\.cargo"
mise trust -y && mise exec -- cargo build --locked
```

```sh
(cd "$spk_work" && cmd.exe /d /c build-native.cmd) >"$spk_work/build.log" 2>&1
```

Check for a successful exit and the executable at `%TEMP%\spk-w\debug\spk-gpui.exe` before launching.

## Launch and capture logs

The Windows app uses the GUI subsystem. Redirect stdout and stderr at process creation. Copy the executable into a separate run directory so subsequent builds can replace the Cargo output while the app stays open.

```powershell
$exe = Join-Path $env:TEMP 'spk-w\debug\spk-gpui.exe'
$run = Join-Path $env:TEMP ('spk-debug-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $run -ErrorAction Stop | Out-Null
Copy-Item -LiteralPath $exe -Destination $run -ErrorAction Stop
$env:RUST_BACKTRACE = '1'
$client = Start-Process -FilePath (Join-Path $run 'spk-gpui.exe') `
    -WorkingDirectory $run -PassThru `
    -RedirectStandardOutput (Join-Path $run 'stdout.log') `
    -RedirectStandardError (Join-Path $run 'stderr.log')
$run
$client.Id
```

Run this interactively in PowerShell or save it as `launch.ps1` in the staged directory and invoke it from WSL.

```sh
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w "$spk_work/launch.ps1")"
```

Confirm the PID has a visible SPK window. Follow the stderr log with `Get-Content (Join-Path $run 'stderr.log') -Wait` in PowerShell, or convert the printed run directory with `wslpath -u` and use `tail -F` from WSL. Keep each run's logs in its own directory for before-and-after comparison.

## Reproduce and collect evidence

- For a crash, capture the first panic line and relevant backtrace frames. Check the exact action again on the rebuilt client.
- For audio cuts, ask for the Windows clock time and which listener or route heard each cut. Test microphone and Voice Output with a web participant, then test Channel Radio or the reported media source separately. Open **Voice connection diagnostics** in the call to check jitter and incoming packet loss.
- The app has no built-in audio log sink. Empty files are normal in a healthy stock run. For timing bugs, use a temporary debug build that emits tagged, timestamped counters to stderr. In the audio-cut investigation, callback gaps, dropped blocks and radio refill gaps distinguished a stalled session runtime from packet loss. Keep logging I/O off audio callbacks. Redact credentials and session data before sharing logs.
