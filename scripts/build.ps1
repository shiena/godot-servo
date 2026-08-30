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
    $cargoArgs = @('build')
    if ($Release) { $cargoArgs += '--release' }
    cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { Die "cargo build failed (exit $LASTEXITCODE)" }
}

# -------------------------------------------------------- Place (stage) ---
$target = Join-Path $repoRoot "target\$profileName"
$built = Join-Path $target 'godot_servo.dll'
if (-not (Test-Path $built)) { Die "Build artifact not found: $built" }

New-Item -ItemType Directory -Force $binDir | Out-Null
Copy-Item -Force $built (Join-Path $binDir 'godot_servo.x86_64.dll')

# ANGLE, picked out of the OUT_DIR mozangle put it in. Without it surfman's
# LoadLibrary fails and Servo never starts.
#
# A build.rs cannot reach it: cargo makes no promise about the order in which a
# crate's own build.rs and its dependencies' run, so mozangle's OUT_DIR may not
# exist yet. Looking after cargo build finishes depends on no such order.
# Changing a feature makes cargo rebuild mozangle under a different fingerprint,
# leaving several mozangle-* directories. Newest first, so the stale one is not
# picked up.
$angleDir = Get-ChildItem (Join-Path $target 'build') -Directory -Filter 'mozangle-*' -ErrorAction SilentlyContinue |
    ForEach-Object { Join-Path $_.FullName 'out' } |
    Where-Object { Test-Path (Join-Path $_ 'libEGL.dll') } |
    Sort-Object { (Get-Item (Join-Path $_ 'libEGL.dll')).LastWriteTime } -Descending |
    Select-Object -First 1
if (-not $angleDir) { Die "libEGL.dll not found under $targetuild\mozangle-*/out (did the build finish?)" }
foreach ($dll in 'libEGL.dll', 'libGLESv2.dll') {
    Copy-Item -Force (Join-Path $angleDir $dll) (Join-Path $binDir $dll)
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
