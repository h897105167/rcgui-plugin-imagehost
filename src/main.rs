//! imagehost 插件 v0.6.1：图床（上传 → 直链 → 短链）+ HTTP 上传服务
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

mod i18n;

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
/// 外部客户端上传图片的入口：`POST /plugin/imagehost/upload`。
const UPLOAD_ROUTE: &str = "upload";
/// 服务自述入口（客户端探测端口 / 上限用）：`GET /plugin/imagehost/ping`。
const PING_ROUTE: &str = "ping";
/// 图片大小上限的默认值（MB）。
const DEFAULT_MAX_MB: u32 = 20;
/// 压缩时最多缩到这个边长，再小就认输。
const MIN_SIDE: u32 = 64;

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
    // 宿主每轮都把界面语言放在 params.lang（`zh` / `en`）：插件自带译文，
    // 缺对应语言时 i18n 模块回落英文。老宿主不带这个字段，按中文处理。
    let lang = input
        .get("params")
        .and_then(|p| p.get("lang"))
        .and_then(|v| v.as_str());
    i18n::set_lang(lang);
    let id = input.get("id").cloned().unwrap_or(json!(0));
    let method = s(input.get("method"));
    if method == "manifest" {
        return json!({
            "id": id,
            "manifest": {
                "id": PLUGIN_ID,
                "name": i18n::t("图床"),
                "version": env!("CARGO_PKG_VERSION"),
                "description": i18n::t("基于 rclone 的图床：上传图片拿直链，可再换短链。"),
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
    /// HTTP 上传服务的图片大小上限（MB）：超过就压缩后再传。
    max_mb: u32,
    /// 超过上限时压缩（关掉就只报错，不动用户的图）。
    compress: bool,
    /// 上次探测到的宿主 HTTP 服务端口（0 = 还没探测过）。
    http_port: u16,
    /// 探测时记下的上传地址，面板上直接显示、可复制。
    http_url: String,
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
            max_mb: DEFAULT_MAX_MB,
            compress: true,
            http_port: 0,
            http_url: String::new(),
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
    if let Some(v) = get("max_mb") {
        cfg.max_mb = v.trim().parse::<u32>().unwrap_or(cfg.max_mb).clamp(1, 2048);
    }
    if let Some(v) = form.get("compress") {
        cfg.compress = truthy(v);
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
        "max_mb": cfg.max_mb.to_string(),
        "compress": if cfg.compress { "true" } else { "false" },
        "http_port": cfg.http_port.to_string(),
        "http_url": cfg.http_url,
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
    // HTTP 入口（`/plugin/imagehost/**`）：上传服务、自述、直链代理都在这里分派。
    if let Some(http) = params.get("http").filter(|v| v.is_object()) {
        return serve_http(id, params, http);
    }

    let action = s(params.get("action"));
    if action == "exit" || action == "shutdown" {
        return json!({
            "id": id,
            "ops": [],
            "result": { "status": 200, "body": { "status": i18n::t("已关闭"), "lamp": "ok" } }
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
            panel(&id, ops, &cfg, &i18n::t("设置已保存"), "ok", None)
        }
        "copy-link" => act_copy(id, cfg, false),
        "copy-short" => act_copy(id, cfg, true),
        "detect-port" => act_detect_port(id, &action, step, &form, cfg, &provided),
        "run" | "upload" => act_run(id, &action, step, &form, cfg, &provided, &work),
        other => err_panel(&id, &cfg, &i18n::f("未知动作：{other}", &[&other])),
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
        i18n::f("共 {} 个远程", &[&list.len()])
    } else if list.is_empty() {
        i18n::t("没读到任何远程：先在 rclone 配置里加一个").to_string()
    } else {
        i18n::f("当前远程：{}", &[&list[0]])
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
        return err_panel(&id, &cfg, &i18n::t("先选择要浏览的远程"));
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
        i18n::f("「{}」下没有子目录", &[&if cur.is_empty() { "/" } else { cur.as_str() }])
    } else {
        i18n::f("「{}」下有 {} 个子目录", &[&if cur.is_empty() { "/" } else { cur.as_str() }, &dirs.len()])
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
                "title": i18n::t("选择要上传的图片"),
                "filters": ["png", "jpg", "jpeg", "gif", "webp", "bmp"],
            }),
        )];
        return next(&id, needs, action, 1, form, &cfg, work);
    }
    let picked = provided.get("pick").cloned().unwrap_or(Value::Null);
    let path = s(picked.get("path"));
    if path.is_empty() {
        return panel(&id, Vec::new(), &cfg, &i18n::t("没有选择文件"), "warn", None);
    }
    cfg.upload_path = path;
    let ops = vec![save_op(&cfg)];
    let status = i18n::f("已选择：{}", &[&cfg.upload_path]);
    panel(&id, ops, &cfg, &status, "ok", None)
}

fn act_copy(id: Value, cfg: Cfg, short: bool) -> Value {
    let text = if short {
        cfg.last_short.trim().to_string()
    } else {
        cfg.last_link.trim().to_string()
    };
    if text.is_empty() {
        let msg = if short { i18n::t("还没有短链") } else { i18n::t("还没有直链") };
        return panel(&id, Vec::new(), &cfg, &msg, "warn", None);
    }
    let ops = vec![json!({ "op": "ui.copy", "text": text })];
    let msg = if short { i18n::t("已复制短链") } else { i18n::t("已复制直链") };
    panel(&id, ops, &cfg, &msg, "ok", None)
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
                return err_panel(&id, &cfg, &i18n::t("先在「上传图片路径」里选一张本机图片"));
            }
            if cfg.remote.trim().is_empty() {
                return err_panel(&id, &cfg, &i18n::t("先选择要上传到的远程"));
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
                return err_panel(&id, &cfg, &i18n::f("读取图片失败：{e}", &[&e]));
            }
            let mut data_b64 = s(file.get("data_b64"));
            if data_b64.is_empty() {
                return err_panel(&id, &cfg, &i18n::t("读到的图片是空的"));
            }
            let mut base_name = file_name(&cfg.upload_path);
            if base_name.is_empty() {
                return err_panel(&id, &cfg, &i18n::t("看不出文件名，检查「上传图片路径」"));
            }
            // 超过上限就压一压：和 HTTP 上传走同一套逻辑。
            let mut shrunk = String::new();
            match shrink_if_needed(&base_name, &data_b64, &cfg) {
                Ok(Some((b64, name, note))) => {
                    data_b64 = b64;
                    base_name = name;
                    shrunk = note;
                }
                Ok(None) => {}
                Err(e) => return err_panel(&id, &cfg, &e),
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
                        &i18n::f("「{shown}」已经存在这份文件，按「去重」取消上传（要留两份就取消勾选「去重」）", &[&shown]),
                    );
                }
                final_name = resolve_conflict(&existing, &stem, &ext);
            }
            let mut work = work.clone();
            work.insert("name".to_string(), json!(final_name));
            if !shrunk.is_empty() {
                work.insert("shrunk".to_string(), json!(shrunk));
            }
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
                return err_panel(&id, &cfg, &i18n::f("上传失败：{e}", &[&e]));
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
        _ => err_panel(&id, &cfg, &i18n::t("状态丢了，重新点一次「生成直链」")),
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
            &i18n::t("文件传上去了，但拿不到直链：该远程不支持公开链接，也没取到本机服务端口"),
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
            "message": if short_err.is_empty() { i18n::t("图片已上传，直链已生成") } else { i18n::t("图片已上传，短链没换成") },
        }));
    } else {
        ops.push(json!({ "op": "ui.toast", "level": "info", "message": i18n::t("图片已上传，直链与短链都已生成") }));
    }

    let mut status = i18n::f("已上传：{stored}", &[&stored]);
    if let Some(src) = source.filter(|s| !s.is_empty()) {
        // `src` 是落盘值（站点 / 本机代理），显示时才翻
        status.push_str(&i18n::f("（直链来源：{src}）", &[&i18n::t(&src)]));
    }
    if let Some(note) = work.get("shrunk").map(vstr).filter(|n| !n.is_empty()) {
        status.push_str(&i18n::f("；{note}", &[&note]));
    }
    if deduped {
        status.push_str(&i18n::t("；站点已有同一份内容，直接复用了旧文件"));
    }
    if !short.is_empty() {
        status.push_str(&i18n::f("；短链：{short}", &[&short]));
    } else if cfg.short_tpl == 1 {
        if short_err.is_empty() {
            status.push_str(&i18n::t("；短链模板没换成功，直链可用"));
        } else {
            status.push_str(&i18n::f("；短链失败：{short_err}", &[&short_err]));
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
            json!(i18n::f("短链服务返回 HTTP {code}：{brief}", &[&code, &brief])),
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
        return Err(i18n::t("未填写短链请求地址").to_string());
    }
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("短链请求地址必须以 http:// 或 https:// 开头".to_string());
    }
    let key = cfg.short_key.trim();
    if key.is_empty() {
        return Err(i18n::t("短链模板一必须填写 api_key").to_string());
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
        return Err(i18n::t("短链服务回了空响应").to_string());
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
                Err(i18n::t("短链服务没给出链接").to_string())
            } else {
                Ok(line)
            }
        }
    }
}

