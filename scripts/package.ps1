#Requires -Version 5.1
<#
.SYNOPSIS
    构建并打包 Simple Media Player 的 Windows 发布包。

.DESCRIPTION
    产出 dist/simple-media-player-<版本>/ 目录与同名 .zip：
      - simple-media-player.exe（release）
      - README.md
      - 可选：libmpv-2.dll（-WithLibmpv，GPL/LGPL 许可，随包分发需自行确认合规）

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/package.ps1
    powershell -ExecutionPolicy Bypass -File scripts/package.ps1 -WithLibmpv
#>
param(
    [Alias("WithMpv")]
    [switch]$WithLibmpv,
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
    # cargo 把进度和错误都写 stderr，而上面 `$ErrorActionPreference = "Stop"` 会让
    # 原生命令的 stderr 输出变成终止性错误（NativeCommandError）——脚本其实跑完了，
    # 退出码却是 1。这里干脆只信 `$LASTEXITCODE`：临时放宽，跑完立刻还原。
    $previousEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & cargo build --release -p simple-media-player
    $buildCode = $LASTEXITCODE
    $ErrorActionPreference = $previousEap
    if ($buildCode -ne 0) { throw "release 构建失败（cargo exit $buildCode）" }
}

$exe = Join-Path $root "target/release/simple-media-player.exe"
if (-not (Test-Path $exe)) { throw "未找到 $exe" }

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null

Copy-Item $exe (Join-Path $stage "simple-media-player.exe")
Copy-Item (Join-Path $root "README.md") (Join-Path $stage "README.md")

# README 里引用的截图也要带上，否则发布包里的图全是坏链（相对路径 docs/screenshots）
$shots = Join-Path $root "docs/screenshots"
if (Test-Path $shots) {
    $shotStage = Join-Path $stage "docs/screenshots"
    New-Item -ItemType Directory -Path $shotStage -Force | Out-Null
    Copy-Item (Join-Path $shots "vitrine-*.png") $shotStage
}

# 可选：把 libmpv-2.dll 一起打进去（视频功能需要；进程内加载，DLL 必须和 exe 同目录）
# 注意：官方 mpv.exe 是静态链接的，拆不出 DLL —— 这里要的是 mpv-dev 构建里的
# libmpv-2.dll（scripts/fetch-libmpv.ps1 可自动下载到 target/mpv-dev）。
if ($WithLibmpv) {
    $dll = $null
    $probe = @()
    if ($env:SMP_LIBMPV_PATH) { $probe += $env:SMP_LIBMPV_PATH }
    if ($env:SMP_MPV_PATH) { $probe += (Join-Path (Split-Path -Parent $env:SMP_MPV_PATH) "libmpv-2.dll") }
    $probe += @(
        (Join-Path $root "target/mpv-dev/libmpv-2.dll"),
        (Join-Path $root "target/release/libmpv-2.dll"),
        "C:\ProgramData\chocolatey\lib\mpvio.install\tools\libmpv-2.dll",
        "C:\Program Files\mpv\libmpv-2.dll",
        (Join-Path $root "libmpv-2.dll")
    )
    foreach ($candidate in $probe) {
        if ($candidate -and (Test-Path $candidate)) { $dll = Get-Item $candidate; break }
    }
    if (-not $dll) {
        throw "未找到 libmpv-2.dll：先跑 scripts/fetch-libmpv.ps1，或用 SMP_LIBMPV_PATH 指定"
    }

    Write-Host "==> 附带 libmpv: $($dll.FullName) ($([math]::Round($dll.Length / 1MB, 1)) MB)"
    Copy-Item $dll.FullName (Join-Path $stage "libmpv-2.dll")

    # mpv 是 GPL/LGPL 双许可，随包分发时把许可文件一并带上
    foreach ($dir in @($dll.DirectoryName, (Join-Path $root "target/mpv-dev"))) {
        $license = Join-Path $dir "LICENSE*"
        if (Test-Path $license) {
            New-Item -ItemType Directory -Path (Join-Path $stage "licenses") -Force | Out-Null
            Copy-Item $license (Join-Path $stage "licenses") -Force
            break
        }
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

# 旧压缩包删不掉不该让整个打包失败：某些环境把 Remove-Item 换成了"必须进回收站"的
# 安全删除，删除失败会直接 throw（fail-closed）。这里退到 -Force 覆盖同名文件。
if (Test-Path $zip) {
    try {
        Remove-Item $zip -Force -ErrorAction Stop
    } catch {
        Write-Host "    （旧压缩包没能删除，改为覆盖写入）"
    }
}
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -Force
$zipItem = Get-Item $zip

Write-Host ""
Write-Host "==> 完成"
Write-Host "    目录: $stage"
Write-Host ("    压缩包: {0} ({1:N1} MB)" -f $zip, ($zipItem.Length / 1MB))

# 显式给成功退出码：`& script.ps1` 的 $LASTEXITCODE 只会保留**上一条原生命令**的码，
# cargo 往 stderr 写东西时会留下非 0（实测 -1），让"打包成功"看起来像失败。
# 失败路径都是 throw，不需要靠退出码区分。
exit 0
