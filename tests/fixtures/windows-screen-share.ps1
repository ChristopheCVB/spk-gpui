param([string]$Title, [string]$Ready)
$ErrorActionPreference = 'Stop'

if ($Title) {
    Add-Type -AssemblyName System.Windows.Forms
    $form = [Windows.Forms.Form]::new()
    $form.Text = $Title
    $form.ClientSize = [Drawing.Size]::new(600, 400)
    $form.BackColor = [Drawing.Color]::SteelBlue

    $pcm = [IO.MemoryStream]::new()
    $wav = [IO.BinaryWriter]::new($pcm)
    $wav.Write([Text.Encoding]::ASCII.GetBytes('RIFF'))
    $wav.Write([int]48036)
    $wav.Write([Text.Encoding]::ASCII.GetBytes('WAVEfmt '))
    $wav.Write([int]16)
    $wav.Write([int16]1)
    $wav.Write([int16]1)
    $wav.Write([int]48000)
    $wav.Write([int]96000)
    $wav.Write([int16]2)
    $wav.Write([int16]16)
    $wav.Write([Text.Encoding]::ASCII.GetBytes('data'))
    $wav.Write([int]48000)
    for ($i = 0; $i -lt 24000; $i++) {
        $wav.Write([int16](1400 * [Math]::Sin(2 * [Math]::PI * 440 * $i / 48000)))
    }
    $pcm.Position = 0
    $player = [Media.SoundPlayer]::new($pcm)
    $timer = [Windows.Forms.Timer]::new()
    $timer.Interval = 50
    $timer.add_Tick({
        if ($form.BackColor -eq [Drawing.Color]::SteelBlue) {
            $form.BackColor = [Drawing.Color]::MediumPurple
        } else {
            $form.BackColor = [Drawing.Color]::SteelBlue
        }
    })
    $form.add_Shown({
        $player.PlayLooping()
        $timer.Start()
        [IO.File]::WriteAllText($Ready, 'ready')
    })
    [Windows.Forms.Application]::Run($form)
    $timer.Dispose()
    $player.Dispose()
    $wav.Dispose()
    $form.Dispose()
    exit
}

# Run from the repository root in an interactive Windows session.
$workspace = Join-Path ([IO.Path]::GetTempPath()) ("spk-screen-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $workspace | Out-Null
$env:SPK_TEST_SCREEN_WINDOW = [IO.Path]::GetFileName($workspace)
$readyFile = Join-Path $workspace 'ready'
try {
    $child = Start-Process powershell.exe -ArgumentList ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "{0}" -Title "{1}" -Ready "{2}"' -f $PSCommandPath, $env:SPK_TEST_SCREEN_WINDOW, $readyFile) -WindowStyle Hidden -PassThru
    for ($i = 0; $i -lt 80 -and !(Test-Path -LiteralPath $readyFile) -and !$child.HasExited; $i++) {
        Start-Sleep -Milliseconds 100
    }
    if (!(Test-Path -LiteralPath $readyFile)) { throw 'Synthetic Window failed to start.' }
    foreach ($mode in @('display', 'window')) {
        $env:SPK_TEST_WINDOWS_SCREEN = $mode
        Write-Host "Testing native Windows $mode sharing with a LiveKit peer."
        & python tests/fixtures/livekit-peers.py mise exec -- cargo test --locked --bin spk-gpui livekit_screen_share_publishes_video_and_separate_audio -- --ignored
        if ($LASTEXITCODE) { exit $LASTEXITCODE }
    }
    & mise exec -- cargo test --locked --bin spk-gpui controlled_window_video_audio_and_source_loss -- --ignored
    exit $LASTEXITCODE
} finally {
    if ($child -and !$child.HasExited) { Stop-Process -InputObject $child }
    Remove-Item -LiteralPath $workspace -Recurse -Force
    Remove-Item Env:SPK_TEST_SCREEN_WINDOW
    Remove-Item Env:SPK_TEST_WINDOWS_SCREEN -ErrorAction SilentlyContinue
}