fn parse_json_short(text: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| i18n::f("短链服务回的不是 JSON：{e}", &[&e]))?;
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
    Err(i18n::f("短链服务返回里没有链接：{}", &[&first_line(text)]))
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------- HTTP 上传服务

/// `/plugin/imagehost/**` 的 HTTP 分派：`ping` 自述、`upload` 收图，其余都当直链代理。
fn serve_http(id: Value, params: &Value, http: &Value) -> Value {
    let path = s(http.get("path"));
    let head = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if head == PING_ROUTE {
        return serve_ping(id, params);
    }
    if head == UPLOAD_ROUTE {
        return serve_upload(id, params, http);
    }
    // `/i/<远程>:<路径>` 等其它路径照旧当直链代理。
    serve_image(id, params, http)
}

/// `/plugin/imagehost/ping`：外部客户端先问这里，拿到入口地址、上限和压缩开关再上传。
fn serve_ping(id: Value, params: &Value) -> Value {
    let prev = params.get("state").cloned().unwrap_or(Value::Null);
    let step = prev.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let provided = params.get("provided").cloned().unwrap_or(Value::Null);

    match step {
        0 => json!({
            "id": id,
            "need": [
                need("kv.get_all", "kv", json!({})),
                need("env.info", "env", json!({})),
            ],
            "state": { "step": 1 }
        }),
        1 => {
            // kv 只在这一步有，先把配置钉进 state，免得下一步读成默认值。
            let (cfg, _) = load_cfg(&provided);
            let data_dir = data_dir_of(&provided);
            if data_dir.is_empty() {
                return serve_upload_info(&id, cfg, 0);
            }
            next_http(
                &id,
                vec![host_cfg_need(&data_dir)],
                json!({ "step": 2, "cfg": cfg }),
            )
        }
        _ => {
            let cfg = match cfg_from_state(&prev) {
                Some(c) => c,
                None => load_cfg(&provided).0,
            };
            serve_upload_info(&id, cfg, host_port_of(&provided))
        }
    }
}

