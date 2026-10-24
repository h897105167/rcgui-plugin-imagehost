//! imagehost 插件 v0.5.0：图床（上传 → 直链 → 短链）
//!
//! 这是 rclone GUI 0.2.0 里那块「图床工具（上传 → 直链 → 短链）」原生面板的
//! 独立插件版：界面完全由 `config.json` 的 `plugin_setting.controls` 声明，
//! 逻辑全在 wasm 里，本体没有任何插件专属代码。
//!
//! 协议：宿主每次调用启动一次 WASI 命令，写一个 JSON 到 stdin，读一个 JSON 从
//! stdout。没有常驻状态 —— 轮次之间靠 `state` 往返传递（见 `docs/PLUGIN.md` §5）。
//!
//! 关键点：要拿直链，上传必须用 **need** 调 `rclone.upload_file`
//! （`ops` 里的同一个名字是即发即忘的，返回值会被丢掉）；K-Vault 型远端回
//! `{link,name,deduped}`，普通远端回 `{}`，那时退回本机服务的代理直链。

use std::io::{Read, Write};

use serde_json::{json, Map, Value};

const PLUGIN_ID: &str = "imagehost";
/// 配置在插件 kv 里的键。
const CFG_KEY: &str = "cfg";
/// 0.2.0 / 0.4.2 写的老键：首轮若 `cfg` 不存在就从这里迁移，别让用户重填一遍。
const LEGACY_KEY: &str = "bed";
/// 路径历史保留条数。
const HISTORY_MAX: usize = 8;
/// 读本机图片的上限（0.4.2 也是 32 MB）。
const MAX_FILE: u64 = 32 * 1024 * 1024;
/// 本机代理直链的前缀：宿主把 `/plugin/imagehost/**` 转给本插件。
const ROUTE_PREFIX: &str = "/i/";

fn main() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return;
    }
    let input: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let out = dispatch(&input);
    let text = serde_json::to_string(&out).unwrap_or_else(|_| "{}".to_string());
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

fn dispatch(input: &Value) -> Value {
    let id = input.get("id").cloned().unwrap_or(json!(0));
    let method = s(input.get("method"));
    if method == "manifest" {
        return json!({
            "id": id,
            "manifest": {
                "id": PLUGIN_ID,
                "name": "图床",
                "version": env!("CARGO_PKG_VERSION"),
                "description": "基于 rclone 的图床：上传图片拿直链，可再换短链。",
            }
        });
    }
    let params = input.get("params").cloned().unwrap_or_else(|| json!({}));
    handle_request(id, &params)
}

// ---------------------------------------------------------------- 协议小工具

fn need(cap: &str, key: &str, params: Value) -> Value {
    json!({ "cap": cap, "key": key, "params": params })
}

fn s(v: Option<&Value>) -> String {
    v.map(vstr).unwrap_or_default()
}

