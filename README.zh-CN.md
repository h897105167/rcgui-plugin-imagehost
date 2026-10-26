# rcgui-plugin-imagehost

**RCGUI 的图床插件** —— 把本地图片传到 rclone 远程，拿回一条稳定的直链；需要时再换一条短链。

[English](README.md) | 简体中文

> 本体仓库：[h897105167/rclone_gui_windows](https://github.com/h897105167/rclone_gui_windows)
> 插件开发文档：[docs/PLUGIN.md](https://github.com/h897105167/rclone_gui_windows/blob/main/docs/PLUGIN.md)

本仓库只放插件自己：Rust 源码、`config.json`、打包脚本、发版 CI。RCGUI 里看到的每个控件都由 `config.json` 的 `plugin_setting` 声明，面板是宿主画出来的 —— 这里没有 HTML、没有前端产物、没有 JavaScript。

`config.json` 里还设了 `"http_entry": true`，于是宿主把 `/plugin/imagehost/...` 交给这个插件。这就是外部客户端的入口：插件能应答 `ping` 探测、能收普通 HTTP 上传，所以任何能发 `POST` 的东西都能拿到链接，完全不必了解 rclone。

## 功能

| 环节 | 你得到的 |
| --- | --- |
| 选图上传 | 选远程、填或浏览目录、挑本机文件 —— 插件通过 rclone 把图传上去 |
| 直链 | K-Vault 这类站点型远程直接给出站点自己的公网链接；普通远程退回由宿主代读的直链 |
| 短链 | 可选的「模板一」转换：请求地址加上 `url` 与 `apikey`，`GET` 或 `POST`，纯文本 / `JSON` / `JSONP` |
| 去重 | 上传目录里已有同名文件时，要么取消上传，要么自动改成 `名字 (1).png` |
| 体积上限 | `max_mb`（默认 20 MB）；超过上限先转 JPEG 再传，或者直接 413 拒绝 |
| HTTP 上传服务 | `ping` 供探测、`upload` 供上传 —— Obsidian 插件对接的就是这一套 |
| 复制按钮 | `复制直链` / `复制短链` 把上一次结果放进剪贴板 |

## 安装

- **随本体分发。** RCGUI 按 `plugins/manifest.json` 下载对应的 Release 包、校验 SHA-256，构建时解进 `crates/app/assets/plugins/imagehost/`，首次启动复制到 `%APPDATA%\RCGUI\plugins\imagehost\`。不需要手动装。
- **手动安装。** 把 `imagehost-<版本>.zip` 解压到 `%APPDATA%\RCGUI\plugins\imagehost\`。包里必须有 `config.json`、`main.wasm`、`check.sha256` 三个文件。
- **卸载。** 在 RCGUI 里「禁用」只隐藏面板，包和设置都留着；「卸载」会连包带设置一起删，包括存着远程、目录和令牌的 `kv` 数据。

## 面板

整个面板就是一张表单，顺序由 `config.json` 自上而下决定。标签目前只有中文 —— 宿主支持 `{"zh":…,"en":…}` 双语标签（见 `docs/PLUGIN.md` §2.1.1），但这个插件还没迁移。

| 键 | 控件 | 标签 | 作用 |
| --- | --- | --- | --- |
| `g-title` | group | 图床工具（上传 → 直链 → 短链） | 整张表单的分组标题 |
| `remote` | select | 存储的远程 | 存到哪个 rclone 远程；列表读自 rclone |
| `refresh-remotes` | action | 刷新远程 | 重新从 rclone 读一遍远程列表 |
| `path` | path | 上传目录 | 远程里的目录；留空表示远程根 |
| `upload_path` | text | 上传图片路径 | 本机图片的完整路径，手填或由 `选择文件…` 填 |
| `pick` | action | 选择文件… | 宿主文件对话框，过滤 `png jpg jpeg gif webp bmp` |
| `dedupe` | bool | 去重 | 默认勾上：同名文件取消上传，而不是覆盖 |
| `dedupe-tip` | hint | — | 说明 `去重`，以及不勾选时改名为 `名字 (1).png` |
| `short_tpl` | select | 短链模板 | `不选择` 或 `模板一`，后者是目前唯一内置的形态 |
| `g-tpl1` | group | 模板一参数 | 短链模板的参数 |
| `short_fmt` | select | 返回格式 | `无`（纯文本）、`JSON` 或 `JSONP` |
| `short_method` | select | 请求方式 | `GET` 或 `POST` |
| `short_rate` | number | 每秒上限 | 每秒请求数上限，默认 4 |
| `short_callback` | text | JSONP 回调 | `JSONP` 用的回调名，默认 `callback` |
| `short_url` | text | 请求地址 | 短链服务的接口地址 |
| `short_key` | password | api_key | 每次请求都带上；`模板一` 必填 |
| `short-tip` | hint | — | 复述参数形状，再提醒去点 `保存设置` |
| `g-http` | group | HTTP 上传服务（给外部客户端） | 给外部客户端用的上传入口 |
| `max_mb` | number | 体积上限（MB） | 允许的最大体积，默认 20 MB |
| `compress` | bool | 超限先压缩 | 默认勾上：超过上限先转成 JPEG 再传 |
| `detect-port` | action | 检测服务 | 探测 `127.0.0.1` 上宿主的 HTTP 端口并填 `上传地址` |
| `http_url` | text | 上传地址 | 由 `检测服务` 填；只是显示用，不是设置项 |
| `http-tip` | hint | — | 说明端口、`ping`、`upload`、压缩与短链回退 |
| `run` | action | 生成直链 | 主操作：上传这张图并给出链接 |
| `save` | action | 保存设置 | 把整张表单写进插件的 `kv` |
| `copy-link` | action | 复制直链 | 把 `直链` 放进剪贴板 |
| `last_link` | text | 直链 | 最近一条直链，`生成直链` 也会写这里 |
| `copy-short` | action | 复制短链 | 把 `短链` 放进剪贴板 |
| `last_short` | text | 短链 | 最近一条短链；模板失败时为空 |
| `state` | status | 状态 | 指示灯，反映上一次动作的结果 |
| `link-tip` | hint | — | 直链的来源：站点型远程，还是宿主代理 |

## HTTP 接口

宿主的 HTTP 服务监听 `127.0.0.1:<端口>`（设置页里的端口，默认 `5273`），`/plugin/imagehost` 下的请求都交给本插件。

| 路由 | 用途 |
| --- | --- |
| `GET /plugin/imagehost/ping` | 探测：这里是哪个插件、上限多少、当前选的哪个远程与目录 |
| `POST /plugin/imagehost/upload` | 上传一张图，拿回 `url` 与 `short_url` |
| `GET /plugin/imagehost/i/<远程>:<路径>` | 经宿主把对象读回来 —— 回退直链就指向这里 |

`ping` 的应答：

```json
{
  "ok": true, "plugin": "imagehost", "version": "0.6.0", "port": 5273,
  "upload_url": "http://127.0.0.1:5273/plugin/imagehost/upload",
  "upload_path": "/plugin/imagehost/upload",
  "accept": "POST multipart/form-data（字段名 file / image）或直接把图片字节当 body",
  "max_mb": 20, "max_bytes": 20971520,
  "compress": true, "short": false,
  "remote": "local", "dir": "image_bed"
}
```

`upload` 的应答：

```json
{
  "ok": true,
  "url": "https://example.com/pics/image_bed/a.png",
  "short_url": null,
  "name": "a.png", "stored": "image_bed/a.png", "remote": "local",
  "content_type": "image/png", "size": 81234, "original_size": 81234,
  "compressed": false, "deduped": false
}
```

- **请求体。** 裸图片字节、`multipart/form-data`（字段名 `file` 或 `image`）、JSON 三种都收。文件名优先取 `?name=`，其次取 multipart 里的文件名。
- **multipart 装不下大图。** 宿主对 multipart 单个字段有约 2 MB 的上限，大图得改用裸字节 POST —— `image-upload-rcgui` 就是这么发的。
- **体积上限。** 超过 `max_mb` 直接 413；勾了 `超限先压缩` 时先在沙箱里解码、转成 JPEG 再传，应答里带 `compressed: true` 和一句 `note`。
- **去重。** 勾着 `去重` 且目录里已有同名文件时，直接把旧链接返回给你，并标 `deduped: true`；不勾则新文件存成 `名字 (1).png`。
- **短链失败不影响上传。** 模板调用出错时 `short_url` 是 `null`，原因写在 `short_error` 里，直链照样可用。
- **代理直链需要宿主在跑。** `/plugin/imagehost/i/...` 每次都去远程现读，所以 RCGUI 关了这条链就断 —— 站点型远程没有这个问题。

## 开发

需要 Rust 1.80 或更新的版本，以及 `wasm32-wasip1` target：

```powershell
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1
pwsh -File scripts/pack.ps1
```

bash 下用 `bash scripts/pack.sh`，两个脚本行为一致。打包会构建 wasm、组装 `dist/<id>-<版本>/`（`config.json`、`main.wasm`、`check.sha256`）、打成 zip，并打印这个 zip 的 SHA-256。构建时会把 cargo home 前缀从二进制里改写掉，避免本机路径留在 `main.wasm` 里。

```
config.json                     身份、能力、http_entry 与整个面板
src/main.rs                     插件本体，单文件
Cargo.toml                      wasm 构建参数与依赖
scripts/pack.ps1                构建、组装 dist/<id>-<版本>、打印 sha256
scripts/pack.sh                 同上，给 bash 与 CI 用
.github/workflows/release.yml   tag -> 校验版本 -> 构建 -> GitHub Release
```

## 发版

1. 把 `config.json` 与 `Cargo.toml` 里的 `version` 一起改掉；tag 与 `config.json` 对不上时 CI 会拒绝。
2. 打 tag 并推送：`git tag v0.7.0`，然后 `git push origin v0.7.0`。
3. CI 构建并把 `imagehost-0.7.0.zip` 挂到 Release 上，同时打印它的 SHA-256。
4. 把这个 tag、包名和 SHA-256 抄进本体仓库的 `plugins/manifest.json`，下一次构建 RCGUI 就会取到新版本。

不要手动 `gh release create`：Release 由 workflow 自己创建并上传附件。

## 插件协议

- 宿主把 `main.wasm` 当作 WASI 命令启动，每轮往 stdin 写一条 JSON 请求。
- 一轮的应答要么是正常结果，要么是 `{"need":[{"cap":…,"params":…}]}`，向宿主申请一次能力调用；宿主答复后这一轮继续。
- 声明使用的能力：`kv`、`ui`、`fs`、`http`、`rclone`、`env`。
- 面板状态存在插件的 `kv` 里 —— `on_start` 跑 `state`，`on_exit` 跑 `exit`。
- 遇到不认识的 action 会回 `未知动作：<名字>`，而不是把这一轮弄崩。

## 已知限制

- 直链能不能公开访问，取决于远程本身是否公开；私有远程拿到的是代理直链，需要 RCGUI 正在运行。
- 代理直链里写死了 `<远程>:<路径>`，所以在远程上改名或移动文件，会让已经分享出去的链接失效。
- `模板一` 是目前唯一支持的短链服务形态，别的服务得改代码。
- 超限压缩在 WASM 沙箱里单线程跑，很大的图要等一会儿。
- 面板标签目前只有中文，见上文。

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
