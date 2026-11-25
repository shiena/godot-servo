# The local development pipeline for godot-servo (Windows / PowerShell).
#
# Builds with cargo, places the result where godot_servo.gdextension points, and
# runs the demo when asked.
#
# Usage:
#   .\scripts\build.ps1                        # build and stage (debug)
#   .\scripts\build.ps1 -Release               # the release profile
#   .\scripts\build.ps1 -Run                   # run the demo against what is staged
#   .\scripts\build.ps1 -Run -Scene flat       # the 2D scene, for checking things
#   .\scripts\build.ps1 -Test                  # input and signal self check
#   .\scripts\build.ps1 -Run -Page webgl       # open a different page
#   .\scripts\build.ps1 -Checks                # also run fmt and clippy
#
# With no stage given, it only builds.

param(
    [switch]$Release,
    [switch]$Run,
    [switch]$Test,
    [switch]$Checks,
    [switch]$NoBuild,
    [string]$Scene = 'main',
    [string]$Page,
    [string]$Screenshot,
    [int]$QuitAfter = 0,
    [int]$TimeoutSeconds = 0,
    [string]$Godot = $(if ($env:GODOT) { $env:GODOT } else { 'godot' })
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

function Say ([string]$m) { Write-Host ">> $m" -ForegroundColor Cyan }
function Ok  ([string]$m) { Write-Host $m -ForegroundColor Green }
function Die ([string]$m) { Write-Error $m }

$profileName = if ($Release) { 'release' } else { 'debug' }
$binDir = Join-Path $repoRoot 'addons\godot_servo\bin\windows'

# The SDK is needed by the Rust bindings at link time.
if (-not $NoBuild -or $Checks) {
    $gstRoot = (python "$PSScriptRoot/gstreamer_sdk.py" windows | Select-Object -Last 1)
    if ($LASTEXITCODE -ne 0 -or -not $gstRoot) { Die 'GStreamer SDK setup failed' }
    $env:PATH = "$gstRoot/bin;$env:PATH"
    $env:PKG_CONFIG_PATH = "$gstRoot/lib/pkgconfig;$env:PKG_CONFIG_PATH"
}

# ------------------------------------------------------------------ Build ---
if ($Checks) {
        Say 'cargo fmt --check'
        cargo fmt --check; if ($LASTEXITCODE -ne 0) { Die 'cargo fmt --check failed (run `cargo fmt`)' }
        Say 'cargo clippy'
        $clippyArgs = @('clippy', '--all-targets')
        if ($Release) { $clippyArgs += '--release' }
    cargo @clippyArgs; if ($LASTEXITCODE -ne 0) { Die 'cargo clippy failed' }
}

if (-not $NoBuild) {
    Say "cargo build ($profileName)"
    $cargoArgs = @('build', '--message-format=json-render-diagnostics')
    if ($Release) { $cargoArgs += '--release' }
    cargo @cargoArgs | ForEach-Object {
        $message = $_ | ConvertFrom-Json
        if ($message.reason -eq 'build-script-executed' -and $message.package_id -match '#mozangle@') {
            $angleDir = $message.out_dir
        }
    }
    if ($LASTEXITCODE -ne 0) { Die "cargo build failed (exit $LASTEXITCODE)" }
}

# -------------------------------------------------------- Place (stage) ---
$buildRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repoRoot "target" }
$target = Join-Path $buildRoot $profileName
$built = Join-Path $target 'godot_servo.dll'
if (-not (Test-Path $built)) { Die "Build artifact not found: $built" }

New-Item -ItemType Directory -Force $binDir | Out-Null
Copy-Item -Force $built (Join-Path $binDir 'godot_servo.x86_64.dll')

# Cargo reports the active build script output even when the crate is cached.
foreach ($dll in 'libEGL.dll', 'libGLESv2.dll') {
    $destination = Join-Path $binDir $dll
    if (-not $NoBuild) {
        if (-not $angleDir) { Die 'Cargo did not report the mozangle build output directory' }
        Copy-Item -LiteralPath (Join-Path $angleDir $dll) -Destination $destination -Force
    } elseif (-not (Test-Path -LiteralPath $destination)) {
        Die "Missing staged $dll. Run the build once before using -NoBuild."
    }
}

if ($gstRoot) {
    python "$PSScriptRoot/stage_gstreamer.py" windows "$gstRoot" "$binDir"
    if ($LASTEXITCODE -ne 0) { Die 'Could not stage GStreamer' }
    python "$PSScriptRoot/stage_gstreamer.py" manifest godot_servo.gdextension addons/godot_servo/bin
    if ($LASTEXITCODE -ne 0) { Die 'Could not generate export dependencies' }
}
Ok "Placed: $binDir"

# -------------------------------------------------------------------- Run ---
if ($Run -or $Test) {
    $sceneName = if ($Test) { 'autotest' } else { $Scene }
    $scenePath = "res://demo/$sceneName.tscn"
    if (-not (Test-Path (Join-Path $repoRoot "demo\$sceneName.tscn"))) {
        Die "Scene not found: demo/$sceneName.tscn"
    }

    $godotArgs = @('--path', $repoRoot, $scenePath)
    if ($QuitAfter -gt 0) { $godotArgs += @('--quit-after', "$QuitAfter") }

    $userArgs = @()
    if ($Page) { $userArgs += @('--page', $Page) }
    if ($Screenshot) { $userArgs += @('--screenshot', $Screenshot) }
    if ($userArgs.Count -gt 0) { $godotArgs += @('--') + $userArgs }

    Say "$Godot $($godotArgs -join ' ')"

    # Start-Process rather than `& $Godot`. The non-console Godot build
    # (Godot_v*_win64.exe, which is what setup-godot installs in CI) detaches
    # itself from the console, so `&` returns without waiting and yields neither
    # the output nor $LASTEXITCODE. The output goes to a file and is replayed.
    # New-TemporaryFile is missing from some Windows PowerShell 5.1 installs; the
    # .NET API behaves the same on 5.1 and 7.
    $stdout = [System.IO.Path]::GetTempFileName()
    $stderr = [System.IO.Path]::GetTempFileName()

    # The self check is supposed to quit by itself. When it stalls, do not wait
    # forever: show whatever it printed and fail. In CI a step's output is flushed
    # only when the step ends, so waiting leaves no trace of what happened.
    $limit = if ($TimeoutSeconds -gt 0) { $TimeoutSeconds } elseif ($Test) { 300 } else { 0 }

    try {
        $process = Start-Process -FilePath $Godot -PassThru -NoNewWindow `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr `
            -ArgumentList ($godotArgs | ForEach-Object { '"{0}"' -f $_ })

        $exited = $true
        if ($limit -gt 0) {
            $exited = $process.WaitForExit($limit * 1000)
            if (-not $exited) {
                $process.Kill()
                $process.WaitForExit()
            }
        } else {
            $process.WaitForExit()
        }

        # Without saying so, 5.1 reads it as ANSI and mangles non-ASCII output.
        Get-Content $stdout -Encoding UTF8 -ErrorAction SilentlyContinue | Write-Host
        Get-Content $stderr -Encoding UTF8 -ErrorAction SilentlyContinue | Write-Host
        if (-not $exited) { Die "Godot did not exit within $limit s" }
        if ($process.ExitCode -ne 0) { Die "Godot exited with $($process.ExitCode)" }
    } finally {
        Remove-Item $stdout, $stderr -Force -ErrorAction SilentlyContinue
    }
}
