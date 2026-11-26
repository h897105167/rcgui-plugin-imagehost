#!/usr/bin/env bash
# 打包插件（Linux / macOS）：构建 → 组装目录包 → 生成 check.sha256 → 打 zip
#
# 产物：dist/<id>-<version>.zip
# 与 scripts/pack.ps1 行为一致，供 CI 和本地使用。
set -euo pipefail

cd "$(dirname "$0")/.."
repo_dir=$PWD
dist_dir="$repo_dir/dist"

# Windows 上 python3 可能是应用商店占位符（能启动但退出码 49，什么都不做），
# 所以先探测真正能执行的解释器再使用；Git Bash / CI 上 python3 正常。
PY=""
for cand in python3 python; do
  if command -v "$cand" >/dev/null 2>&1 && "$cand" -c 'import sys' >/dev/null 2>&1; then
    PY="$cand"
    break
  fi
done
if [ -z "$PY" ]; then
  echo "找不到可用的 python 解释器" >&2
  exit 1
fi

# Keep the build machine's cargo home out of the published artifact: dependency
# panic locations embed absolute paths, which would leak the user name and the
# local directory layout into main.wasm.
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
command -v cygpath >/dev/null 2>&1 && cargo_home="$(cygpath -w "$cargo_home")"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$cargo_home=/cargo"

if [ "${SKIP_BUILD:-0}" != "1" ]; then
  cargo build --release --target wasm32-wasip1
fi

# 顶层字段用 sed 取，不依赖 python（Windows 上的 python3 常常是应用商店占位符）
json_str() { sed -n "s/^ *\"$1\": *\"\([^\"]*\)\".*/\1/p" config.json | head -1; }

cfg_id=$(json_str id)
cfg_version=$(json_str version)
cfg_entry=$(json_str entry)
[ -n "$cfg_entry" ] || cfg_entry=main.wasm

# 这三项会被拼进路径，后面还要 rm -rf，所以先按白名单收紧：
# 写成 ../../x 或带路径分隔符的一律拒绝，而不是交给拼接结果去赌。
if ! [[ "$cfg_id" =~ ^[a-z0-9][a-z0-9-]{1,63}$ ]]; then
  echo "config.json 的 id 不合法：'$cfg_id'。只允许小写字母、数字、连字符，2-64 字符，且以字母或数字开头。" >&2
  exit 1
fi
if ! [[ "$cfg_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "config.json 的 version 不是 semver：'$cfg_version'（例：0.4.1、1.0.0-rc.1）。" >&2
  exit 1
fi
if ! [[ "$cfg_entry" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*\.wasm$ ]]; then
  echo "config.json 的 entry 不合法：'$cfg_entry'。要是一个 .wasm 文件名，带目录或其它扩展名都不行。" >&2
  exit 1
fi

wasm="target/wasm32-wasip1/release/$cfg_entry"
if [ ! -f "$wasm" ]; then
  echo "找不到 $wasm" >&2
  echo "target 里的 wasm：" >&2
  ls target/wasm32-wasip1/release/*.wasm 2>/dev/null || true
  echo "请让 [[bin]] name 与 config.json 的 entry 对应。" >&2
  exit 1
fi

out="$dist_dir/$cfg_id-$cfg_version"
zip="$dist_dir/$cfg_id-$cfg_version.zip"

# 这两个路径马上会被删掉：只允许动 dist 的直接子项，符号链接一律不碰，
# 免得一个指到别处的链接把删除带出 dist 之外。
for target in "$out" "$zip"; do
  if [ "$(dirname "$target")" != "$dist_dir" ]; then
    echo "拒绝操作：$target 不在 $dist_dir 下。" >&2
    exit 1
  fi
  if [ -L "$target" ]; then
    echo "拒绝操作：$target 是符号链接。" >&2
    exit 1
  fi
done

rm -rf -- "$out"
mkdir -p -- "$out"

cp -- config.json "$out/config.json"
cp -- "$wasm" "$out/$cfg_entry"

# check.sha256：每行 "<sha256>  <相对路径>"，入口必须在表里
(
  cd "$out"
  : > check.sha256
  for f in config.json "$cfg_entry"; do
    printf '%s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$f" >> check.sha256
  done
)

rm -f -- "$zip"
# 不依赖 zip / PowerShell：直接 python 打包（条目落在压缩包根，与 zip -r 一致）
"$PY" - "$out" "$zip" <<'PACKEOF'
import os, sys, zipfile
out, zip_path = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as z:
    for root, _dirs, files in os.walk(out):
        for f in sorted(files):
            p = os.path.join(root, f)
            z.write(p, os.path.relpath(p, out))
PACKEOF

echo
echo "打包完成：$zip"
echo "  插件 id : $cfg_id"
echo "  版本    : $cfg_version"
echo "  sha256  : $(sha256sum "$zip" | cut -d' ' -f1)"