/// ping 的正文：客户端照着 `upload_url` 传就行。
fn serve_upload_info(id: &Value, cfg: Cfg, port: u16) -> Value {
    let origin = origin_of(port);
    let path = format!("/plugin/{PLUGIN_ID}/{UPLOAD_ROUTE}");
    http_json(
        id,
        200,
        json!({
            "ok": true,
            "plugin": PLUGIN_ID,
            "version": env!("CARGO_PKG_VERSION"),
            "port": port,
            "upload_url": if origin.is_empty() { String::new() } else { format!("{origin}{path}") },
            "upload_path": path,
            "accept": i18n::t("POST multipart/form-data（字段名 file / image）或直接把图片字节当 body"),
            "max_mb": cfg.max_mb,
            "max_bytes": limit_of(&cfg) as u64,
            "compress": cfg.compress,
            "short": cfg.short_tpl == 1,
            "remote": cfg.remote,
            "dir": norm_dir(&cfg.path),
        }),
    )
}

/// `/plugin/imagehost/upload`：外部客户端把图片发进来 → 传上远程 → 回直链/短链。
fn serve_upload(id: Value, params: &Value, http: &Value) -> Value {
    let prev = params.get("state").cloned().unwrap_or(Value::Null);
    let step = prev.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let provided = params.get("provided").cloned().unwrap_or(Value::Null);

    if step == 0 {
        return json!({
            "id": id,
            "need": [
                need("kv.get_all", "kv", json!({})),
                need("env.info", "env", json!({})),
            ],
            "state": { "step": 1 }
        });
    }

    let (cfg, _) = match cfg_from_state(&prev) {
        Some(c) => (c, false),
        None => load_cfg(&provided),
    };
    let work: Map<String, Value> = prev
        .get("work")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    match step {
        1 => {
            if cfg.remote.trim().is_empty() {
                return http_json(
                    &id,
                    503,
                    json!({
                        "ok": false,
                        "error": i18n::t("插件还没选存储远程：先在 RCGUI 的「图床」面板里选一个远程并保存"),
                    }),
                );
            }
            let (name, bytes) = match parse_upload(http) {
                Ok(v) => v,
                Err((code, msg)) => return http_json(&id, code, json!({ "ok": false, "error": msg })),
            };
            if bytes.is_empty() {
                return http_json(&id, 400, json!({ "ok": false, "error": i18n::t("收到的图片是空的") }));
            }
            if bytes.len() > limit_of(&cfg) && !cfg.compress {
                return http_json(
                    &id,
                    413,
                    json!({
                        "ok": false,
                        // `i18n::f` 只认 `{}`：浮点自己先格式化成一位小数
                        "error": i18n::f(
                            "图片 {} MB 超过 {} MB 上限，而且「超限压缩」是关的",
                            &[&mb1(bytes.len()), &cfg.max_mb]
                        ),
                        "max_bytes": limit_of(&cfg) as u64,
                    }),
                );
            }
            let dir = norm_dir(&cfg.path);
            let mut needs = vec![need(
                "rclone.call",
                "list",
                json!({
                    "method": "operations/list",
                    "params": { "fs": format!("{}:", cfg.remote), "remote": dir, "no_modtime": true }
                }),
            )];
            let data_dir = data_dir_of(&provided);
            if !data_dir.is_empty() {
                needs.push(host_cfg_need(&data_dir));
            }
            let mut work = Map::new();
            work.insert("name".to_string(), json!(name));
            work.insert("orig".to_string(), json!(bytes.len()));
            next_http(&id, needs, json!({ "step": 2, "cfg": cfg, "work": work }))
        }
        2 => {
            let (name, bytes) = match parse_upload(http) {
                Ok(v) => v,
                Err((code, msg)) => return http_json(&id, code, json!({ "ok": false, "error": msg })),
            };
            let dir = norm_dir(&cfg.path);
            let port = host_port_of(&provided);

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
                let n = s(it.get("Name"));
                if !n.is_empty() {
                    existing.push(n);
                }
            }

            // 同名文件已经在远程上：开着「去重」就直接把旧文件的直链回过去。
            let stored = join_dir(&dir, &name);
            if cfg.dedupe && existing.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                let link = if cfg.base.trim().is_empty() {
                    proxy_link(&cfg, &origin_of(port), &stored)
                } else {
                    proxy_link(&cfg, "", &stored)
                };
                let mut work = work.clone();
                work.insert("name".to_string(), json!(name));
                work.insert("stored".to_string(), json!(stored));
                work.insert("deduped".to_string(), json!(true));
                work.insert("size".to_string(), json!(bytes.len()));
                work.insert("link".to_string(), json!(link));
                return http_after_link(&id, cfg, work);
            }

            // 超过上限就压一压（和面板上传同一套逻辑）。
            let (bytes, name, note) = match shrink_bytes(&name, &bytes, &cfg) {
                Ok(Some((out, new_name, note))) => (out, new_name, note),
                Ok(None) => (bytes, name, String::new()),
                Err(e) => return http_json(&id, 413, json!({ "ok": false, "error": e })),
            };

            let (stem, ext) = split_ext(&name);
            let mut final_name = name.clone();
            if existing.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                final_name = resolve_conflict(&existing, &stem, &ext);
            }

            let orig = work
                .get("orig")
                .and_then(|v| v.as_u64())
                .unwrap_or(bytes.len() as u64);
            let mut work = Map::new();
            work.insert("name".to_string(), json!(final_name));
            work.insert("orig".to_string(), json!(orig));
            work.insert("size".to_string(), json!(bytes.len()));
            if !note.is_empty() {
                work.insert("shrunk".to_string(), json!(note));
            }
            let needs = vec![need(
                "rclone.upload_file",
                "up",
                json!({
                    "fs": format!("{}:{}", cfg.remote, dir),
                    "remote_path": final_name,
                    "filename": final_name,
                    "data_b64": encode_b64(&bytes),
                }),
            )];
            next_http(&id, needs, json!({ "step": 3, "cfg": cfg, "work": work }))
        }
        3 => {
            let up = provided.get("up").cloned().unwrap_or(Value::Null);
            if let Some(e) = cap_err(&up) {
                return http_json(&id, 502, json!({ "ok": false, "error": i18n::f("上传失败：{e}", &[&e]) }));
            }
            let dir = norm_dir(&cfg.path);
            let name = s(work.get("name"));
            let stored = join_dir(&dir, &name);
            let mut work = work.clone();
            work.insert("stored".to_string(), json!(stored));
            if up.get("deduped").and_then(|v| v.as_bool()).unwrap_or(false) {
                work.insert("deduped".to_string(), json!(true));
            }
            let site = s(up.get("link")).trim().to_string();
            let link = if !site.is_empty() {
                work.insert("source".to_string(), json!("站点"));
                site
            } else {
                work.insert("source".to_string(), json!("本机代理"));
                proxy_link(&cfg, &origin_of(host_port_of(&provided)), &stored)
            };
            if link.is_empty() {
                return http_json(
                    &id,
                    502,
                    json!({
                        "ok": false,
                        "error": i18n::t("文件传上去了，但拿不到直链：该远程不支持公开链接，也没取到本机服务端口"),
                        "stored": stored,
                    }),
                );
            }
            work.insert("link".to_string(), json!(link));
            http_after_link(&id, cfg, work)
        }
        _ => {
            let resp = provided.get("short").cloned().unwrap_or(Value::Null);
            let mut work = work.clone();
            if let Some(e) = cap_err(&resp) {
                work.insert("short_err".to_string(), json!(e));
            } else {
                let code = resp.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
                let body = resp.get("body").map(vstr).unwrap_or_default();
                if code != 0 && !(200..300).contains(&code) {
                    work.insert(
                        "short_err".to_string(),
                        json!(i18n::f("短链服务返回 HTTP {code}：{}", &[&code, &first_line(&body)])),
                    );
                } else {
                    match parse_short(cfg.short_fmt, &body) {
                        Ok(x) => {
                            work.insert("short".to_string(), json!(x));
                        }
                        Err(e) => {
                            work.insert("short_err".to_string(), json!(e));
                        }
                    }
                }
            }
            http_done(&id, cfg, work)
        }
    }
}

