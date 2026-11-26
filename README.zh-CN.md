# rcgui-plugin-imagehost

**RCGUI 的图床插件** —— 把本机图片传到 rclone 远程，拿回一条能长期用的直链，需要的话再换成短链。

[English](README.md) | 简体中文

> 宿主程序：[h897105167/rclone_gui_windows](https://github.com/h897105167/rclone_gui_windows)
> 插件开发文档：[docs/PLUGIN.md](https://github.com/h897105167/rclone_gui_windows/blob/main/docs/PLUGIN.md)

这个仓库只有插件本身：Rust 源码、`config.json`、打包脚本和发布工作流。你在 RCGUI 里看到的每个控件，都由 `config.json` 的 `plugin_setting` 声明，面板由宿主画出来 —— 没有 HTML、没有前端包、没有 JavaScript。每个 label、placeholder 和提示语都在这个文件里写了两份，`{"zh":…,"en":…}`，宿主按自己界面当前的语言挑一份来显示。

面板分成几页。`plugin_setting.pages` 按页签顺序列出这些页，每个控件写明自己属于哪一页，所以把一个控件挪到另一页只是改一个字段；页内的先后顺序就是在文件里的先后顺序。`line` 相同的控件画在同一行里。

`config.json` 里的 `"http_entry": true` 会让宿主把 `/plugin/imagehost/...` 转给这个插件。这就是给外部客户端留的门：插件回应一个 `ping` 探测，也接受普通 HTTP 上传，所以任何能发 `POST` 的东西都能拿到链接，完全不用懂 rclone。

## 功能

| 模块 | 能做什么 |
| --- | --- |
| 选远程、传图 | 选远程、填或浏览目录、挑一张本机图片，插件通过 rclone 传上去 |
| 直链 | K-Vault 这类站点型远程直接返回自己的公开地址；普通远程退回宿主代理的链接 |
| 短链 | 可选的模板一转换：请求地址加 `url` 和 `apikey`，`GET` 或 `POST`，返回纯文本 / `JSON` / `JSONP` |
| 去重 | 上传目录里已有同名对象时，直接复用已有的那条链接，不重复上传 |
| 体积上限 | `max_mb`（默认 20 MB）；超过就压成 JPEG，关掉压缩则用 413 拒绝 |
| HTTP 上传服务 | `ping` 探测、`upload` 上传 —— Obsidian 插件说的就是它 |
| 复制按钮 | 「复制直链」「复制短链」把上一次的结果放进剪贴板 |
| 分页面板 | 一张长表单拆成五页：上传、存储与链接、压缩与命名、HTTP 接入、高级 |

## 安装

- **随程序内置**。RCGUI 构建时按 `plugins/manifest.json` 指定的发布包下载、校验 SHA-256，解到 `crates/app/assets/plugins/imagehost/`；首次启动再拷到 `%APPDATA%\RCGUI\plugins\imagehost\`。不需要手工安装。
- **手动**。把 `imagehost-<版本>.zip` 解到 `%APPDATA%\RCGUI\plugins\imagehost\`，包里必须有 `config.json`、`main.wasm` 和 `check.sha256`。
- **卸载**。在 RCGUI 里停用插件只保留包和设置；卸载会删掉两者，包括存着远程、目录和令牌的 `kv` 存储。

## 面板

五页，按这个顺序。标签在中英各声明一份，跟宿主界面语言走；下表列的是中文界面下看到的文字。

| 页 | `id` | 放什么 |
| --- | --- | --- |
| 上传 | `upload` | 选远程、选图片、拿链接、保存 |
| 存储与链接 | `storage` | 文件存到哪、短链模板、上次的结果和对应的复制按钮 |
| 压缩与命名 | `compress` | 体积上限、压缩、去重 |
| HTTP 接入 | `http` | 给外部客户端用的上传服务，以及它的地址 |
| 高级 | `advanced` | 短链模板的参数 |

`state` 是唯一没有 `page` 的控件：它不属于任何一页，状态灯和最后一次操作的提示文字由宿主画在面板上。

### 上传

| 键 | 控件 | 标签 | 作用 |
| --- | --- | --- | --- |
| `g-title` | group | 选择远程与文件 | 本页的标题 |
| `remote` | select | 存储远程 | 要存进哪个 rclone 远程，列表从 rclone 读出来 |
| `refresh-remotes` | action | 刷新列表 | 重新从 rclone 读一次远程列表 |
| `upload_path` | text | 图片路径 | 本机图片的绝对路径 —— 手填或由「选择图片…」填入 |
| `pick` | action | 选择图片… | 宿主的文件对话框，按 `png jpg jpeg gif webp bmp` 过滤 |
| `run` | action | 生成直链 | 主操作：上传图片并产出链接 |
| `save` | action | 保存设置 | 把整张表单写进插件的 `kv` 存储 |

### 存储与链接

| 键 | 控件 | 标签 | 作用 |
| --- | --- | --- | --- |
| `path` | path | 存储目录 | 远程里的目录；留空就是远程根 |
| `short_tpl` | select | 短链模板 | 不选择或模板一；模板一是唯一内置的形态 |
| `last_link` | text | 直链 | 上一条直链；「生成直链」也会写这里 |
| `copy-link` | action | 复制直链 | 与「直链」同一行，复制它 |
| `last_short` | text | 短链 | 上一条短链；模板失败时是空的 |
| `copy-short` | action | 复制短链 | 与「短链」同一行，复制它 |
| `link-tip` | hint | — | 直链是从哪来的：站点型远程，还是宿主的代理 |

### 压缩与命名

| 键 | 控件 | 标签 | 作用 |
| --- | --- | --- | --- |
| `max_mb` | number | 体积上限 MB | 允许的最大体积，默认 20 MB |
| `compress` | bool | 超限先压缩 | 默认开：超过上限就先压成 JPEG 再传 |
| `dedupe` | bool | 去重 | 默认开：同名对象直接复用已有链接，不重复上传 |
| `dedupe-tip` | hint | — | 说明「去重」，以及关掉后新文件会改名成 `名字 (1).png` |

### HTTP 接入

| 键 | 控件 | 标签 | 作用 |
| --- | --- | --- | --- |
| `g-http` | group | 上传服务 | 外部客户端要访问的接口 |
| `http_port` | number | 本机端口 | RCGUI 实际在监听的口；主程序已经没有 HTTP 设置页，自动读不到时就靠这一项填 |
| `http_url` | text | 上传地址 | 由「检测端口」按上面的端口填；只是展示用，不是设置项 |
| `detect-port` | action | 检测端口 | 先读宿主配置里的端口；读不到就沿用「本机端口」里填的值重算上传地址 |
| `http-tip` | hint | — | 端口从哪来（先读宿主配置，读不到用「本机端口」，默认 `5273`），以及 `ping` 和 `upload` 收什么 |

### 高级

| 键 | 控件 | 标签 | 作用 |
| --- | --- | --- | --- |
| `g-tpl1` | group | 模板一参数 | 短链模板的参数 |
| `short_fmt` | select | 返回格式 | 无（纯文本）、`JSON` 或 `JSONP` |
| `short_method` | select | 请求方式 | `GET` 或 `POST` |
| `short_rate` | number | 每秒上限 | 每秒请求数上限，默认 4 |
| `short_callback` | text | JSONP 回调 | `JSONP` 用的回调名，默认 `callback` |
| `short_url` | text | 请求地址 | 短链服务的接口地址 |
| `short_key` | password | api_key | 每次请求都带上；模板一必填 |
| `short-tip` | hint | — | 复述这几个参数，并提示先点「保存设置」 |

## 权限

`config.json` 里的 `capabilities` 说明这个插件可以向宿主申请什么。一轮对话要么带回结果，要么申请一次能力调用，所以某一项被拒绝只会让那一个动作失败，不影响别的。

宿主版本：0.3.0 或更新。面板分页从宿主 0.3.0 起支持；更老的宿主会忽略 `page` 字段，把所有控件平铺在一页，功能不丢。0.7.1 还没有发布，最新发布过的版本是 0.6.1。

| 能力 | 用来做什么 | 数据去哪 |
| --- | --- | --- |
| `kv` | 读回保存的表单和上一条链接，再写回去 | 宿主在 `%APPDATA%\RCGUI` 下的本地存储 |
| `fs` | 文件对话框、读选中的那张图、读宿主自己的 `config.json` 找它的 HTTP 端口 | 只在本机磁盘上 |
| `ui` | 复制到剪贴板、浮层提示 | 哪都不去 |
| `env` | 取用户目录和数据目录，拼出宿主配置的路径 | 哪都不去 |
| `rclone` | 列出远程、上传图片、代理直链时把对象读回来 | 你选中的那个远程 |
| `http` | 模板一的短链请求 | 你填在「请求地址」里的那个接口 |

只有后两条会往外发：除非你选了那个 rclone 远程、或配了那个短链接口，否则不会把任何东西发出去。没有埋点，也没有更新检查。

## HTTP 接口

宿主的 HTTP 服务监听 `127.0.0.1:<端口>`，默认 `5273`，`/plugin/imagehost` 下的一切都路由到这里。点「检测端口」时先去宿主配置里读端口；如果这个宿主版本已经没有那项设置，就在「HTTP 接入」页的「本机端口」里填上实际端口，再点一次「检测端口」重算上传地址。

| 路由 | 用途 |
| --- | --- |
| `GET /plugin/imagehost/ping` | 探测：谁在这里应答、限制是什么、当前选的远程和目录 |
| `POST /plugin/imagehost/upload` | 传一张图，回 `url` 和 `short_url` |
| `GET /plugin/imagehost/i/<远程>:<路径>` | 通过宿主读回一个对象 —— 回退直链就指在这里 |

`ping` 回的是：

```json
{
  "ok": true, "plugin": "imagehost", "version": "0.7.1", "port": 5273,
  "upload_url": "http://127.0.0.1:5273/plugin/imagehost/upload",
  "upload_path": "/plugin/imagehost/upload",
  "accept": "POST multipart/form-data（字段名 file / image），或直接把图片字节当 body 发过来",
  "max_mb": 20, "max_bytes": 20971520,
  "compress": true, "short": false,
  "remote": "local", "dir": "image_bed"
}
```

`accept` 这句跟着宿主界面语言走，英文界面回的就是英文那句。

`upload` 回的是：

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

- **请求体**。裸图片字节、`multipart/form-data`（字段名 `file` 或 `image`）、JSON（`{"filename":…,"data_b64":…}`）都收。文件名先看 `?name=`，再看 multipart 那一段。
- **大图走裸字节**。multipart 由宿主自己拆、每个字段有大小上限，所以大截图必须把字节直接当 body 发、名字放 `?name=` —— `image-upload-rcgui` 就是这么发的。
- **体积上限**。超过 `max_mb` 直接回 413；开着「超限先压缩」时，插件先解码、在沙箱里重新编码成 JPEG 再传，并在回包里给 `compressed: true` 和一句 `note`。
- **去重**。开着「去重」且目录里已有同名对象时，直接返回已有的那条链接，`deduped: true`；关掉的话，新对象存成 `名字 (1).png`。
- **短链失败不影响上传**。模板那一步出错时 `short_url` 是 `null`，`short_error` 说明原因，直链照样能用。
- **代理直链需要宿主在跑**。`/plugin/imagehost/i/...` 每次请求都要去远程读，所以只有 RCGUI 运行时才有效 —— 用站点型远程就完全绕开这一点。

## 开发

需要 Rust 1.80 以上和 `wasm32-wasip1` 目标：

```powershell
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1
pwsh -File scripts/pack.ps1
```

在 bash 里改用 `bash scripts/pack.sh`，两个脚本行为一致。打包会构建 wasm、组装 `dist/<id>-<版本>/`（`config.json`、`main.wasm`、`check.sha256`）、打成 zip，并打印压缩包的 SHA-256。构建时会把 cargo home 前缀重写掉，所以本机路径不会留在 `main.wasm` 里。

```
config.json                     身份、能力、http_entry 和整张面板
src/main.rs                     插件本体，一个文件
src/i18n.json                   英文词表，用中文原文当 key
src/i18n.rs                     查表函数，以及保证词表完整的测试
Cargo.toml                      wasm 构建设置与依赖
scripts/pack.ps1                构建、组装 dist/<id>-<版本>、打印 sha256
scripts/pack.sh                 同一套流程的 bash 版，CI 用它
.github/workflows/release.yml   打 tag → 校验版本 → 构建 → 发 GitHub Release
```

## 发版

1. 改 `config.json` 和 `Cargo.toml` 里的 `version`；tag 与 `config.json` 对不上时工作流会拒绝。
2. 打 tag 并推：`git tag v0.7.1`，然后 `git push origin v0.7.1`。
3. CI 构建并挂上 `imagehost-0.7.1.zip`，同时打印它的 SHA-256。
4. 把这个 tag、包名和 SHA-256 抄进宿主仓库的 `plugins/manifest.json`，下次构建 RCGUI 就会用上新版本。

不要手工 `gh release create`：Release 和资产都由工作流自己创建、上传。

## 插件协议

- 宿主把 `main.wasm` 当 WASI 命令启动，每轮从 stdin 读一条 JSON 请求。
- 一轮要么回正常载荷，要么回 `{"need":[{"cap":…,"params":…}]}` 向宿主申请能力调用；宿主答完这一轮继续。
- 声明的能力：`kv`、`ui`、`fs`、`http`、`rclone`、`env` —— 每一项用来做什么见[权限](#权限)。
- 面板状态存在插件的 `kv` 里 —— `on_start` 跑 `state`，`on_exit` 跑 `exit`。
- 所有给用户看的文字都过一次按中文原文查的表，所以未知动作回的是 `未知动作：<名字>`（英文界面则是 `Unknown action: <name>`），而不是让这一轮崩掉。

## 已知限制

- 直链只在远程本身公开时对外有效；私有远程拿到的是代理链接，需要 RCGUI 在运行。
- 代理链接里明写着 `<远程>:<路径>`，所以在远程上改名或移动对象，会让已经发出去的链接失效。
- 短链只有模板一这一种服务商形态，别的得改代码。
- 超限压缩跑在 WASM 沙箱里、单线程，很大的图会明显慢。

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
