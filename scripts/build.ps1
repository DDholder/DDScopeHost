<#
.SYNOPSIS
    DDScopeHost 编译构建与验证脚本（支持交互式菜单与命令行参数两种模式）。

.DESCRIPTION
    提供前端、Rust 核心引擎、CLI 工具、独立桌面端（自动启用 custom-protocol 内嵌前端）、
    Windows 安装包（MSI / NSIS）、全量测试门禁及混合信号压测等按需构建选项。

.EXAMPLE
    .\scripts\build.ps1
    打开交互式构建菜单按编号选择。

.EXAMPLE
    .\scripts\build.ps1 -Target desktop -SmokeTest
    构建带内嵌前端的 Release 桌面可执行文件并执行启动冒烟测试。

.EXAMPLE
    .\scripts\build.ps1 -Target bundle -BundleTypes msi
    构建前端并仅打包 Windows MSI 安装包。

.EXAMPLE
    .\scripts\build.ps1 -Target cli -Profile release
    仅编译 Release 版 ddscope-cli.exe。

.EXAMPLE
    .\scripts\build.ps1 -Target verify
    运行全量静态检查、类型检查与单元测试/集成测试门禁。
#>
[CmdletBinding()]
param(
    [ValidateSet(
        'menu',
        'dev',
        'dev-ui',
        'ui',
        'desktop',
        'bundle',
        'cli',
        'core',
        'workspace',
        'verify',
        'soak',
        'clean'
    )]
    [string]$Target = 'menu',

    [ValidateSet('debug', 'release')]
    [string]$Profile = 'release',

    [ValidateSet('all', 'msi', 'nsis')]
    [string]$BundleTypes = 'all',

    [int]$SoakSecs = 30,

    [switch]$SmokeTest,

    [switch]$ForceInstall,

    [switch]$List
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

function Write-Step {
    param([string]$Message)
    Write-Host "`n==> $Message" -ForegroundColor Cyan
}

function Write-Ok {
    param([string]$Message)
    Write-Host "[OK] $Message" -ForegroundColor Green
}

function Invoke-Native {
    param(
        [string]$FilePath,
        [string[]]$Arguments
    )
    Write-Host "  > $FilePath $($Arguments -join ' ')" -ForegroundColor DarkGray
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $FilePath $($Arguments -join ' ')"
    }
}

function Ensure-Command {
    param([string]$Name)
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required tool '$Name' was not found in PATH."
    }
}

function Ensure-NodeDeps {
    if ($ForceInstall -or -not (Test-Path (Join-Path $RepoRoot 'node_modules'))) {
        Ensure-Command 'pnpm'
        Write-Step 'Installing Node workspace dependencies (pnpm install)'
        Invoke-Native 'pnpm' @('install')
    }
}

function Build-WebUi {
    Ensure-Command 'pnpm'
    Ensure-NodeDeps
    Write-Step 'Building @ddscope/web-ui production assets (apps/web-ui/dist)'
    Invoke-Native 'pnpm' @('build:ui')
}

function Assert-ProcessNotRunning {
    param([string]$ProcessName)
    $running = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue
    if ($running) {
        throw "检测到 $ProcessName.exe 正在运行（PID: $(($running | Select-Object -ExpandProperty Id) -join ', ')），Windows 下无法覆盖正在运行的程序，请先关闭后再执行构建。"
    }
}

function Build-DesktopExe {
    param([string]$BuildProfile)
    Ensure-Command 'cargo'
    Assert-ProcessNotRunning -ProcessName 'ddscope-desktop'
    if ($BuildProfile -eq 'release') {
        Build-WebUi
        Write-Step 'Building release ddscope-desktop.exe (with custom-protocol embedded UI)'
        Invoke-Native 'cargo' @(
            'build', '--release', '-p', 'ddscope-desktop', '--features', 'custom-protocol'
        )
        $exePath = Join-Path $RepoRoot 'target\release\ddscope-desktop.exe'
    } else {
        Ensure-NodeDeps
        Write-Step 'Building debug ddscope-desktop.exe (connects to Vite dev server unless custom-protocol is enabled)'
        Invoke-Native 'cargo' @('build', '-p', 'ddscope-desktop')
        $exePath = Join-Path $RepoRoot 'target\debug\ddscope-desktop.exe'
    }

    if (Test-Path $exePath) {
        $info = Get-Item $exePath
        Write-Ok "Desktop binary ready: $($info.FullName) ($($info.Length) bytes)"
    }
    if ($SmokeTest) {
        Invoke-DesktopSmokeTest -ExePath $exePath
    }
}