/// 直链到手：开了短链模板就再插一轮 HTTP 请求换短链，否则直接回 JSON。
fn http_after_link(id: &Value, mut cfg: Cfg, work: Map<String, Value>) -> Value {
    let link = s(work.get("link"));
    if cfg.short_tpl != 1 {
        return http_done(id, cfg, work);
    }
    match short_request(&cfg, &link) {
        Ok((method, url, body, headers)) => {
            let mut p = Map::new();
            p.insert("method".to_string(), json!(method));
            p.insert("url".to_string(), json!(url));
            if !headers.is_empty() {
                p.insert("headers".to_string(), json!(headers));
            }
            if let Some(b) = body {
                p.insert("body".to_string(), json!(b));
            }
            p.insert("timeout_ms".to_string(), json!(15000));
            p.insert("max_bytes".to_string(), json!(1048576));
            next_http(
                id,
                vec![need("http.request", "short", Value::Object(p))],
                json!({ "step": 4, "cfg": cfg, "work": work }),
            )
        }
        Err(e) => {
            let mut work = work;
            work.insert("short_err".to_string(), json!(e));
            // 短链没换成不算失败：直链照给，客户端自己决定用不用短链。
            cfg.short_tpl = 0;
            http_done(id, cfg, work)
        }
    }
}

