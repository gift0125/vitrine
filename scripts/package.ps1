#Requires -Version 5.1
<#
.SYNOPSIS
    构建并打包 Simple Media Player 的 Windows 发布包。

.DESCRIPTION
    产出 dist/simple-media-player-<版本>/ 目录与同名 .zip：
      - simple-media-player.exe（release）
      - README.md
      - 可选：mpv.exe（-WithMpv，注意 mpv 为 GPL 许可，随包分发需自行确认合规）

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/package.ps1
    powershell -ExecutionPolicy Bypass -File scripts/package.ps1 -WithMpv
#>
param(
    [switch]$WithMpv,
    [string]$OutDir = "dist",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# 版本号取自 crate 清单，避免两处手写不一致
$manifest = Join-Path $root "crates/app/Cargo.toml"
$version = (Select-String -Path $manifest -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$name = "simple-media-player-$version"
$stage = Join-Path $root (Join-Path $OutDir $name)
$zip = Join-Path $root (Join-Path $OutDir "$name.zip")

Write-Host "==> 版本 $version"

if (-not $SkipBuild) {
    Write-Host "==> cargo build --release"
    cargo build --release -p simple-media-player
    if ($LASTEXITCODE -ne 0) { throw "release 构建失败" }
}

$exe = Join-Path $root "target/release/simple-media-player.exe"
if (-not (Test-Path $exe)) { throw "未找到 $exe" }

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null

Copy-Item $exe (Join-Path $stage "simple-media-player.exe")
Copy-Item (Join-Path $root "README.md") (Join-Path $stage "README.md")

# 可选：把 mpv 一起打进去（视频功能需要）
if ($WithMpv) {
    $mpv = $null
    if ($env:SMP_MPV_PATH -and (Test-Path $env:SMP_MPV_PATH)) {
        $mpv = Get-Item $env:SMP_MPV_PATH
    } else {
        $candidates = @(
            "C:\ProgramData\chocolatey\lib\mpvio.install\tools\mpv.exe",
            "C:\Program Files\mpv\mpv.exe",
            (Join-Path $root "mpv.exe")
        )
        foreach ($candidate in $candidates) {
            if (Test-Path $candidate) { $mpv = Get-Item $candidate; break }
        }
    }
    if (-not $mpv) { throw "未找到 mpv.exe：设置 SMP_MPV_PATH 或用 -WithMpv 前先安装 mpv" }

    Write-Host "==> 附带 mpv: $($mpv.FullName) ($([math]::Round($mpv.Length / 1MB, 1)) MB)"
    Copy-Item $mpv.FullName (Join-Path $stage "mpv.exe")

    $license = Join-Path $mpv.DirectoryName "LICENSE*"
    if (Test-Path $license) {
        New-Item -ItemType Directory -Path (Join-Path $stage "licenses") -Force | Out-Null
        Copy-Item $license (Join-Path $stage "licenses") -Force
    }
}

# 打包清单：便于用户核对内容与校验和
$files = Get-ChildItem $stage -File -Recurse | Sort-Object FullName
$lines = @("# $name", "", "文件清单：", "")
foreach ($f in $files) {
    $rel = $f.FullName.Substring($stage.Length + 1).Replace("\", "/")
    $hash = (Get-FileHash $f.FullName -Algorithm SHA256).Hash
    $lines += ("- {0}  ({1:N0} 字节)  sha256={2}" -f $rel, $f.Length, $hash.ToLower())
}
$lines += ""
$lines += "生成时间：$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
Set-Content -Path (Join-Path $stage "MANIFEST.txt") -Value $lines -Encoding UTF8

if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
$zipItem = Get-Item $zip

Write-Host ""
Write-Host "==> 完成"
Write-Host "    目录: $stage"
Write-Host ("    压缩包: {0} ({1:N1} MB)" -f $zip, ($zipItem.Length / 1MB))