function Invoke-DesktopSmokeTest {
    param([string]$ExePath)
    if (-not (Test-Path $ExePath)) {
        throw "Cannot run smoke test; binary not found at $ExePath"
    }
    Write-Step "Running GUI smoke test (DDSCOPE_SMOKE_EXIT_MS=1500) on $ExePath"
    $prev = $env:DDSCOPE_SMOKE_EXIT_MS
    try {
        $env:DDSCOPE_SMOKE_EXIT_MS = '1500'
        $proc = Start-Process -FilePath $ExePath -Wait -PassThru
        if ($proc.ExitCode -ne 0) {
            throw "Desktop smoke test failed with exit code $($proc.ExitCode)."
        }
        Write-Ok 'Desktop GUI smoke test passed (exit code 0).'
    } finally {
        if ($null -eq $prev) {
            Remove-Item Env:DDSCOPE_SMOKE_EXIT_MS -ErrorAction SilentlyContinue
        } else {
            $env:DDSCOPE_SMOKE_EXIT_MS = $prev
        }
    }
}

function Build-DesktopBundle {
    param([string]$SelectedBundles)
    Ensure-Command 'pnpm'
    Ensure-Command 'cargo'
    Assert-ProcessNotRunning -ProcessName 'ddscope-desktop'
    Ensure-NodeDeps
    Write-Step "Building Tauri v2 desktop app & Windows installer(s) (targets=$SelectedBundles)"
    $args = @('--filter', '@ddscope/desktop', 'tauri', 'build')
    if ($SelectedBundles -ne 'all') {
        $args += @('--bundles', $SelectedBundles)
    }
    Invoke-Native 'pnpm' $args

    $exePath = Join-Path $RepoRoot 'target\release\ddscope-desktop.exe'
    if (Test-Path $exePath) {
        $info = Get-Item $exePath
        Write-Ok "Desktop binary: $($info.FullName) ($($info.Length) bytes)"
    }
    $bundleRoot = Join-Path $RepoRoot 'target\release\bundle'
    if (Test-Path $bundleRoot) {
        Get-ChildItem -Path $bundleRoot -Recurse -File | ForEach-Object {
            Write-Ok "Installer: $($_.FullName) ($($_.Length) bytes)"
        }
    }
    if ($SmokeTest) {
        Invoke-DesktopSmokeTest -ExePath $exePath
    }
}

function Build-CargoTarget {
    param(
        [string]$Package,
        [string]$BuildProfile,
        [string[]]$ExtraArgs = @()
    )
    Ensure-Command 'cargo'
    $args = @('build')
    if ($BuildProfile -eq 'release') {
        $args += '--release'
    }
    if ($Package) {
        $args += @('-p', $Package)
    } else {
        $args += '--workspace'
    }
    if ($ExtraArgs.Count -gt 0) {
        $args += $ExtraArgs
    }
    Invoke-Native 'cargo' $args
}

function Invoke-VerifyGates {
    Ensure-Command 'pnpm'
    Ensure-Command 'cargo'
    Ensure-NodeDeps

    Write-Step '1/4 Frontend TypeScript check (pnpm typecheck)'
    Invoke-Native 'pnpm' @('typecheck')

    Write-Step '2/4 Frontend unit tests (pnpm test:ui)'
    Invoke-Native 'pnpm' @('test:ui')

    Write-Step '3/4 Rust workspace Clippy lints (-D warnings)'
    Invoke-Native 'cargo' @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')

    Write-Step '4/4 Rust full workspace tests (cargo test --workspace)'
    Invoke-Native 'cargo' @('test', '--workspace')

    Write-Ok 'All verification gates passed.'
}