/// HTTP 上传的收尾：顺手把「上次直链/短链」记进配置，面板上一眼能看见。
fn http_done(id: &Value, mut cfg: Cfg, work: Map<String, Value>) -> Value {
    let link = s(work.get("link"));
    let short = s(work.get("short"));
    let stored = s(work.get("stored"));
    let name = s(work.get("name"));
    let short_err = s(work.get("short_err"));
    let deduped = work
        .get("deduped")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let size = work.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let orig = work.get("orig").and_then(|v| v.as_u64()).unwrap_or(size);
    let note = work.get("shrunk").map(vstr).unwrap_or_default();
    let content_type = guess_content_type(&name);

    cfg.last_link = link.clone();
    if !short.is_empty() {
        cfg.last_short = short.clone();
    }
    remember_path(&mut cfg, &dirname(&stored));

    let mut body = Map::new();
    body.insert("ok".to_string(), json!(true));
    body.insert("url".to_string(), json!(link));
    body.insert(
        "short_url".to_string(),
        if short.is_empty() { Value::Null } else { json!(short) },
    );
    body.insert("name".to_string(), json!(name));
    body.insert("stored".to_string(), json!(stored));
    body.insert("remote".to_string(), json!(cfg.remote));
    body.insert("content_type".to_string(), json!(content_type));
    body.insert("size".to_string(), json!(size));
    body.insert("original_size".to_string(), json!(orig));
    body.insert("compressed".to_string(), json!(!note.is_empty()));
    if !note.is_empty() {
        body.insert("note".to_string(), json!(note));
    }
    body.insert("deduped".to_string(), json!(deduped));
    if !short_err.is_empty() {
        body.insert("short_error".to_string(), json!(short_err));
    }
    json!({
        "id": id,
        "ops": [save_op(&cfg)],
        "result": { "status": 200, "body": Value::Object(body) }
    })
}