fn vstr(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 宿主把失败的能力调用写成 `{"_error": "…"}`（见 PLUGIN.md §5.5）。
fn cap_err(v: &Value) -> Option<String> {
    v.get("_error")
        .map(vstr)
        .filter(|e| !e.trim().is_empty())
}

/// 有的能力会把真正的结果再包一层 `{"result": …}`。
fn unwrap_result(v: &Value) -> Value {
    match v.get("result") {
        Some(r) if !r.is_null() => r.clone(),
        _ => v.clone(),
    }
}

/// 列表类返回值：裸数组，或 `{"list":[…]}` / `{"remotes":[…]}` / `{"paths":[…]}`。
fn as_list(v: &Value) -> Vec<Value> {
    let v = unwrap_result(v);
    if let Some(a) = v.as_array() {
        return a.clone();
    }
    for key in ["list", "remotes", "paths", "dirs", "items"] {
        if let Some(a) = v.get(key).and_then(|x| x.as_array()) {
            return a.clone();
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------- 配置

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Cfg {
    remote: String,
    path: String,
    upload_path: String,
    /// 0 = 不选择，1 = 模板一。
    short_tpl: u32,
    /// 0 = 无（服务端直接回纯文本），1 = JSON，2 = JSONP。
    short_fmt: u32,
    short_callback: String,
    short_url: String,
    /// 0 = GET，1 = POST。
    short_method: u32,
    /// 每秒最多几次短链请求。
    short_rate: u32,
    short_key: String,
    last_link: String,
    last_short: String,
    dedupe: bool,
    path_history: Vec<String>,
    /// 用户自填的直链前缀（0.2.0 的「公网地址」）；留空则用本机服务地址。
    #[serde(alias = "lastlink")]
    base: String,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            remote: String::new(),
            path: String::new(),
            upload_path: String::new(),
            short_tpl: 0,
            short_fmt: 0,
            short_callback: "callback".to_string(),
            short_url: "http://api.ft12.com/api.php".to_string(),
            short_method: 0,
            short_rate: 4,
            short_key: String::new(),
            last_link: String::new(),
            last_short: String::new(),
            dedupe: true,
            path_history: Vec::new(),
            base: String::new(),
        }
    }
}

/// 从 `provided.kv` 里取配置；`cfg` 不在就用老键 `bed` 迁移。
fn load_cfg(provided: &Value) -> (Cfg, bool) {
    let kv = provided.get("kv").cloned().unwrap_or_else(|| json!({}));
    for key in [CFG_KEY, LEGACY_KEY] {
        if let Some(v) = kv.get(key) {
            if v.is_object() {
                if let Ok(c) = serde_json::from_value::<Cfg>(v.clone()) {
                    return (c, key == LEGACY_KEY);
                }
            }
        }
    }
    (Cfg::default(), true)
}

fn cfg_from_state(state: &Value) -> Option<Cfg> {
    state
        .get("cfg")
        .and_then(|v| serde_json::from_value::<Cfg>(v.clone()).ok())
}

/// 表单 → 配置。表单值是字符串，select 回的是选项文字，所以两种写法都认。
fn apply_form(cfg: &mut Cfg, form: &Map<String, Value>) {
    let get = |k: &str| form.get(k).map(vstr);
    if let Some(v) = get("remote") {
        cfg.remote = v.trim().to_string();
    }
    if let Some(v) = get("path") {
        cfg.path = norm_dir(&v);
    }
    if let Some(v) = get("upload_path") {
        cfg.upload_path = v.trim().to_string();
    }
    if let Some(v) = form.get("dedupe") {
        cfg.dedupe = truthy(v);
    }
    if let Some(v) = get("short_tpl") {
        cfg.short_tpl = choice(&v, &["不选择", "模板一"], cfg.short_tpl).min(1);
    }
    if let Some(v) = get("short_fmt") {
        cfg.short_fmt = choice(&v, &["无", "JSON", "JSONP"], cfg.short_fmt).min(2);
    }
    if let Some(v) = get("short_method") {
        cfg.short_method = choice(&v, &["GET", "POST"], cfg.short_method).min(1);
    }
    if let Some(v) = get("short_callback") {
        cfg.short_callback = v.trim().to_string();
    }
    if let Some(v) = get("short_url") {
        cfg.short_url = v.trim().to_string();
    }
    if let Some(v) = get("short_key") {
        cfg.short_key = v.trim().to_string();
    }
    if let Some(v) = get("short_rate") {
        cfg.short_rate = v.trim().parse::<u32>().unwrap_or(cfg.short_rate).max(1);
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        other => {
            let t = vstr(other);
            !matches!(t.trim(), "" | "0" | "false" | "no" | "off")
        }
    }
}

/// `"模板一"` → 1；`"1"` → 1；认不出来就用旧值。
fn choice(text: &str, labels: &[&str], old: u32) -> u32 {
    let t = text.trim();
    for (i, l) in labels.iter().enumerate() {
        if t.eq_ignore_ascii_case(l) {
            return i as u32;
        }
    }
    t.parse::<u32>().unwrap_or(old)
}

fn values_of(cfg: &Cfg) -> Value {
    let fmt_label = match cfg.short_fmt {
        1 => "JSON",
        2 => "JSONP",
        _ => "无",
    };
    json!({
        "remote": cfg.remote,
        "path": cfg.path,
        "upload_path": cfg.upload_path,
        "dedupe": if cfg.dedupe { "true" } else { "false" },
        "short_tpl": if cfg.short_tpl == 1 { "模板一" } else { "不选择" },
        "short_fmt": fmt_label,
        "short_callback": cfg.short_callback,
        "short_method": if cfg.short_method == 1 { "POST" } else { "GET" },
        "short_rate": cfg.short_rate.to_string(),
        "short_url": cfg.short_url,
        "short_key": cfg.short_key,
        "last_link": cfg.last_link,
        "last_short": cfg.last_short,
    })
}

// ---------------------------------------------------------------- 面板返回

/// 交回面板：`values` 回填字段，`choices` 给下拉候选，`status` + `lamp` 写状态行。
fn panel(
    id: &Value,
    ops: Vec<Value>,
    cfg: &Cfg,
    status: &str,
    lamp: &str,
    choices: Option<Value>,
) -> Value {
    let mut body = Map::new();
    body.insert("values".to_string(), values_of(cfg));
    body.insert("status".to_string(), json!(status));
    body.insert("lamp".to_string(), json!(lamp));
    if let Some(c) = choices {
        body.insert("choices".to_string(), c);
    }
    json!({
        "id": id,
        "ops": ops,
        "result": { "status": 200, "body": Value::Object(body) }
    })
}

fn err_panel(id: &Value, cfg: &Cfg, status: &str) -> Value {
    panel(id, Vec::new(), cfg, status, "err", None)
}

fn save_op(cfg: &Cfg) -> Value {
    json!({ "op": "kv.set", "key": CFG_KEY, "value": cfg })
}

/// 下一轮：`state` 带上动作、进度、表单与已解析的配置。
fn next(
    id: &Value,
    needs: Vec<Value>,
    action: &str,
    step: u64,
    form: &Map<String, Value>,
    cfg: &Cfg,
    work: &Map<String, Value>,
) -> Value {
    json!({
        "id": id,
        "need": needs,
        "state": {
            "action": action,
            "step": step,
            "form": Value::Object(form.clone()),
            "cfg": cfg,
            "work": Value::Object(work.clone()),
        }
    })
}

// ---------------------------------------------------------------- 请求入口

fn handle_request(id: Value, params: &Value) -> Value {
    // HTTP 入口（`/plugin/imagehost/i/…`）：直接回图片字节。
    if let Some(http) = params.get("http").filter(|v| v.is_object()) {
        return serve_image(id, params, http);
    }

    let action = s(params.get("action"));
    if action == "exit" || action == "shutdown" {
        return json!({
            "id": id,
            "ops": [],
            "result": { "status": 200, "body": { "status": "已关闭", "lamp": "ok" } }
        });
    }

    let provided = params.get("provided").cloned().unwrap_or(Value::Null);
    let first = provided.is_null()
        || provided
            .as_object()
            .map(|o| o.is_empty())
            .unwrap_or(true);
    let prev = params.get("state").cloned().unwrap_or(Value::Null);
    let form: Map<String, Value> = params
        .get("form")
        .and_then(|v| v.as_object())
        .cloned()
        .or_else(|| prev.get("form").and_then(|v| v.as_object()).cloned())
        .unwrap_or_default();

    if first {
        // 一律先要一次全部 kv：配置、迁移和老面板的候选都从它出发。
        let state = json!({ "action": action, "step": 0, "form": Value::Object(form) });
        return json!({
            "id": id,
            "need": [need("kv.get_all", "kv", json!({}))],
            "state": state
        });
    }

    let step = prev.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let work: Map<String, Value> = prev
        .get("work")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let (mut cfg, migrated) = match cfg_from_state(&prev) {
        Some(c) => (c, false),
        None => load_cfg(&provided),
    };
    // 表单是用户当前看到的样子，先并进来（`browse`/`pick` 之外的动作都不再写回）。
    apply_form(&mut cfg, &form);

    match action.as_str() {
        "state" | "refresh-remotes" => act_remotes(id, &action, step, &form, cfg, &provided, &work, migrated),
        "browse" => act_browse(id, &action, step, &form, cfg, &provided, &work),
        "pick" => act_pick(id, &action, step, &form, cfg, &provided, &work),
        "save" => {
            let cur = cfg.path.clone();
            remember_path(&mut cfg, &cur);
            let ops = vec![save_op(&cfg)];
            panel(&id, ops, &cfg, "设置已保存", "ok", None)
        }
        "copy-link" => act_copy(id, cfg, false),
        "copy-short" => act_copy(id, cfg, true),
        "run" | "upload" => act_run(id, &action, step, &form, cfg, &provided, &work),
        other => err_panel(&id, &cfg, &format!("未知动作：{other}")),
    }
}

// ---------------------------------------------------------------- 各动作

fn act_remotes(
    id: Value,
    action: &str,
    step: u64,
    form: &Map<String, Value>,
    cfg: Cfg,
    provided: &Value,
    work: &Map<String, Value>,
    migrated: bool,
) -> Value {
    if step == 0 {
        return next(
            &id,
            vec![need("rclone.remotes", "remotes", json!({}))],
            action,
            1,
            form,
            &cfg,
            work,
        );
    }
    let remotes_list = as_list(provided.get("remotes").unwrap_or(&Value::Null));
    let remotes = remotes_list.iter().map(|v| {
        // rclone 偶尔回 "名字:类型"，面板只认裸名字。
        let raw = vstr(v);
        raw.split(':').next().unwrap_or("").trim().to_string()
    });
    let mut list: Vec<String> = Vec::new();
    if !cfg.remote.trim().is_empty() {
        list.push(cfg.remote.trim().to_string());
    }
    for r in remotes {
        if !r.is_empty() && !list.contains(&r) {
            list.push(r);
        }
    }
    let status = if list.len() > 1 {
        format!("共 {} 个远程", list.len())
    } else if list.is_empty() {
        "没读到任何远程：先在 rclone 配置里加一个".to_string()
    } else {
        format!("当前远程：{}", list[0])
    };
    let mut ops = Vec::new();
    if migrated {
        ops.push(save_op(&cfg));
    }
    let choices = json!({
        "remote": list,
        "path": path_choices(&cfg),
    });
    let lamp = if list.is_empty() { "warn" } else { "ok" };
    panel(&id, ops, &cfg, &status, lamp, Some(choices))
}

/// `path` 字段的候选：当前目录 + 历史 + 上层。
fn path_choices(cfg: &Cfg) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let cur = norm_dir(&cfg.path);
    if !cur.is_empty() {
        out.push(cur.clone());
    }
    for h in &cfg.path_history {
        if !out.contains(h) {
            out.push(h.clone());
        }
    }
    if !cur.is_empty() {
        if let Some(p) = cur.rsplit_once('/') {
            let parent = p.0.to_string();
            if !parent.is_empty() && !out.contains(&parent) {
                out.push(parent);
            }
        }
    }
    out
}

fn act_browse(
    id: Value,
    action: &str,
    step: u64,
    form: &Map<String, Value>,
    cfg: Cfg,
    provided: &Value,
    work: &Map<String, Value>,
) -> Value {
    if cfg.remote.trim().is_empty() {
        return err_panel(&id, &cfg, "先选择要浏览的远程");
    }
    let cur = norm_dir(&cfg.path);
    if step == 0 {
        return next(
            &id,
            vec![need(
                "rclone.call",
                "list",
                json!({
                    "method": "operations/list",
                    "params": { "fs": format!("{}:", cfg.remote), "remote": cur, "no_modtime": true }
                }),
            )],
            action,
            1,
            form,
            &cfg,
            work,
        );
    }
    let mut dirs: Vec<String> = Vec::new();
    for it in as_list(provided.get("list").unwrap_or(&Value::Null)) {
        let is_dir = it
            .get("IsDir")
            .or_else(|| it.get("isDir"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !is_dir {
            continue;
        }
        let name = s(it.get("Name"));
        if !name.is_empty() {
            dirs.push(join_dir(&cur, &name));
        }
    }
    dirs.sort();
    let mut list = path_choices(&cfg);
    for d in dirs.iter() {
        if !list.contains(d) {
            list.push(d.clone());
        }
    }
    let status = if dirs.is_empty() {
        format!("「{}」下没有子目录", if cur.is_empty() { "/" } else { cur.as_str() })
    } else {
        format!("「{}」下有 {} 个子目录", if cur.is_empty() { "/" } else { cur.as_str() }, dirs.len())
    };
    let choices = json!({ "path": list });
    panel(&id, Vec::new(), &cfg, &status, "ok", Some(choices))
}

fn act_pick(
    id: Value,
    action: &str,
    step: u64,
    form: &Map<String, Value>,
    mut cfg: Cfg,
    provided: &Value,
    work: &Map<String, Value>,
) -> Value {
    if step == 0 {
        let needs = vec![need(
            "fs.pick_file",
            "pick",
            json!({
                "title": "选择要上传的图片",
                "filters": ["png", "jpg", "jpeg", "gif", "webp", "bmp"],
            }),
        )];
        return next(&id, needs, action, 1, form, &cfg, work);
    }
    let picked = provided.get("pick").cloned().unwrap_or(Value::Null);
    let path = s(picked.get("path"));
    if path.is_empty() {
        return panel(&id, Vec::new(), &cfg, "没有选择文件", "warn", None);
    }
    cfg.upload_path = path;
    let ops = vec![save_op(&cfg)];
    let status = format!("已选择：{}", cfg.upload_path);
    panel(&id, ops, &cfg, &status, "ok", None)
}

fn act_copy(id: Value, cfg: Cfg, short: bool) -> Value {
    let text = if short {
        cfg.last_short.trim().to_string()
    } else {
        cfg.last_link.trim().to_string()
    };
    if text.is_empty() {
        let msg = if short { "还没有短链" } else { "还没有直链" };
        return panel(&id, Vec::new(), &cfg, msg, "warn", None);
    }
    let ops = vec![json!({ "op": "ui.copy", "text": text })];
    let msg = if short { "已复制短链" } else { "已复制直链" };
    panel(&id, ops, &cfg, msg, "ok", None)
}

// ---------------------------------------------------------------- 生成直链

fn act_run(
    id: Value,
    action: &str,
    step: u64,
    form: &Map<String, Value>,
    cfg: Cfg,
    provided: &Value,
    work: &Map<String, Value>,
) -> Value {
    let dir = norm_dir(&cfg.path);
    match step {
        0 => {
            if cfg.upload_path.trim().is_empty() {
                return err_panel(&id, &cfg, "先在「上传图片路径」里选一张本机图片");
            }
            if cfg.remote.trim().is_empty() {
                return err_panel(&id, &cfg, "先选择要上传到的远程");
            }
            let needs = vec![
                need(
                    "fs.read_b64",
                    "file",
                    json!({ "path": cfg.upload_path.trim(), "max_bytes": MAX_FILE }),
                ),
                need(
                    "rclone.call",
                    "list",
                    json!({
                        "method": "operations/list",
                        "params": { "fs": format!("{}:", cfg.remote), "remote": dir, "no_modtime": true }
                    }),
                ),
                need("env.info", "env", json!({})),
            ];
            next(&id, needs, action, 1, form, &cfg, work)
        }
        1 => {
            let file = provided.get("file").cloned().unwrap_or(Value::Null);
            if let Some(e) = cap_err(&file) {
                return err_panel(&id, &cfg, &format!("读取图片失败：{e}"));
            }
            let data_b64 = s(file.get("data_b64"));
            if data_b64.is_empty() {
                return err_panel(&id, &cfg, "读到的图片是空的");
            }
            let base_name = file_name(&cfg.upload_path);
            if base_name.is_empty() {
                return err_panel(&id, &cfg, "看不出文件名，检查「上传图片路径」");
            }
            let mut existing: Vec<String> = Vec::new();
            for it in as_list(provided.get("list").unwrap_or(&Value::Null)) {
                let is_dir = it
                    .get("IsDir")
                    .or_else(|| it.get("isDir"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_dir {
                    continue;
                }
                let name = s(it.get("Name"));
                if !name.is_empty() {
                    existing.push(name);
                }
            }
            let (stem, ext) = split_ext(&base_name);
            let mut final_name = base_name.clone();
            if existing.iter().any(|n| n.eq_ignore_ascii_case(&base_name)) {
                if cfg.dedupe {
                    let shown = join_dir(&dir, &base_name);
                    return err_panel(
                        &id,
                        &cfg,
                        &format!("「{shown}」已经存在这份文件，按「去重」取消上传（要留两份就取消勾选「去重」）"),
                    );
                }
                final_name = resolve_conflict(&existing, &stem, &ext);
            }
            let mut work = work.clone();
            work.insert("name".to_string(), json!(final_name));
            if let Some(env) = provided.get("env") {
                work.insert("env".to_string(), env.clone());
            }
            let needs = vec![need(
                "rclone.upload_file",
                "up",
                json!({
                    "fs": format!("{}:{}", cfg.remote, dir),
                    "remote_path": final_name,
                    "filename": final_name,
                    "data_b64": data_b64,
                }),
            )];
            next(&id, needs, action, 2, form, &cfg, &work)
        }
        2 => {
            let up = provided.get("up").cloned().unwrap_or(Value::Null);
            if let Some(e) = cap_err(&up) {
                return err_panel(&id, &cfg, &format!("上传失败：{e}"));
            }
            let name = s(work.get("name"));
            let stored = join_dir(&dir, &name);
            let deduped = up.get("deduped").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut work = work.clone();
            work.insert("stored".to_string(), json!(stored));
            work.insert("deduped".to_string(), json!(deduped));
            let link = s(up.get("link")).trim().to_string();
            if !link.is_empty() {
                work.insert("link".to_string(), json!(link));
                work.insert("source".to_string(), json!("站点"));
                return after_link(&id, action, form, &cfg, &work);
            }
            // 普通远程没有直链：借本机服务的端口拼一条代理直链。
            let env = work.get("env").cloned().unwrap_or(Value::Null);
            let data_dir = s(env.get("data_dir"));
            if data_dir.is_empty() {
                work.insert("origin".to_string(), json!(""));
                return after_link(&id, action, form, &cfg, &work);
            }
            let host_cfg = format!("{data_dir}\\config.json");
            let needs = vec![need(
                "fs.read_text",
                "host_cfg",
                json!({ "path": host_cfg, "max_bytes": 1048576 }),
            )];
            next(&id, needs, action, 3, form, &cfg, &work)
        }
        3 => {
            let raw = provided.get("host_cfg").cloned().unwrap_or(Value::Null);
            let text = raw
                .get("text")
                .map(vstr)
                .unwrap_or_else(|| vstr(&unwrap_result(&raw)));
            let port = json_port(&text);
            let origin = if port > 0 {
                format!("http://127.0.0.1:{port}")
            } else {
                String::new()
            };
            let mut work = work.clone();
            work.insert("origin".to_string(), json!(origin));
            work.insert("source".to_string(), json!("本机代理"));
            after_link(&id, action, form, &cfg, &work)
        }
        4 => {
            let source = s(work.get("source"));
            finish_short(&id, cfg, provided, work, Some(source))
        }
        _ => err_panel(&id, &cfg, "状态丢了，重新点一次「生成直链」"),
    }
}

/// 直链有了（或确定拿不到）：该换短链就再插一轮，否则直接收尾。
fn after_link(
    id: &Value,
    action: &str,
    form: &Map<String, Value>,
    cfg: &Cfg,
    work: &Map<String, Value>,
) -> Value {
    let link = s(work.get("link"));
    let source = s(work.get("source"));
    let origin = s(work.get("origin"));
    let stored = s(work.get("stored"));
    let mut work = work.clone();

    // 没有站点直链时退回本机代理：`<origin>/plugin/imagehost/i/<remote>:<路径>`。
    let link = if !link.is_empty() {
        link
    } else if !origin.is_empty() {
        let mut base = cfg.base.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            base = origin.clone();
        }
        format!(
            "{base}/plugin/imagehost/i/{}:{}",
            cfg.remote,
            uri_path(&stored)
        )
    } else {
        String::new()
    };
    work.insert("link".to_string(), json!(link));

    if link.is_empty() {
        return err_panel(
            id,
            cfg,
            "文件传上去了，但拿不到直链：该远程不支持公开链接，也没取到本机服务端口",
        );
    }

    if cfg.short_tpl != 1 {
        return finish_upload(id, cfg.clone(), &work, Some(source));
    }
    match short_request(cfg, &link) {
        Ok((method, url, body, headers)) => {
            let mut params = Map::new();
            params.insert("method".to_string(), json!(method));
            params.insert("url".to_string(), json!(url));
            if !headers.is_empty() {
                params.insert("headers".to_string(), json!(headers));
            }
            if let Some(b) = body {
                params.insert("body".to_string(), json!(b));
            }
            params.insert("timeout_ms".to_string(), json!(15000));
            params.insert("max_bytes".to_string(), json!(1048576));
            next(
                id,
                vec![need("http.request", "short", Value::Object(params))],
                action,
                4,
                form,
                cfg,
                &work,
            )
        }
        Err(e) => {
            work.insert("short_err".to_string(), json!(e));
            finish_upload(id, cfg.clone(), &work, Some(source))
        }
    }
}

fn finish_upload(id: &Value, mut cfg: Cfg, work: &Map<String, Value>, source: Option<String>) -> Value {
    let link = s(work.get("link"));
    let short = s(work.get("short"));
    let stored = s(work.get("stored"));
    let deduped = work
        .get("deduped")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let short_err = s(work.get("short_err"));

    cfg.last_link = link.clone();
    if !short.is_empty() {
        cfg.last_short = short.clone();
    }
    let dir = dirname(&stored);
    remember_path(&mut cfg, &dir);

    let mut ops = vec![save_op(&cfg)];
    if short.is_empty() {
        ops.push(json!({
            "op": "ui.toast",
            "level": "info",
            "message": if short_err.is_empty() { "图片已上传，直链已生成" } else { "图片已上传，短链没换成" },
        }));
    } else {
        ops.push(json!({ "op": "ui.toast", "level": "info", "message": "图片已上传，直链与短链都已生成" }));
    }

    let mut status = format!("已上传：{stored}");
    if let Some(src) = source.filter(|s| !s.is_empty()) {
        status.push_str(&format!("（直链来源：{src}）"));
    }
    if deduped {
        status.push_str("；站点已有同一份内容，直接复用了旧文件");
    }
    if !short.is_empty() {
        status.push_str(&format!("；短链：{short}"));
    } else if cfg.short_tpl == 1 {
        if short_err.is_empty() {
            status.push_str("；短链模板没换成功，直链可用");
        } else {
            status.push_str(&format!("；短链失败：{short_err}"));
        }
    }
    let lamp = if short.is_empty() && cfg.short_tpl == 1 && !short_err.is_empty() {
        "warn"
    } else {
        "ok"
    };
    panel(id, ops, &cfg, &status, lamp, None)
}

/// `short` 请求回来了：解析出短链再收尾。
fn finish_short(
    id: &Value,
    cfg: Cfg,
    provided: &Value,
    work: &Map<String, Value>,
    source: Option<String>,
) -> Value {
    let resp = provided.get("short").cloned().unwrap_or(Value::Null);
    let mut work = work.clone();
    if let Some(e) = cap_err(&resp) {
        work.insert("short_err".to_string(), json!(e));
        return finish_upload(id, cfg, &work, source);
    }
    let code = resp.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
    let body = resp.get("body").map(vstr).unwrap_or_default();
    if !(200..300).contains(&(code as i32)) && code != 0 {
        let brief = first_line(&body);
        work.insert(
            "short_err".to_string(),
            json!(format!("短链服务返回 HTTP {code}：{brief}")),
        );
        return finish_upload(id, cfg, &work, source);
    }
    match parse_short(cfg.short_fmt, &body) {
        Ok(short) => {
            work.insert("short".to_string(), json!(short));
            finish_upload(id, cfg, &work, source)
        }
        Err(e) => {
            work.insert("short_err".to_string(), json!(e));
            finish_upload(id, cfg, &work, source)
        }
    }
}

// ---------------------------------------------------------------- 短链

/// 短链请求（模板一）：与 0.2.0 的 `short_request` 同一套参数。
fn short_request(
    cfg: &Cfg,
    direct: &str,
) -> Result<(String, String, Option<String>, Map<String, Value>), String> {
    let base = cfg.short_url.trim();
    if base.is_empty() {
        return Err("未填写短链请求地址".to_string());
    }
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("短链请求地址必须以 http:// 或 https:// 开头".to_string());
    }
    let key = cfg.short_key.trim();
    if key.is_empty() {
        return Err("短链模板一必须填写 api_key".to_string());
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    pairs.push(("url".to_string(), direct.to_string()));
    pairs.push(("apikey".to_string(), key.to_string()));
    match cfg.short_fmt {
        1 => pairs.push(("format".to_string(), "json".to_string())),
        2 => {
            pairs.push(("format".to_string(), "jsonp".to_string()));
            let cb = cfg.short_callback.trim();
            pairs.push((
                "callback".to_string(),
                if cb.is_empty() { "callback" } else { cb }.to_string(),
            ));
        }
        _ => {}
    }
    let encoded = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    if cfg.short_method == 1 {
        let mut headers = Map::new();
        headers.insert(
            "content-type".to_string(),
            json!("application/x-www-form-urlencoded"),
        );
        Ok(("POST".to_string(), base.to_string(), Some(encoded), headers))
    } else {
        let sep = if base.contains('?') { '&' } else { '?' };
        Ok((
            "GET".to_string(),
            format!("{base}{sep}{encoded}"),
            None,
            Map::new(),
        ))
    }
}

fn form_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn parse_short(fmt: u32, body: &str) -> Result<String, String> {
    let text = body.trim();
    if text.is_empty() {
        return Err("短链服务回了空响应".to_string());
    }
    match fmt {
        1 => parse_json_short(text),
        2 => {
            let inner = match (text.find('('), text.rfind(')')) {
                (Some(a), Some(b)) if b > a => text[a + 1..b].trim().to_string(),
                _ => text.to_string(),
            };
            parse_json_short(&inner)
        }
        // 「无」：服务端直接回纯文本链接，取第一行。
        _ => {
            let line = first_line(text);
            if line.is_empty() {
                Err("短链服务没给出链接".to_string())
            } else {
                Ok(line)
            }
        }
    }
}

fn parse_json_short(text: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("短链服务回的不是 JSON：{e}"))?;
    let v = unwrap_result(&v);
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    for key in ["url", "short", "short_url", "shorturl", "link", "data"] {
        if let Some(x) = v.get(key) {
            let t = vstr(x).trim().to_string();
            if !t.is_empty() {
                return Ok(t);
            }
        }
    }
    Err(format!("短链服务返回里没有链接：{}", first_line(text)))
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------- 直链代理

/// `/plugin/imagehost/i/<remote>:<路径>`：从远程读回图片字节。
fn serve_image(id: Value, params: &Value, http: &Value) -> Value {
    let path = s(http.get("path"));
    let rest = path.trim_start_matches('/');
    let rest = match rest.strip_prefix(ROUTE_PREFIX.trim_start_matches('/')) {
        Some(x) => x,
        None => return http_not_found(&id, "不是图片路径"),
    };
    let (remote, stored) = match rest.split_once(':') {
        Some((r, p)) => (r.to_string(), p.trim_start_matches('/').to_string()),
        None => return http_not_found(&id, "路径缺少「远程:文件」"),
    };
    if remote.is_empty() || stored.is_empty() || stored.contains("..") {
        return http_not_found(&id, "路径不合法");
    }

    let prev = params.get("state").cloned().unwrap_or(Value::Null);
    let step = prev.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let provided = params.get("provided").cloned().unwrap_or(Value::Null);

    if step == 0 {
        return json!({
            "id": id,
            "need": [need("rclone.cat", "img", json!({
                "remote": remote, "path": stored, "max_bytes": MAX_FILE,
            }))],
            "state": { "step": 1 }
        });
    }

    let img = provided.get("img").cloned().unwrap_or(Value::Null);
    if let Some(e) = cap_err(&img) {
        return http_not_found(&id, &e);
    }
    let b64 = s(img.get("data_b64"));
    if b64.is_empty() {
        return http_not_found(&id, "文件是空的");
    }
    let ct = {
        let given = s(img.get("content_type"));
        if given.is_empty() {
            format!("{}", guess_content_type(&stored))
        } else {
            given
        }
    };
    json!({
        "id": id,
        "ops": [],
        "result": { "status": 200, "content_type": ct, "body_b64": b64 }
    })
}

fn http_not_found(id: &Value, msg: &str) -> Value {
    json!({
        "id": id,
        "ops": [],
        "result": {
            "status": 404,
            // 正文里带错误原因：宿主只回 `body`/`body_b64`，`message` 会被丢掉，
            // 直接从浏览器打开链接时就能看见为什么 404。
            "body": { "error": msg }
        }
    })
}

fn guess_content_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// 本机服务端口：从 `<数据目录>\config.json` 的 `http.port` 里读。
fn json_port(text: &str) -> u16 {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let port = v
        .get("http")
        .and_then(|h| h.get("port"))
        .and_then(|p| p.as_u64())
        .unwrap_or(0);
    if port > 0 && port < 65536 {
        port as u16
    } else {
        0
    }
}

// ---------------------------------------------------------------- 路径工具

fn norm_dir(text: &str) -> String {
    let t = text.trim().replace('\\', "/");
    let t = t.trim_matches('/');
    if t == "." || t == "/" {
        String::new()
    } else {
        t.to_string()
    }
}

fn join_dir(dir: &str, name: &str) -> String {
    let dir = norm_dir(dir);
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn dirname(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((d, _)) => d.to_string(),
        None => String::new(),
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn split_ext(name: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), ext.to_string()),
        _ => (name.to_string(), String::new()),
    }
}

/// 「名字 (1).png」：只在本次要传的名字上让路，不碰历史里已有的序号。
fn resolve_conflict(existing: &[String], stem: &str, ext: &str) -> String {
    let dot = if ext.is_empty() {
        String::new()
    } else {
        format!(".{ext}")
    };
    for i in 1..1000 {
        let cand = format!("{stem} ({i}){dot}");
        if !existing.iter().any(|n| n.eq_ignore_ascii_case(&cand)) {
            return cand;
        }
    }
    format!("{stem} ({}){dot}", epoch_like())
}

fn epoch_like() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 逐段做百分号编码，斜杠保留。
fn uri_path(path: &str) -> String {
    path.split('/')
        .map(form_encode)
        .collect::<Vec<_>>()
        .join("/")
}

fn remember_path(cfg: &mut Cfg, dir: &str) {
    let d = norm_dir(dir);
    if d.is_empty() {
        return;
    }
    cfg.path_history.retain(|h| h != &d);
    cfg.path_history.insert(0, d);
    cfg.path_history.truncate(HISTORY_MAX);
}