function Invoke-SoakBench {
    param([int]$DurationSecs)
    if ($DurationSecs -lt 1) {
        throw 'SoakSecs must be >= 1.'
    }
    Write-Step 'Building release ddscope-cli'
    Build-CargoTarget -Package 'ddscope-cli' -BuildProfile 'release'

    $cliExe = Join-Path $RepoRoot 'target\release\ddscope-cli.exe'
    $reportPath = Join-Path $RepoRoot 'target\m5_soak_report.json'
    Write-Step "Running ${DurationSecs}s release mixed-signal soak gate"
    Invoke-Native $cliExe @(
        'soak',
        '--duration-secs', "$DurationSecs",
        '--verify-dod',
        '--json-out', $reportPath
    )
    Write-Ok "Soak report saved to $reportPath"
}

function Invoke-Clean {
    $distDir = Join-Path $RepoRoot 'apps\web-ui\dist'
    if (Test-Path $distDir) {
        Write-Step "Removing $distDir"
        Remove-Item -Path $distDir -Recurse -Force
    }
    Ensure-Command 'cargo'
    Write-Step 'Running cargo clean'
    Invoke-Native 'cargo' @('clean')
    Write-Ok 'Workspace build artifacts cleaned.'
}

function Show-Targets {
    Write-Host @"

DDScopeHost Build Targets:
  dev        启动 Tauri 桌面端热重载开发模式 (pnpm dev)
  dev-ui     仅启动前端 Vite 开发服务器 (pnpm dev:ui)
  ui         仅构建前端生产包 apps/web-ui/dist (pnpm build:ui)
  desktop    编译独立桌面程序 ddscope-desktop.exe (Release 自动内嵌前端; 支持 -Profile debug|release, -SmokeTest)
  bundle     构建桌面端与 Windows 安装包 (支持 -BundleTypes all|msi|nsis, -SmokeTest)
  cli        编译 ddscope-cli.exe (支持 -Profile debug|release)
  core       仅编译 ddscope-core 与 ddscope-plugin-api (支持 -Profile debug|release)
  workspace  编译全量 Rust 工作空间 (Release 时先构建前端并启用 custom-protocol)
  verify     运行全量门禁 (typecheck + test:ui + clippy -D warnings + cargo test --workspace)
  soak       编译 Release ddscope-cli 并运行压测 (默认 30s, 可传 -SoakSecs 1800)
  clean      清理 apps/web-ui/dist 与 target 目录

"@ -ForegroundColor White
}

