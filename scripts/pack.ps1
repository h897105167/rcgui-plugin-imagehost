# 打包插件：构建 → 组装目录包 → 生成 check.sha256 → 打 zip
#
# 产物：dist/<id>-<version>.zip，内含 config.json、入口 wasm、check.sha256。
#
# 用 pwsh 跑：pwsh -NoProfile -File scripts\pack.ps1
# （Windows PowerShell 5.1 按 ANSI 读脚本，本文件的中文注释会让它解析失败。）
#
# id 和 version 来自 config.json，会被拼进要递归删除的路径。下面先按白名单校验，
# 再把每个目标路径限制在固定的 dist 目录的直接子项上，并拒绝符号链接/联接点。
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repo = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))

# 别把本机的 cargo 目录写进发布的 wasm：依赖里 panic 的位置信息会带上绝对路径，
# 等于把用户名和目录结构带出去。
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME ".cargo" }
$env:RUSTFLAGS = "$env:RUSTFLAGS --remap-path-prefix=$cargoHome=/cargo"

function Fail([string]$msg) { throw $msg }

# 只允许小写字母、数字、连字符，2-64 字符。够表达插件名，又拼不出路径分隔符、盘符或 ..
function Assert-SafeId([string]$value) {
    if ($value -notmatch '^[a-z0-9][a-z0-9-]{1,63}$') {
        Fail "config.json 的 id 不合法：'$value'。只允许小写字母、数字、连字符，2-64 字符，且以字母或数字开头。"
    }
    return $value
}

function Assert-Semver([string]$value) {
    if ($value -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$') {
        Fail "config.json 的 version 不是 semver：'$value'（例：0.4.1、1.0.0-rc.1）。"
    }
    return $value
}

# 入口文件名同样会拼进路径，且必须落在包根，不能带目录。
function Assert-EntryName([string]$value) {
    if ($value -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*\.wasm$') {
        Fail "config.json 的 entry 不合法：'$value'。要是一个 .wasm 文件名，带目录或其它扩展名都不行。"
    }
    return $value
}

$dist = [IO.Path]::GetFullPath((Join-Path $repo "dist"))

# 每个要删或要写的路径都过这里：必须是 dist 的直接子项，且不是重定向的链接。
# 这样 config.json 写错一个字段，最坏也只是打包失败，碰不到 dist 之外的文件。
function Assert-DistChild([string]$path, [string]$what) {
    $full = [IO.Path]::GetFullPath($path)
    if ([IO.Path]::GetDirectoryName($full) -ne $dist) {
        Fail "$what 不在 dist 的直接子目录里：$full"
    }
    if (Test-Path -LiteralPath $full) {
        $item = Get-Item -LiteralPath $full -Force
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            Fail "$what 是符号链接或联接点，拒绝改动：$full"
        }
    }
    return $full
}

if (-not $SkipBuild) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail "找不到 cargo，请先安装 Rust：https://rustup.rs"
    }
    cargo build --release --target wasm32-wasip1
    if ($LASTEXITCODE -ne 0) { Fail "wasm 构建失败（退出码 $LASTEXITCODE）" }
}

$cfgPath = Join-Path $repo "config.json"
$cfg = Get-Content -LiteralPath $cfgPath -Raw -Encoding UTF8 | ConvertFrom-Json
if (-not $cfg) { Fail "读不出 config.json：$cfgPath" }

$id = Assert-SafeId $cfg.id
$version = Assert-Semver $cfg.version
$entry = Assert-EntryName $(if ($cfg.entry) { $cfg.entry } else { "main.wasm" })

$releaseDir = Join-Path $repo "target\wasm32-wasip1\release"
$wasm = Join-Path $releaseDir $entry
if (-not (Test-Path -LiteralPath $wasm)) {
    $built = Get-ChildItem -LiteralPath $releaseDir -Filter *.wasm -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty Name
    Fail "找不到 $wasm。target 里的 wasm：$($built -join ', ')。请让 [[bin]] name 与 config.json 的 entry 对应。"
}

$outDir = Assert-DistChild (Join-Path $dist "$id-$version") "包目录"
$zipPath = Assert-DistChild (Join-Path $dist "$id-$version.zip") "zip"

if (Test-Path -LiteralPath $outDir) { Remove-Item -LiteralPath $outDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

Copy-Item -LiteralPath $cfgPath -Destination (Join-Path $outDir "config.json")
Copy-Item -LiteralPath $wasm -Destination (Join-Path $outDir $entry)

# 每行「<sha256>  <文件名>」，与 sha256sum 的输出一致，两边脚本可互换。
$files = @("config.json", $entry)
$lines = foreach ($f in $files) {
    $h = (Get-FileHash -LiteralPath (Join-Path $outDir $f) -Algorithm SHA256).Hash.ToLower()
    "{0}  {1}" -f $h, $f
}
Set-Content -LiteralPath (Join-Path $outDir "check.sha256") -Value ($lines -join "`n") -Encoding ascii -NoNewline

if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
Compress-Archive -Path (Join-Path $outDir "*") -DestinationPath $zipPath -Force

$zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLower()
Write-Host ""
Write-Host "打包完成：$zipPath" -ForegroundColor Green
Write-Host "  插件 id : $id"
Write-Host "  版本    : $version"
Write-Host "  体积    : $([Math]::Round((Get-Item -LiteralPath $zipPath).Length / 1KB, 1)) KB"
Write-Host "  sha256  : $zipHash" -ForegroundColor Yellow
Write-Host ""
Write-Host "把它填进本体 plugins/manifest.json 对应条目（tag = v$version, asset = $id-$version.zip）。"
