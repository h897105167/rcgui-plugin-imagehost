# rcgui-plugin-imagehost

**The image-host plugin for RCGUI** — send a local image to an rclone remote, get a stable direct link back, and shorten it if you want.

English | [简体中文](README.zh-CN.md)

> Host app: [h897105167/rclone_gui_windows](https://github.com/h897105167/rclone_gui_windows)
> Plugin development guide: [docs/PLUGIN.md](https://github.com/h897105167/rclone_gui_windows/blob/main/docs/PLUGIN.md)

This repository holds the plugin and nothing else: the Rust source, `config.json`, the packaging scripts and the release workflow. Every control you see in RCGUI is declared by `plugin_setting` in `config.json`, and the host draws the panel — there is no HTML, no front-end bundle, no JavaScript.

`config.json` also sets `"http_entry": true`, which makes the host hand `/plugin/imagehost/...` to this plugin. That is the door external clients use: the plugin answers a `ping` probe and accepts uploads over plain HTTP, so anything that can send a `POST` gets a link back without knowing rclone at all.

## Features

| Area | What you get |
| --- | --- |
| Pick and upload | choose a remote, type a directory or browse to it, pick a local file — the plugin uploads it through rclone |
| Direct link | site-type remotes such as K-Vault return their own public URL; plain remotes fall back to a link proxied by the host |
| Short link | optional 模板一 conversion: request URL plus `url` and `apikey`, `GET` or `POST`, plain text / `JSON` / `JSONP` |
| Dedupe | a same-named object in the upload directory either cancels the upload, or gets suffixed to `名字 (1).png` |
| Size cap | `max_mb` (20 MB by default); past the cap the image is re-encoded to JPEG, or the upload is refused with 413 |
| HTTP upload service | `ping` for probing, `upload` for uploading — this is what the Obsidian plugin talks to |
| Copy buttons | `复制直链` / `复制短链` put the last result on the clipboard |

## Install

- **Bundled.** RCGUI downloads the release archive named by `plugins/manifest.json`, verifies its SHA-256, unpacks it into `crates/app/assets/plugins/imagehost/` at build time, and copies it into `%APPDATA%\RCGUI\plugins\imagehost\` on first start. You do not install anything by hand.
- **Manual.** Unpack `imagehost-<version>.zip` into `%APPDATA%\RCGUI\plugins\imagehost\`. The archive must contain `config.json`, `main.wasm` and `check.sha256`.
- **Removal.** Disabling the plugin in RCGUI keeps the package and its settings; uninstalling deletes both, including the `kv` store that holds your remote, directory and token.

## Panel

The panel is one form, declared top to bottom in `config.json`. Labels are still Chinese-only in this repository — the host can localise them with `{"zh":…,"en":…}` pairs (see `docs/PLUGIN.md` §2.1.1), but this plugin has not been migrated yet.

| Key | Control | Label | What it does |
| --- | --- | --- | --- |
| `g-title` | group | 图床工具（上传 → 直链 → 短链） | section heading for the whole form |
| `remote` | select | 存储的远程 | the rclone remote to store into; the list is read from rclone |
| `refresh-remotes` | action | 刷新远程 | re-read the remote list from rclone |
| `path` | path | 上传目录 | directory inside the remote; empty means the remote root |
| `upload_path` | text | 上传图片路径 | absolute path of the local image — typed here or filled by `选择文件…` |
| `pick` | action | 选择文件… | host file dialog, filtered to `png jpg jpeg gif webp bmp` |
| `dedupe` | bool | 去重 | on by default: a same-named object cancels the upload instead of being overwritten |
| `dedupe-tip` | hint | — | explains `去重` and the `名字 (1).png` fallback when it is off |
| `short_tpl` | select | 短链模板 | `不选择` or `模板一`; `模板一` is the only built-in shape |
| `g-tpl1` | group | 模板一参数 | parameters of the short-link template |
| `short_fmt` | select | 返回格式 | `无` (plain text), `JSON` or `JSONP` |
| `short_method` | select | 请求方式 | `GET` or `POST` |
| `short_rate` | number | 每秒上限 | requests per second cap, 4 by default |
| `short_callback` | text | JSONP 回调 | callback name used for `JSONP`, `callback` by default |
| `short_url` | text | 请求地址 | endpoint of the short-link service |
| `short_key` | password | api_key | sent with every request; required by `模板一` |
| `short-tip` | hint | — | recaps the parameters, then points at `保存设置` |
| `g-http` | group | HTTP 上传服务（给外部客户端） | the upload endpoint used by external clients |
| `max_mb` | number | 体积上限（MB） | maximum accepted size, 20 MB by default |
| `compress` | bool | 超限先压缩 | on by default: past the cap the image is re-encoded to JPEG before upload |
| `detect-port` | action | 检测服务 | probes `127.0.0.1` for the host's HTTP port and fills `上传地址` |
| `http_url` | text | 上传地址 | filled by `检测服务`; a display convenience, not a setting |
| `http-tip` | hint | — | explains the port, `ping`, `upload`, compression and the short-link fallback |
| `run` | action | 生成直链 | primary action: upload the image and produce the link |
| `save` | action | 保存设置 | write the whole form into the plugin's `kv` store |
| `copy-link` | action | 复制直链 | copy `直链` to the clipboard |
| `last_link` | text | 直链 | the last direct link; also written by `run` |
| `copy-short` | action | 复制短链 | copy `短链` to the clipboard |
| `last_short` | text | 短链 | the last short link, empty when the template failed |
| `state` | status | 状态 | lamp showing the result of the last action |
| `link-tip` | hint | — | where the direct link comes from: site remote, or the host's proxy |

## HTTP API

The host's HTTP server listens on `127.0.0.1:<port>` (Settings → port, `5273` by default) and routes everything under `/plugin/imagehost` here.

| Route | Purpose |
| --- | --- |
| `GET /plugin/imagehost/ping` | probe: who answers here, what the limits are, which remote and directory are selected |
| `POST /plugin/imagehost/upload` | upload one image, get `url` and `short_url` back |
| `GET /plugin/imagehost/i/<remote>:<path>` | read an object back through the host — the fallback direct link points here |

`ping` answers with:

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

`upload` answers with:

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

- **Body.** Raw image bytes, `multipart/form-data` (a field named `file` or `image`) and JSON are all accepted. The file name comes from `?name=` first, then from the multipart part.
- **Multipart is small.** The host caps a multipart field at about 2 MB, so a large image has to be posted as raw bytes — that is exactly what `image-upload-rcgui` does.
- **Size cap.** Past `max_mb` the request is refused with 413, unless `超限先压缩` is on: then the plugin decodes, re-encodes to JPEG inside the sandbox and uploads that, reporting `compressed: true` plus a `note`.
- **Dedupe.** With `去重` on and a same-named object already in the directory, the existing link is returned with `deduped: true`. With it off, the new object is stored as `名字 (1).png`.
- **Short links never fail the upload.** When the template call goes wrong, `short_url` is `null` and `short_error` explains why; the direct link is still good.
- **Proxied links need the host.** `/plugin/imagehost/i/...` reads the object from the remote on every request, so it only works while RCGUI runs — a site-type remote avoids that entirely.

## Development

Rust 1.80 or newer and the `wasm32-wasip1` target:

```powershell
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1
pwsh -File scripts/pack.ps1
```

On a bash shell use `bash scripts/pack.sh` instead; both scripts behave the same. Packaging builds the wasm, assembles `dist/<id>-<version>/` with `config.json`, `main.wasm` and `check.sha256`, zips it, and prints the SHA-256 of the archive. The build rewrites the cargo home prefix out of the binary so no local path ends up inside `main.wasm`.

```
config.json                     identity, capabilities, http_entry and the whole panel
src/main.rs                     the plugin, in one file
Cargo.toml                      wasm build settings and dependencies
scripts/pack.ps1                build, assemble dist/<id>-<version>, print sha256
scripts/pack.sh                 the same for bash and CI
.github/workflows/release.yml   tag -> verify version -> build -> GitHub Release
```

## Releasing

1. Bump `version` in `config.json` and `Cargo.toml`; the workflow refuses a tag that disagrees with `config.json`.
2. Tag and push: `git tag v0.7.0` then `git push origin v0.7.0`.
3. CI builds and attaches `imagehost-0.7.0.zip` to the release, and prints its SHA-256.
4. Copy that tag, asset name and SHA-256 into `plugins/manifest.json` in the host repository, so the next RCGUI build picks the new version up.

Do not run `gh release create` by hand: the workflow creates the release and uploads the asset itself.

## Plugin protocol

- The host starts `main.wasm` as a WASI command and writes one JSON request per round to stdin.
- A round answers either with a normal payload, or with `{"need":[{"cap":…,"params":…}]}` to ask the host for a capability call; the host replies and the round continues.
- Declared capabilities: `kv`, `ui`, `fs`, `http`, `rclone`, `env`.
- Panel state lives in the plugin's `kv` store — `on_start` runs `state`, `on_exit` runs `exit`.
- An unknown action answers `未知动作：<name>` instead of aborting the round.

## Known limitations

- A direct link is only public when the remote itself is public; a private remote gets the proxied link, which needs RCGUI running.
- A proxied link spells out `<remote>:<path>`, so renaming or moving the object on the remote breaks links already shared.
- `模板一` is the only short-link provider shape; anything else needs code.
- Over-cap compression runs in the WASM sandbox on a single thread, so a very large image takes a while.
- Panel labels are Chinese-only for now, as noted above.

## License

MIT — see [LICENSE](LICENSE).