function Select-TargetInteractive {
    Write-Host '=============================================' -ForegroundColor Cyan
    Write-Host '       DDScopeHost 编译构建向导 (Build)      ' -ForegroundColor Cyan
    Write-Host '=============================================' -ForegroundColor Cyan
    Write-Host '  [1] 完整构建: 桌面端 + Windows 安装包 (MSI & NSIS)'
    Write-Host '  [2] 仅构建 MSI 安装包'
    Write-Host '  [3] 仅构建 NSIS 安装包 (-setup.exe)'
    Write-Host '  [4] 构建独立 Release 桌面程序 (ddscope-desktop.exe, 免安装包)'
    Write-Host '  [5] 构建独立 Release 桌面程序 + 启动冒烟测试'
    Write-Host '  [6] 仅构建前端静态资源 (apps/web-ui/dist)'
    Write-Host '  [7] 编译 Release 版命令行工具 (ddscope-cli.exe)'
    Write-Host '  [8] 编译全量 Rust 工作空间 (Release, 含内嵌前端)'
    Write-Host '  [9] 仅编译核心引擎 (ddscope-core, Release)'
    Write-Host ' [10] 启动桌面端开发模式 (热重载)'
    Write-Host ' [11] 运行全量测试与质量门禁 (verify)'
    Write-Host ' [12] 运行 30s Release 混合信号极限压测 (soak)'
    Write-Host ' [13] 清理构建产物 (clean)'
    Write-Host '  [0] 退出'
    Write-Host ''

    $choice = (Read-Host '请输入选项编号 [默认 4]').Trim()
    if ([string]::IsNullOrEmpty($choice)) {
        $choice = '4'
    }

    switch ($choice) {
        '1'  { return @{ Target = 'bundle'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '2'  { return @{ Target = 'bundle'; BundleTypes = 'msi'; Profile = 'release'; SmokeTest = $false } }
        '3'  { return @{ Target = 'bundle'; BundleTypes = 'nsis'; Profile = 'release'; SmokeTest = $false } }
        '4'  { return @{ Target = 'desktop'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '5'  { return @{ Target = 'desktop'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $true } }
        '6'  { return @{ Target = 'ui'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '7'  { return @{ Target = 'cli'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '8'  { return @{ Target = 'workspace'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '9'  { return @{ Target = 'core'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '10' { return @{ Target = 'dev'; BundleTypes = 'all'; Profile = 'debug'; SmokeTest = $false } }
        '11' { return @{ Target = 'verify'; BundleTypes = 'all'; Profile = 'debug'; SmokeTest = $false } }
        '12' { return @{ Target = 'soak'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '13' { return @{ Target = 'clean'; BundleTypes = 'all'; Profile = 'release'; SmokeTest = $false } }
        '0'  { return $null }
        default { throw "无效选项: $choice" }
    }
}

Push-Location $RepoRoot
try {
    if ($List) {
        Show-Targets
        exit 0
    }

    if ($Target -eq 'menu') {
        $selected = Select-TargetInteractive
        if ($null -eq $selected) {
            Write-Host '已取消构建。' -ForegroundColor Yellow
            exit 0
        }
        $Target = $selected.Target
        $BundleTypes = $selected.BundleTypes
        $Profile = $selected.Profile
        if ($selected.SmokeTest) {
            $SmokeTest = $true
        }
    }

    switch ($Target) {
        'dev' {
            Ensure-Command 'pnpm'
            Ensure-NodeDeps
            Write-Step 'Starting Tauri dev mode (pnpm dev)'
            Invoke-Native 'pnpm' @('dev')
        }
        'dev-ui' {
            Ensure-Command 'pnpm'
            Ensure-NodeDeps
            Write-Step 'Starting web-ui Vite dev server (pnpm dev:ui)'
            Invoke-Native 'pnpm' @('dev:ui')
        }
        'ui' {
            Build-WebUi
        }
        'desktop' {
            Build-DesktopExe -BuildProfile $Profile
        }
        'bundle' {
            Build-DesktopBundle -SelectedBundles $BundleTypes
        }
        'cli' {
            Write-Step "Building ddscope-cli ($Profile)"
            Build-CargoTarget -Package 'ddscope-cli' -BuildProfile $Profile
        }
        'core' {
            Write-Step "Building ddscope-plugin-api & ddscope-core ($Profile)"
            Build-CargoTarget -Package 'ddscope-plugin-api' -BuildProfile $Profile
            Build-CargoTarget -Package 'ddscope-core' -BuildProfile $Profile
        }
        'workspace' {
            if ($Profile -eq 'release') {
                Build-WebUi
                Write-Step 'Building full Rust workspace (release + desktop custom-protocol)'
                Build-CargoTarget -Package '' -BuildProfile 'release' -ExtraArgs @('--features', 'ddscope-desktop/custom-protocol')
            } else {
                Write-Step 'Building full Rust workspace (debug)'
                Build-CargoTarget -Package '' -BuildProfile 'debug'
            }
            if ($SmokeTest -and $Profile -eq 'release') {
                Invoke-DesktopSmokeTest -ExePath (Join-Path $RepoRoot 'target\release\ddscope-desktop.exe')
            }
        }
        'verify' {
            Invoke-VerifyGates
        }
        'soak' {
            Invoke-SoakBench -DurationSecs $SoakSecs
        }
        'clean' {
            Invoke-Clean
        }
    }
} finally {
    Pop-Location
}