fn http_json(id: &Value, status: u16, body: Value) -> Value {
    json!({
        "id": id,
        "ops": [],
        "result": { "status": status, "body": body }
    })
}

fn next_http(id: &Value, needs: Vec<Value>, state: Value) -> Value {
    json!({ "id": id, "need": needs, "state": state })
}

/// 把请求体认成图片：multipart / 裸字节 / JSON 三种写法都接。
fn parse_upload(http: &Value) -> Result<(String, Vec<u8>), (u16, String)> {
    let query_name = s(http.get("query").and_then(|q| q.get("name")));
    let ctype = s(http.get("headers").and_then(|h| h.get("content-type"))).to_ascii_lowercase();

    // 1) multipart/form-data：宿主已经拆好了，文件在 files[] 里。
    if let Some(mp) = http.get("multipart").filter(|v| v.is_object()) {
        let want = s(mp.get("fields").and_then(|f| f.get("name")));
        let mut saw_file = false;
        if let Some(files) = mp.get("files").and_then(|v| v.as_array()) {
            for f in files {
                let data = s(f.get("dataB64"));
                if data.is_empty() {
                    saw_file = true;
                    continue;
                }
                let bytes = decode_b64(&data)?;
                let given = if want.trim().is_empty() {
                    s(f.get("filename"))
                } else {
                    want.trim().to_string()
                };
                return Ok((upload_name(&given, &ctype), bytes));
            }
        }
        if saw_file {
            return Err((
                413,
                i18n::t("文件字段是空的：RCGUI 宿主对 multipart 的单个字段有大小上限，大图请改用裸字节 POST（图片字节直接当 body，文件名放 ?name=）").to_string(),
            ));
        }
        return Err((
            400,
            i18n::t("multipart 里没有文件：字段名用 file / image 都认，也可以把图片字节直接当 body").to_string(),
        ));
    }

    let raw = s(http.get("body_b64"));
    if raw.is_empty() {
        return Err((
            400,
            i18n::t("请求体是空的：把图片放在 multipart 的 file 字段，或直接发图片字节").to_string(),
        ));
    }
    let bytes = decode_b64(&raw)?;

    // 2) JSON：{"filename": "a.png", "data_b64": "..."}，有的客户端喜欢这么发。
    if ctype.contains("json") || bytes.first() == Some(&b'{') {
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            let data = ["data_b64", "data", "base64"]
                .iter()
                .map(|k| s(v.get(*k)))
                .find(|t| !t.is_empty());
            if let Some(data) = data {
                let given = ["filename", "name", "file"]
                    .iter()
                    .map(|k| s(v.get(*k)))
                    .find(|t| !t.is_empty())
                    .unwrap_or_else(|| query_name.clone());
                let bytes = decode_b64(&data)?;
                return Ok((upload_name(&given, &ctype), bytes));
            }
            if ctype.contains("json") {
                return Err((400, i18n::t("JSON 里没有 data_b64 / data / base64 字段").to_string()));
            }
        } else if ctype.contains("json") {
            return Err((400, i18n::t("JSON 解析失败").to_string()));
        }
    }

    // 3) 裸字节：名字只能从 `?name=` 或 content-type 猜。
    Ok((upload_name(&query_name, &ctype), bytes))
}

/// 文件名兜底：去掉路径分隔符；没有扩展名就按 content-type 补一个。
fn upload_name(given: &str, ctype: &str) -> String {
    let mut out: String = file_name(given)
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if out.trim().is_empty() {
        out = "image".to_string();
    }
    let (_, ext) = split_ext(&out);
    if ext.is_empty() {
        let guess = if ctype.contains("jpeg") || ctype.contains("jpg") {
            "jpg"
        } else if ctype.contains("png") {
            "png"
        } else if ctype.contains("webp") {
            "webp"
        } else if ctype.contains("gif") {
            "gif"
        } else if ctype.contains("bmp") {
            "bmp"
        } else if ctype.contains("avif") {
            "avif"
        } else {
            ""
        };
        if !guess.is_empty() {
            out.push('.');
            out.push_str(guess);
        }
    }
    out
}

// ---------------------------------------------------------------- 压缩

