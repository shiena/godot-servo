# godot-servo のローカル開発パイプライン (Windows / PowerShell)。
#
# cargo でビルドし、成果物を godot_servo.gdextension が指す場所へ配り、必要なら
# デモを起動する。
#
# 使い方:
#   .\scripts\build.ps1                        # ビルドして配置 (debug)
#   .\scripts\build.ps1 -Release               # release プロファイル
#   .\scripts\build.ps1 -Run                   # 配置済みのものでデモを起動
#   .\scripts\build.ps1 -Run -Scene flat       # 2D の確認用シーン
#   .\scripts\build.ps1 -Test                  # 入力とシグナルのセルフチェック
#   .\scripts\build.ps1 -Run -Page webgl       # 別のページを開く
#   .\scripts\build.ps1 -Checks                # fmt + clippy も回す
#
# ステージを何も指定しなければビルドのみ。

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

# ANGLE。mozangle が自分の OUT_DIR に置いたものを拾う。これが無いと surfman の
# LoadLibrary が失敗して Servo が起動しない。
#
# 以前は build.rs でコピーしていたが、cargo は自クレートの build.rs と依存クレートの
# build.rs の実行順を保証しない。ローカルでは mozangle が先にビルド済みだったので
# 通っていただけで、CI のクリーンビルドでは DLL がまだ存在しなかった。
# cargo build の完了後に探せば順序の問題は起きない。
# feature を変えると cargo は別の fingerprint で mozangle を作り直すので、
# mozangle-* が複数残る。古いものを掴まないよう新しい順に見る。
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
    & $Godot @godotArgs
    if ($LASTEXITCODE -ne 0) { Die "Godot exited with $LASTEXITCODE" }
}
