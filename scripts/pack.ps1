# 打包插件：构建 → 组装目录包 → 生成 check.sha256 → 打 zip
#
# 产物：dist/<id>-<version>.zip
#   config.json
#   main.wasm
#   check.sha256
#
# 打完会打印 zip 的 sha256，把它填进本体的 plugins/manifest.json。
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

if (-not $SkipBuild) {
    cargo build --release --target wasm32-wasip1
    if ($LASTEXITCODE -ne 0) { throw "wasm 构建失败（退出码 $LASTEXITCODE）" }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "找不到 cargo，请先安装 Rust：https://rustup.rs"
    }
}

$cfg = Get-Content config.json -Raw -Encoding UTF8 | ConvertFrom-Json
$id = $cfg.id
$version = $cfg.version
$entry = if ($cfg.entry) { $cfg.entry } else { "main.wasm" }

$wasm = "target/wasm32-wasip1/release/$entry"
if (-not (Test-Path $wasm)) {
    # 包名和 [[bin]] name 不一致时给出提示，避免"构建成功但产物名不对"
    $built = Get-ChildItem "target/wasm32-wasip1/release" -Filter *.wasm -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty Name
    throw "找不到 $wasm。target 里的 wasm：$($built -join ', ')。请让 [[bin]] name 与 config.json 的 entry 对应。"
}

$out = "dist/$id-$version"
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force -Path $out | Out-Null

Copy-Item config.json "$out/config.json"
Copy-Item $wasm "$out/$entry"

# check.sha256：每行 "<sha256>  <相对路径>"，入口必须在表里
$files = @("config.json", $entry)
$lines = foreach ($f in $files) {
    $h = (Get-FileHash (Join-Path $out $f) -Algorithm SHA256).Hash.ToLower()
    "{0}  {1}" -f $h, $f
}
Set-Content -Path "$out/check.sha256" -Value ($lines -join "`n") -Encoding ascii -NoNewline

$zip = "dist/$id-$version.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$out/*" -DestinationPath $zip -Force

$zipHash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Write-Host ""
Write-Host "打包完成：$zip" -ForegroundColor Green
Write-Host "  插件 id : $id"
Write-Host "  版本    : $version"
Write-Host "  体积    : $([Math]::Round((Get-Item $zip).Length / 1KB, 1)) KB"
Write-Host "  sha256  : $zipHash" -ForegroundColor Yellow
Write-Host ""
Write-Host "把它填进本体 plugins/manifest.json 的对应条目（tag = v$version, asset = $id-$version.zip）。"
