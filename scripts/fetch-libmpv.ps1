#Requires -Version 5.1
<#
.SYNOPSIS
    下载 mpv-dev 包并解出 libmpv-2.dll（进程内视频内核的运行时依赖）。

.DESCRIPTION
    官方 mpv.exe 是**静态链接**的，拆不出 DLL；能进程内调用的只有 mpv-dev 构建里的
    libmpv-2.dll。仓库不携带这个 32 MB 压缩包（解压后 122 MB），用本脚本取：

      scripts/fetch-libmpv.ps1              # 通用版（SSE2 基线，老机器也能跑）
      scripts/fetch-libmpv.ps1 -Lgpl        # LGPL 变体（随包分发许可更宽松）
      scripts/fetch-libmpv.ps1 -CopyToExe   # 取完顺手拷到 target/debug 与 target/release
      scripts/fetch-libmpv.ps1 -Force       # 已有 DLL 也重新下载（换版本）

    解压依赖 Windows 自带的 bsdtar（`%SystemRoot%\System32\tar.exe`，带 libarchive，
    能解 .7z），不需要装 7-Zip。
#>
param(
    [string]$Dest = "",
    [switch]$Lgpl,
    [switch]$CopyToExe,
    [switch]$Force,
    [string]$Release = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
if (-not $Dest) { $Dest = Join-Path $root "target/mpv-dev" }

$tar = Join-Path $env:SystemRoot "System32/tar.exe"
if (-not (Test-Path $tar)) { throw "未找到 $tar（Win10 1803+ 自带，用来解 .7z）" }

New-Item -ItemType Directory -Path $Dest -Force | Out-Null
$dll = Join-Path $Dest "libmpv-2.dll"
$archive = Join-Path $Dest "mpv-dev.7z"

if ((Test-Path $dll) -and (-not $Force)) {
    # 已解过就不再下 32 MB
    Write-Host "==> 已有 $dll，跳过下载（换版本加 -Force）"
} else {
    # 选包：x86_64、非 aarch64、非 v3（v3 要 AVX2，老机器会 illegal instruction）
    $prefix = if ($Lgpl) { "mpv-dev-lgpl-x86_64-" } else { "mpv-dev-x86_64-" }
    $api = if ($Release) {
        "https://api.github.com/repos/zhongfly/mpv-winbuild/releases/tags/$Release"
    } else {
        "https://api.github.com/repos/zhongfly/mpv-winbuild/releases/latest"
    }
    Write-Host "==> 查询 $api"
    $release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "chroma-fetch-libmpv" }
    $asset = $release.assets | Where-Object {
        $_.name.StartsWith($prefix) -and
        $_.name.EndsWith(".7z") -and
        -not $_.name.Contains("aarch64")
    } | Select-Object -First 1
    if (-not $asset) { throw "release $($release.tag_name) 里没有 $prefix*.7z" }

    Write-Host "==> 下载 $($asset.name) ($([math]::Round($asset.size / 1MB, 1)) MB)"
    # 大文件走 Invoke-WebRequest 会先把整个包读进内存，直接交给 curl（Win10 1803+ 自带）
    if (Get-Command curl.exe -ErrorAction SilentlyContinue) {
        & curl.exe -L --fail --silent --show-error -o $archive $asset.browser_download_url
        if ($LASTEXITCODE -ne 0) { throw "下载失败（curl exit $LASTEXITCODE）" }
    } else {
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $archive -UseBasicParsing
    }

    Write-Host "==> 解压到 $Dest"
    & $tar -xf $archive -C $Dest
    if ($LASTEXITCODE -ne 0) { throw "解压失败（tar exit $LASTEXITCODE）" }
}

if (-not (Test-Path $dll)) { throw "没有 libmpv-2.dll，检查 $archive" }
$item = Get-Item $dll
Write-Host "    libmpv-2.dll $([math]::Round($item.Length / 1MB, 1)) MB"

if ($CopyToExe) {
    foreach ($profile in @("debug", "release")) {
        $dir = Join-Path $root "target/$profile"
        if (Test-Path $dir) {
            Copy-Item $dll (Join-Path $dir "libmpv-2.dll") -Force
            Write-Host "    已拷贝到 target/$profile"
        }
    }
}

Write-Host ""
Write-Host "==> 完成。应用按以下顺序找 DLL："
Write-Host "    SMP_LIBMPV_PATH / SMP_MPV_PATH → exe 同目录 → PATH → 常见安装路径"
Write-Host "    没拷到 exe 目录时也可以：`$env:SMP_LIBMPV_PATH = '$dll'"
exit 0
