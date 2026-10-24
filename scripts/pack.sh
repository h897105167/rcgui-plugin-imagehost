#!/usr/bin/env bash
# 打包插件（Linux / macOS）：构建 → 组装目录包 → 生成 check.sha256 → 打 zip
#
# 产物：dist/<id>-<version>.zip
# 与 scripts/pack.ps1 行为一致，供 CI 和本地使用。
set -euo pipefail

cd "$(dirname "$0")/.."

if [ "${SKIP_BUILD:-0}" != "1" ]; then
  cargo build --release --target wasm32-wasip1
fi

cfg_id=$(python3 -c 'import json,sys;print(json.load(open("config.json"))["id"])')
cfg_version=$(python3 -c 'import json,sys;print(json.load(open("config.json"))["version"])')
cfg_entry=$(python3 -c 'import json,sys;print(json.load(open("config.json")).get("entry") or "main.wasm")')

wasm="target/wasm32-wasip1/release/$cfg_entry"
if [ ! -f "$wasm" ]; then
  echo "找不到 $wasm" >&2
  echo "target 里的 wasm：" >&2
  ls target/wasm32-wasip1/release/*.wasm 2>/dev/null || true
  echo "请让 [[bin]] name 与 config.json 的 entry 对应。" >&2
  exit 1
fi

out="dist/$cfg_id-$cfg_version"
rm -rf "$out"
mkdir -p "$out"

cp config.json "$out/config.json"
cp "$wasm" "$out/$cfg_entry"

# check.sha256：每行 "<sha256>  <相对路径>"，入口必须在表里
(
  cd "$out"
  : > check.sha256
  for f in config.json "$cfg_entry"; do
    printf '%s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$f" >> check.sha256
  done
)

zip="dist/$cfg_id-$cfg_version.zip"
rm -f "$zip"
( cd "$out" && zip -q -r "../$cfg_id-$cfg_version.zip" . )

echo
echo "打包完成：$zip"
echo "  插件 id : $cfg_id"
echo "  版本    : $cfg_version"
echo "  sha256  : $(sha256sum "$zip" | cut -d' ' -f1)"