/// 面板上传用的一层壳：读进来的是 base64，压完再编回去。
fn shrink_if_needed(
    name: &str,
    data_b64: &str,
    cfg: &Cfg,
) -> Result<Option<(String, String, String)>, String> {
    if limit_of(cfg) == 0 {
        return Ok(None);
    }
    let bytes = match decode_b64(data_b64) {
        Ok(b) => b,
        Err((_, e)) => return Err(e),
    };
    match shrink_bytes(name, &bytes, cfg)? {
        Some((out, new_name, note)) => Ok(Some((encode_b64(&out), new_name, note))),
        None => Ok(None),
    }
}

/// 超过上限就等比缩小重编码；面板上传（base64）和 HTTP 上传（裸字节）共用这一份。
fn shrink_bytes(
    name: &str,
    bytes: &[u8],
    cfg: &Cfg,
) -> Result<Option<(Vec<u8>, String, String)>, String> {
    let limit = limit_of(cfg);
    if bytes.len() <= limit {
        return Ok(None);
    }
    let fmt = image::guess_format(bytes)
        .map_err(|_| i18n::t("认不出图片格式（支持 png / jpg / webp），没法压缩").to_string())?;
    let img = image::load_from_memory_with_format(bytes, fmt)
        .map_err(|e| i18n::f("图片解码失败：{e}", &[&e]))?;
    let alpha = img.color().has_alpha();
    let (w0, h0) = (img.width().max(1), img.height().max(1));
    let mut scale = 1.0f32;
    let mut last = bytes.len();

    for round in 0..12u8 {
        let w = ((w0 as f32 * scale).round() as u32).max(MIN_SIDE).min(w0);
        let h = ((h0 as f32 * scale).round() as u32).max(MIN_SIDE).min(h0);
        let (out, ext) = encode_image(&img, w, h, alpha, round)?;
        last = out.len();
        if out.len() <= limit {
            let note = i18n::f(
                "原图 {} MB 超过 {} MB，已压成 {}×{} 的 {}（{} MB）",
                &[
                    &mb1(bytes.len()),
                    &cfg.max_mb,
                    &w,
                    &h,
                    &ext.to_uppercase(),
                    &mb1(out.len()),
                ],
            );
            return Ok(Some((out, renamed(name, ext), note)));
        }
        if w <= MIN_SIDE && h <= MIN_SIDE {
            break;
        }
        scale *= 0.82;
    }
    Err(i18n::f(
        "压到 {}px 还是降不到 {} MB（现在 {} MB）",
        &[&MIN_SIDE, &cfg.max_mb, &mb1(last)],
    ))
}

/// 缩到 `w×h` 再编码：带透明通道留 PNG，其余转 JPEG（体积差一个量级）。
fn encode_image(
    img: &image::DynamicImage,
    w: u32,
    h: u32,
    alpha: bool,
    round: u8,
) -> Result<(Vec<u8>, &'static str), String> {
    use image::ImageEncoder as _;
    let scaled = if (w, h) == (img.width(), img.height()) {
        img.clone()
    } else {
        img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
    };
    let mut out: Vec<u8> = Vec::new();
    if alpha {
        let rgba = scaled.to_rgba8();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| i18n::f("PNG 编码失败：{e}", &[&e]))?;
        Ok((out, "png"))
    } else {
        // 每轮顺手再降一点质量，尺寸兜底时才不至于只能压到 64px。
        let quality = 82u8.saturating_sub(round * 4).max(52);
        let rgb = scaled.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .map_err(|e| i18n::f("JPEG 编码失败：{e}", &[&e]))?;
        Ok((out, "jpg"))
    }
}

/// 换了格式就顺手换扩展名。
fn renamed(name: &str, ext: &str) -> String {
    let (stem, old) = split_ext(name);
    if old.eq_ignore_ascii_case(ext) {
        return name.to_string();
    }
    format!("{stem}.{ext}")
}

// ---------------------------------------------------------------- 服务口径

/// 上限的字节数。
fn limit_of(cfg: &Cfg) -> usize {
    (cfg.max_mb.max(1) as usize) * 1024 * 1024
}

fn mb(n: usize) -> f64 {
    (n as f64) / (1024.0 * 1024.0)
}

/// 兆字节的展示形式（固定一位小数）：`i18n::f` 只认 `{}`，浮点得先自己格式化。
fn mb1(n: usize) -> String {
    format!("{:.1}", mb(n))
}

/// 本机服务的根地址；端口没探到就空串（调用方自己兜）。
fn origin_of(port: u16) -> String {
    if port == 0 {
        String::new()
    } else {
        format!("http://127.0.0.1:{port}")
    }
}

