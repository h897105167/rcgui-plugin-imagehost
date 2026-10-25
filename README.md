# rcgui-plugin-imagehost

**RCGUI 的图床插件** —— 把本地图片传到 rclone 远程，生成稳定直链，并维护一份可搜索的索引。

> 本体仓库：[h897105167/rclone_gui_windows](https://github.com/h897105167/rclone_gui_windows)
> 插件开发文档：[docs/PLUGIN.md](https://github.com/h897105167/rclone_gui_windows/blob/main/docs/PLUGIN.md)

本仓库**只包含插件自己**：Rust 源码、`config.json`、打包脚本、发版 CI。
面板的界面不是代码，而是 `config.json` 里的声明（`plugin_setting`），由本体按声明渲染 —— 所以插件的 UI 与逻辑都在这个仓库里，本体没有任何关于「图床」的硬编码。

---

## 安装

### 内置分发（推荐）

内置分发：构建本体时 `scripts/fetch_plugins.ps1` 按 `plugins/manifest.json` 登记的版本与本仓库 [Release](https://github.com/h897105167/rcgui-plugin-imagehost/releases) 下载 zip、校验 sha256，解包到 `crates/app/assets/plugins/imagehost/`；本体启动时再释放到 `%APPDATA%\RCGUI\plugins\imagehost\`。
升级 = 改本体 manifest 里的 `tag` 与 `sha256`（zip 的哈希）。

### 手动

1. 从 [Releases](https://github.com/h897105167/rcgui-plugin-imagehost/releases) 下载 `imagehost-<版本>.zip`；
2. 解压到 `%APPDATA%\RCGUI\plugins\imagehost\`，目录里应有 `config.json`、`main.wasm`、`check.sha256`；
3. 重启 RCGUI，在「插件」页看到「图床」即可。

卸载：插件页里的「卸载」会把包目录和该插件的 `kv` 数据一起删掉（索引也就没了），
内置插件卸载后会留下墓碑，升级不会自动装回来。

---

## 功能

| 能力 | 说明 |
|---|---|
| 上传 | 选一张本地图片，按当前命名规则传到指定 rclone 远程，返回直链 |
| 去重 | 相同内容（sha1 + 大小）命中已有记录时直接复用旧直链，不重复上传 |
| 命名规则 | `timestamp`（时间戳）/ `seq`（递增序号，可设位数）/ `keep`（保留原名）/ `hash`（内容哈希） |
| 目录布局 | `note`（Obsidian 笔记同名目录）/ `date`（按年月）/ `vault`（按库）/ `flat`（平铺） |
| 索引 | 记录路径、大小、sha1、上传时间，支持按路径/名称过滤、重新读取、刷新 |
| 回收站 | 删除的文件可移入指定远程目录而不是直接删掉 |
| 访问令牌 | 生成 `Bearer` 令牌，供 Obsidian 等外部客户端调用上传/扫描接口 |

## 设置项

面板由 `config.json` 的 `plugin_setting.controls` 声明，本体据此渲染。分组与动作 key：

| 分组 | 控件 key | 类型 |
|---|---|---|
| 图床工具（上传 → 直链） | `remote` / `refresh-remotes` / `path` / `browse` / `subdir` / `upload` / `scan` / `lastlink` / `copy-link` / `state` | 下拉、文本、路径、动作、状态 |
| 存储与命名 | `roots` / `save` / `naming` / `seqpad` / `dedupe` / `obsidian` / `trash` / `base` | 文本、下拉、数字、开关、路径、动作 |
| 已索引 | `reload` / `refresh` / `filter` / `files` | 动作、文本、清单 |
| 访问令牌 | `tokenlabel` / `add-token` | 文本、动作 |
| 提示 | `tip` / `store-tip` / `token-tip` | 提示 |

「存储的远程」的候选不是写死的：插件被调用时用 `rclone config/listremotes` 取列表，通过返回值的 `choices` 交给本体渲染；
所以 `config.json` 里 `options` 只放一个占位项，真实选项运行时才有。「路径」右边的「浏览」列出该远程当前路径的子目录（含上一层）。

`roots` 的写法是 `id=远程路径`，多个用逗号分隔，例如 `local=D:\pics, oss=oss:img/test`。
第一个根是默认根 —— 上面「存储的远程 + 路径」这两栏编辑的就是它。

直链的拼法是 `<公网地址>/i/<根 id>/<路径>`；`公网地址` 留空时用本体服务的 `origin`。
`/i/<根 id>/<路径>` 是插件自己的图片路由（宿主用 `rclone.cat` 读字节），所以公网地址可以反代到本体上。

---

## 开发

需要 Rust 1.80+ 和 `wasm32-wasip1` 目标：

```powershell
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1   # 产物 target/wasm32-wasip1/release/main.wasm
```

打包（生成目录包 + `check.sha256` + zip，并打印 zip 的 sha256）：

```powershell
pwsh -File scripts/pack.ps1        # Windows
bash scripts/pack.sh               # Linux / macOS / CI
```

### 目录结构

```
.
├── Cargo.toml              # package imagehost-plugin, [[bin]] name = "main"
├── config.json             # 插件清单 + 声明式面板（本体只读这个）
├── src/
│   ├── main.rs             # 入口：stdin 读一个 JSON，stdout 写一个 JSON
│   └── ...
├── scripts/
│   ├── pack.ps1            # Windows 打包
│   └── pack.sh             # CI / *nix 打包
└── .github/workflows/
    └── release.yml         # 打 tag → 构建 → 打包 → 发 Release
```

### 发版

1. 改 `config.json` 的 `version`（必须与 tag 一致，CI 会校验）；
2. `git tag v0.4.0 && git push origin v0.4.0`；
3. CI 构建、打包、建 Release，资产名 `imagehost-0.4.0.zip`；
4. 把 Release 页面上打印的 sha256 填进本体的 `plugins/manifest.json`。

> Release 由 CI 建，别在本地 `gh release create` —— 本地包与 CI 包的字节不同（编译器版本/路径差异），
> 同时上传会互相覆盖，manifest 的 sha256 就对不上了。要发版只推 tag。

---

## 面板动作（action 协议）

面板上的按钮点下去，本体带着 `params.action`（控件 key）和 `params.form`（**所有值都是字符串**）重新调用插件：

| action | 等价 HTTP 请求 | 结果（`result.body`） |
|---|---|---|
| `state` / `autostart` | `GET /config` + `GET /stats` | `status`（如「2 个存储根 · 128 张图片」）、`lamp`、`checks`、`hints`、`values`、`choices`（远程列表） |
| `refresh-remotes` | 宿主能力 `rclone.remotes`（读本地 `rclone.conf`，不走 RC） | `choices.remote` + `status:"N 个远程"` |
| `browse` | `rclone operations/list`（`form.remote:form.path`） | `choices.path`：上一层 + 当前路径下的子目录 |
| `save` | `PUT /config`（由 `form` 拼 body） | `status:"已保存"`、`values` 回填规范化后的配置；校验失败 400 + `lamp:"err"` |
| `reload` | `GET /config`，并重新读 `kv` | 同 `state`，但不受旧快照影响 |
| `refresh` / `scan` | `POST /scan` | 扫描结果 `added`/`total` + 刷新后的状态 |
| `upload` | `POST /upload`（`form.file` 经 `fs.read_b64` 合成 multipart） | 直链写进 `values.lastlink`，`toast` 提示上传成功 |
| `add-token` | `POST /token`（`form.tokenlabel` → `name`） | `status:"已生成令牌：<令牌>"` |
| `copy-link` | 无 HTTP 路由，直接 `ui.copy` | `status:"已复制"` |
| `shutdown` / `exit` | 不请求任何宿主能力 | `status:"ok"` |
| `http` | `params.http` 原样分发 | 与旧版一致，路由表不变 |
| `detect-port` | 无 HTTP 路由，直接读本体的 `%APPDATA%\RCGUI\config.json` | `values.http_port`（本体当前的端口）+ `status`，用来把面板里的端口对齐到本体设置 |

面板可写回本体的字段：`status`、`lamp`（`ok`/`warn`/`err`）、`checks`（`[{label,value,ok}]`）、
`hints`（`{key:text}`）、`values`（回填进表单）、`enabled`、`progress`（负数 = 不确定进度）、
`toast`（`{level,message}`），以及图片路由专用的 `content_type` / `body_b64`。多余字段宿主会忽略。

新增的宿主能力调用：`fs.read_b64`（读待上传文件，上限 64 MiB）、`rclone.cat`（读图片字节，上限 32 MiB）、
`ui.copy`（复制直链）。

---

## HTTP 上传服务（给外部客户端用）

除了面板上的按钮，本插件还接住本体转发来的、专门用于图床的 HTTP 请求。外部客户端（例如 Obsidian 插件
`image-upload-rcgui`）把图片 POST 过来，拿回直链或短链。

端口不归插件管：本体的「插件 HTTP 服务」端口（设置里那一项，默认 `5273`）决定入口；端口被占用时由本体提示
「端口被占用」，插件面板里的「服务」分组只是显示/对齐它（「检测端口」按钮读取本体 `config.json` 里的 `http_port`）。

| 路由 | 说明 |
|---|---|
| `GET /plugin/imagehost/ping` | 探活 + 读服务端配置：`{ok, plugin, version, port, max_mb, max_bytes, compress, short, remote, dir, upload_path, upload_url, accept}` |
| `POST /plugin/imagehost/upload?name=<文件名>` | 上传一张图。body **直接放图片字节**（推荐），`Content-Type` 填图片 MIME；也接受 `multipart/form-data`（字段名 `file` 或 `image`） |
| `GET /plugin/imagehost/i/...` | 直链代理（本体把该路径转给插件，插件用 `rclone.cat` 读字节） |

成功响应（200）：

```json
{"ok":true,"url":"https://…/x.png","short_url":"http://i71i.com/l8zk","name":"x.png",
 "stored":"image_bed/x.png","remote":"k-vault","content_type":"image/png","size":8,
 "original_size":8,"compressed":false,"deduped":false}
```

* 超过「单张上限」（面板里的 `max_mb`，默认 20 MB）时，若 `compress` 开着，插件先在 WASM 里解码、必要时
  重新编码成 JPG 压到上限内，再上传；`note` 字段会说明压了多少。压不动或 `compress` 关着则返回 413。
* `short` 开着时会顺手取短链；短链服务失败不影响直链，失败原因在 `short_error`。
* 同名文件默认不覆盖：命中已有索引会直接复用（`deduped:true`）；没有索引但远端已有同名文件时改名为 `name-1.png`。
* multipart 走本体解析，**实测 2 MB 以上会被本体判成「multipart 里没有文件」**，所以大图请用原始字节 body。

---

## 协议

插件是一个 **WASI 命令行程序**：每次调用，本体把一段 JSON 写到 stdin，插件把一段 JSON 写到 stdout。
需要宿主能力时，插件返回 `{"need":[{"cap":"...","params":{...}}]}`，本体执行后带着结果重新运行插件。
完整协议、能力清单、面板 schema、命名规则、接入与分发规则见本体的
**[docs/PLUGIN.md](https://github.com/h897105167/rclone_gui_windows/blob/main/docs/PLUGIN.md)**。

本插件声明的能力：`kv`（索引与配置）、`fs`（读本地图片、路径选择）、`http`（下载远程图片做校验）、
`rclone`（列目录、复制文件）、`ui`（提示与剪贴板）、`env`（数据目录）。

---

## 已知限制

* 直链依赖 rclone 远程本身可公开访问；插件只负责拼接 URL，不做反代。
* `/i/<根 id>/<路径>` 需要本体把该路径转给插件（插件再用 `rclone.cat` 读字节）；本体若没接这条路由，
  公网地址就得指向 rclone 远程自己的公开地址。
* 索引存在插件私有 `kv` 里（`%APPDATA%\RCGUI\plugins\imagehost\kv.json`），卸载插件会一起删除。
* 同一远程被多个 RCGUI 实例同时写入时，索引可能落后，需要手动「扫描索引」重建。