/// 直链：站点直链拿不到时用本机代理，`<base 或 origin>/plugin/imagehost/i/<远程>:<路径>`。
fn proxy_link(cfg: &Cfg, origin: &str, stored: &str) -> String {
    let mut base = cfg.base.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        base = origin.trim_end_matches('/').to_string();
    }
    if base.is_empty() || stored.is_empty() {
        return String::new();
    }
    format!(
        "{base}/plugin/{PLUGIN_ID}{ROUTE_PREFIX}{}:{}",
        cfg.remote,
        uri_path(stored)
    )
}

fn data_dir_of(provided: &Value) -> String {
    s(provided.get("env").and_then(|e| e.get("data_dir")))
}

fn settings_path(data_dir: &str) -> String {
    let d = data_dir.trim_end_matches(['\\', '/']);
    format!("{d}\\config.json")
}

fn host_cfg_need(data_dir: &str) -> Value {
    need(
        "fs.read_text",
        "host_cfg",
        json!({ "path": settings_path(data_dir), "max_bytes": 1048576 }),
    )
}

/// 宿主配置里的 `http.port`（本机服务端口）；没读到就是 0。
fn host_port_of(provided: &Value) -> u16 {
    let raw = provided.get("host_cfg").cloned().unwrap_or(Value::Null);
    if cap_err(&raw).is_some() {
        return 0;
    }
    let text = raw
        .get("text")
        .map(vstr)
        .unwrap_or_else(|| vstr(&unwrap_result(&raw)));
    json_port(&text)
}

fn encode_b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_b64(text: &str) -> Result<Vec<u8>, (u16, String)> {
    use base64::Engine as _;
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if let Ok(b) = base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes()) {
        return Ok(b);
    }
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(cleaned.as_bytes())
        .map_err(|_| (400, i18n::t("data_b64 不是有效的 base64").to_string()))
}

/// 面板上的「探测端口」：读宿主 config.json 的 http.port，把上传地址写回面板。
fn act_detect_port(
    id: Value,
    action: &str,
    step: u64,
    form: &Map<String, Value>,
    mut cfg: Cfg,
    provided: &Value,
) -> Value {
    match step {
        0 => next(
            &id,
            vec![need("env.info", "env", json!({}))],
            action,
            1,
            form,
            &cfg,
            &Map::new(),
        ),
        1 => {
            let data_dir = data_dir_of(provided);
            if data_dir.is_empty() {
                return err_panel(&id, &cfg, &i18n::t("取不到 RCGUI 的数据目录：重启一下 RCGUI 再点「探测端口」"));
            }
            next(
                &id,
                vec![host_cfg_need(&data_dir)],
                action,
                2,
                form,
                &cfg,
                &Map::new(),
            )
        }
        _ => {
            let port = host_port_of(provided);
            if port == 0 {
                cfg.http_port = 0;
                cfg.http_url = String::new();
                return panel(
                    &id,
                    vec![save_op(&cfg)],
                    &cfg,
                    &i18n::t("没读到宿主端口：先确认 RCGUI 设置里的「HTTP 服务」是开着的"),
                    "warn",
                    None,
                );
            }
            cfg.http_port = port;
            cfg.http_url = format!(
                "{}/plugin/{PLUGIN_ID}/{UPLOAD_ROUTE}",
                origin_of(port)
            );
            let status = i18n::f("上传地址：{}（上限 {} MB，{}）", &[&cfg.http_url, &cfg.max_mb, &if cfg.compress {
                    i18n::t("超限自动压缩")
                } else {
                    i18n::t("超限直接报错")
                }]);
            panel(&id, vec![save_op(&cfg)], &cfg, &status, "ok", None)
        }
    }
}

// ---------------------------------------------------------------- 直链代理

/// `/plugin/imagehost/i/<remote>:<路径>`：从远程读回图片字节。
fn serve_image(id: Value, params: &Value, http: &Value) -> Value {
    let path = s(http.get("path"));
    let rest = path.trim_start_matches('/');
    let rest = match rest.strip_prefix(ROUTE_PREFIX.trim_start_matches('/')) {
        Some(x) => x,
        None => return http_not_found(&id, &i18n::t("不是图片路径")),
    };
    let (remote, stored) = match rest.split_once(':') {
        Some((r, p)) => (r.to_string(), p.trim_start_matches('/').to_string()),
        None => return http_not_found(&id, &i18n::t("路径缺少「远程:文件」")),
    };
    if remote.is_empty() || stored.is_empty() || stored.contains("..") {
        return http_not_found(&id, &i18n::t("路径不合法"));
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
        return http_not_found(&id, &i18n::t("文件是空的"));
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
