//! 图床插件（wasm32-wasip1）：语义移植自旧版 TypeScript 实现。
//! stdin 读入一个 JSON 请求，stdout 写出一个 JSON 响应；需要宿主能力时
//! 先回复 `need`，宿主执行后带 `provided` 重新运行本插件并依据 `state` 续算。

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------- helpers ----------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha1_hex(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    hex(&h.finalize())
}

fn b64decode(s: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("base64 解码失败: {e}"))
}

fn b64encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// 拆扩展名：ext 不含点；无扩展名或隐藏文件返回空 ext
fn split_ext(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i + 1..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

/// 规范化子目录：统一正斜杠，去掉空段/./..
fn normalize_sub(dir: &str) -> String {
    dir.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect::<Vec<_>>()
        .join("/")
}

fn dirname_of(p: &str) -> String {
    match p.rfind('/') {
        Some(i) => p[..i].to_string(),
        None => String::new(),
    }
}

fn basename_of(p: &str) -> String {
    let clean = p.replace('\\', "/");
    match clean.rfind('/') {
        Some(i) => clean[i + 1..].to_string(),
        None => clean,
    }
}

fn join_nonempty(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn s(v: Option<&Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 宿主能力返回值有的裸给，有的套着 rclone RC 的 `{"result": ...}` 包装，统一拆一层
fn unwrap_result(v: &Value) -> Value {
    match v.get("result") {
        Some(r) if !r.is_null() => r.clone(),
        _ => v.clone(),
    }
}

/// 规范化远程相对路径：去盘符/前导斜杠、反斜杠转正斜杠、词法消解 `.` 与 `..`。
/// 任何想爬出根目录的写法都返回 Err —— `/i/` 直链代理靠它挡住 `../` 穿越。
fn normalize_rel_path(p: &str) -> Result<String, String> {
    let mut t = p.trim();
    let b = t.as_bytes();
    // 去 Windows 盘符：`C:\a.png` / `c:/a.png`
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        t = &t[2..];
    }
    let norm = t.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for seg in norm.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if out.pop().is_none() {
                    return Err("图片路径越界".to_string());
                }
            }
            other => out.push(other),
        }
    }
    Ok(out.join("/"))
}

/// 新协议只保证给 `ymd/hms/iso/epoch`；补出老字段（`date_iso`/`ms`/`epoch_ms`），
/// 免得既有命名、日期分目录、令牌种子读到空值。
fn normalize_now(now: &Value) -> Value {
    let mut out = if now.is_object() { now.clone() } else { json!({}) };
    if s(out.get("date_iso")).is_empty() {
        let d: String = s(out.get("iso")).chars().take(10).collect();
        if d.len() == 10 {
            out["date_iso"] = json!(d);
        }
    }
    let epoch = out.get("epoch").and_then(|v| v.as_u64()).unwrap_or(0);
    if out.get("epoch_ms").and_then(|v| v.as_u64()).unwrap_or(0) == 0 && epoch > 0 {
        out["epoch_ms"] = json!(epoch.saturating_mul(1000));
    }
    if s(out.get("ms")).is_empty() {
        let sub = if epoch > 0 { epoch % 1000 } else { 0 };
        out["ms"] = json!(format!("{sub:03}"));
    }
    out
}

// ---------------- config types ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRoot {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub remote: String,
    #[serde(rename = "publicUrlTemplate", default)]
    pub public_url_template: String,
    #[serde(rename = "isDefault", default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Naming {
    #[serde(default = "d_mode")]
    pub mode: String,
    #[serde(rename = "seqPad", default = "d_pad")]
    pub seq_pad: i64,
}
fn d_mode() -> String { "timestamp".into() }
fn d_pad() -> i64 { 3 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObsidianCfg {
    #[serde(rename = "dirLayout", default = "d_layout")]
    pub dir_layout: String,
}
fn d_layout() -> String { "note".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub token: String,
    #[serde(rename = "createdAt", default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagehostConfig {
    #[serde(default = "d_roots")]
    pub roots: Vec<ImageRoot>,
    #[serde(default = "d_naming")]
    pub naming: Naming,
    #[serde(default = "d_true")]
    pub dedupe: bool,
    #[serde(default)]
    pub trash_dir: String,
    #[serde(default = "d_obs")]
    pub obsidian: ObsidianCfg,
    #[serde(default)]
    pub tokens: Vec<Token>,
    /// 公网地址（可选）：非空时直链用它打头，否则退回本次请求的 origin
    #[serde(default)]
    pub base: String,
    /// 最近一次上传的直链：只用来预填面板（宿主不在替插件存 LastLink 了）
    #[serde(default)]
    pub lastlink: String,
}
fn d_true() -> bool { true }
fn d_roots() -> Vec<ImageRoot> {
    vec![ImageRoot {
        id: "default".into(),
        name: "默认图床".into(),
        remote: String::new(),
        public_url_template: String::new(),
        is_default: true,
    }]
}
fn d_naming() -> Naming { Naming { mode: d_mode(), seq_pad: d_pad() } }
fn d_obs() -> ObsidianCfg { ObsidianCfg { dir_layout: d_layout() } }

impl ImagehostConfig {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
    /// 保证有默认 root 标记
    fn normalize(&mut self) {
        if !self.roots.iter().any(|r| r.is_default) {
            for (i, r) in self.roots.iter_mut().enumerate() {
                r.is_default = i == 0;
            }
        }
    }
}

/// 老版本把图床配置写在 `bed` 键里（remote + path + dedupe + lastlink）。
/// 升级后第一次跑时搬进新结构，省得用户重填一遍。
fn migrate_legacy_bed(kv: &Value) -> Option<ImagehostConfig> {
    let bed = kv.get("bed")?;
    let remote = s(bed.get("remote"));
    if remote.is_empty() {
        return None;
    }
    let path = s(bed.get("path"));
    let full = if path.is_empty() {
        remote
    } else {
        format!("{remote}:{}", path.trim_matches('/'))
    };
    Some(ImagehostConfig {
        roots: vec![ImageRoot {
            id: "default".into(),
            name: "默认图床".into(),
            remote: full,
            public_url_template: String::new(),
            is_default: true,
        }],
        naming: d_naming(),
        dedupe: bed.get("dedupe").and_then(|v| v.as_bool()).unwrap_or(true),
        trash_dir: String::new(),
        obsidian: d_obs(),
        tokens: vec![],
        base: String::new(),
        lastlink: s(bed.get("lastlink")),
    })
}

fn load_config(kv: &Value) -> ImagehostConfig {
    let raw = kv.get("config").cloned().unwrap_or(Value::Null);
    if raw.is_null() || raw.as_object().is_some_and(|o| o.is_empty()) {
        if let Some(migrated) = migrate_legacy_bed(kv) {
            return migrated;
        }
    }
    let mut cfg: ImagehostConfig = serde_json::from_value(raw).unwrap_or(ImagehostConfig {
        roots: d_roots(),
        naming: d_naming(),
        dedupe: true,
        trash_dir: String::new(),
        obsidian: d_obs(),
        tokens: vec![],
        base: String::new(),
        lastlink: String::new(),
    });
    if cfg.roots.is_empty() {
        cfg.roots = d_roots();
    }
    cfg.normalize();
    cfg
}

/// 解析面板里的存储根文本：`id=远程路径`，逗号/分号/换行分隔，可省略 id。
/// 同 id 的老根保留名称、公网模板与默认标记，避免面板一存就把别处配置抹掉。
fn parse_roots(text: &str, old: &[ImageRoot]) -> Vec<ImageRoot> {
    let mut out: Vec<ImageRoot> = Vec::new();
    for (i, tok) in text
        .split([',', '，', ';', '；', '\n'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .enumerate()
    {
        let (id, remote) = match tok.split_once('=') {
            Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
            None => (String::new(), tok.to_string()),
        };
        if remote.is_empty() {
            continue;
        }
        let id = if id.is_empty() { format!("root{}", i + 1) } else { id };
        if out.iter().any(|r| r.id == id) {
            continue; // 重复 id 只认第一个
        }
        let prev = old.iter().find(|r| r.id == id);
        out.push(ImageRoot {
            id: id.clone(),
            name: prev.map(|r| r.name.clone()).unwrap_or_default(),
            remote,
            public_url_template: prev.map(|r| r.public_url_template.clone()).unwrap_or_default(),
            is_default: prev.map(|r| r.is_default).unwrap_or(false),
        });
    }
    out
}

/// 根列表 → 面板文本框内容（`id=remote` 逗号分隔）
fn roots_to_text(roots: &[ImageRoot]) -> String {
    roots
        .iter()
        .map(|r| format!("{}={}", r.id, r.remote))
        .collect::<Vec<_>>()
        .join(",")
}

/// rclone 远端写法 "k-vault:assets/icons" → ("k-vault", "assets/icons")
fn split_spec(spec: &str) -> (String, String) {
    match spec.split_once(':') {
        Some((r, p)) => (
            r.trim().to_string(),
            p.trim().trim_matches('/').to_string(),
        ),
        None => (spec.trim().to_string(), String::new()),
    }
}

/// 远端名 + 目录 → rclone 写法（目录为空就只留远端名）
fn join_spec(remote: &str, path: &str) -> String {
    let r = remote.trim();
    let p = path.trim().trim_matches('/');
    if r.is_empty() {
        return String::new();
    }
    if p.is_empty() {
        r.to_string()
    } else {
        format!("{r}:{p}")
    }
}

/// 默认根：面板上「存储的远程 + 路径」编辑的就是它
fn default_root_index(roots: &[ImageRoot]) -> Option<usize> {
    roots
        .iter()
        .position(|r| r.is_default)
        .or(if roots.is_empty() { None } else { Some(0) })
}

/// 默认根当前拆出来的 (远端, 目录)
fn default_root_spec(cfg: &ImagehostConfig) -> (String, String) {
    match default_root_index(&cfg.roots).and_then(|i| cfg.roots.get(i)) {
        Some(r) => split_spec(&r.remote),
        None => (String::new(), String::new()),
    }
}

/// 把人话两栏（远端 / 路径）写回默认根的 remote。
///
/// 只在两栏与现状不同时才覆盖 —— 否则高级区里手写的多根 `roots` 文本会被
/// 面板上那些只是「回显」的值顶掉。
fn apply_store_fields(roots: &mut Vec<ImageRoot>, remote: &str, path: &str) {
    let remote = remote.trim();
    if remote.is_empty() {
        return;
    }
    if roots.is_empty() {
        roots.push(ImageRoot {
            id: "default".into(),
            name: "默认图床".into(),
            remote: String::new(),
            public_url_template: String::new(),
            is_default: true,
        });
    }
    let idx = default_root_index(roots).unwrap_or(0);
    let (cur_remote, cur_path) = split_spec(&roots[idx].remote);
    if remote == cur_remote && path.trim().trim_matches('/') == cur_path {
        return;
    }
    roots[idx].remote = join_spec(remote, path);
}

/// 直链拼接的唯一入口：配了公网地址就用它，否则退回本次请求的 origin。
fn build_link(base: &str, origin: &str, root_id: &str, path: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.is_empty() {
        format!("{origin}/i/{root_id}/{path}")
    } else {
        format!("{b}/i/{root_id}/{path}")
    }
}

/// 配置 → 回填进表单的 values（值一律是字符串，与宿主约定一致）
fn config_values(cfg: &ImagehostConfig) -> Value {
    let (remote, path) = default_root_spec(cfg);
    json!({
        "remote": remote,
        "path": path,
        "roots": roots_to_text(&cfg.roots),
        "naming": cfg.naming.mode,
        "seqpad": cfg.naming.seq_pad.to_string(),
        "dedupe": if cfg.dedupe { "true" } else { "false" },
        "obsidian": cfg.obsidian.dir_layout,
        "trash": cfg.trash_dir,
        "base": cfg.base,
        "lastlink": cfg.lastlink,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageItem {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "rootId", default)]
    pub root_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub ext: String,
    #[serde(rename = "uploadedAt", default)]
    pub uploaded_at: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn load_index(kv: &Value) -> Vec<ImageItem> {
    let raw = kv.get("index").cloned().unwrap_or(Value::Null);
    let items = raw.get("items").cloned().unwrap_or(Value::Array(vec![]));
    serde_json::from_value(items).unwrap_or_default()
}

// ---------------- naming（移植 naming.ts） ----------------

/// timestamp: YYYYMMDDHHmmssSSS（14 位 + 毫秒 3 位防冲突，来自宿主本地时间）
fn timestamp_name(now: &Value) -> String {
    format!("{}{}{}", s(now.get("ymd")), s(now.get("hms")), s(now.get("ms")))
}

/// seq: 已有文件名中纯数字序号最大值 +1，补零
fn next_seq_name(names: &[String], pad: i64) -> String {
    let mut max = 0i64;
    for n in names {
        let base = split_ext(n).0;
        if !base.is_empty() && base.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(v) = base.parse::<i64>() {
                max = max.max(v);
            }
        }
    }
    format!("{:0width$}", max + 1, width = pad.max(1) as usize)
}

/// 冲突：同名且不同 sha1 时追加 -1/-2
fn resolve_conflict_name(existing: &std::collections::HashSet<String>, base: &str, ext: &str) -> String {
    let suffix = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    if !existing.contains(&format!("{base}{suffix}")) {
        return format!("{base}{suffix}");
    }
    for n in 1..1000 {
        let cand = format!("{base}-{n}{suffix}");
        if !existing.contains(&cand) {
            return cand;
        }
    }
    format!("{base}-{}{suffix}", epoch_ms())
}

/// dirLayout=note: obsidian/<vault>/<dirname(notePath)>/
/// dirLayout=date:  obsidian/<vault>/<yyyy-MM-dd>/
/// dirLayout=vault: obsidian/<vault>/
/// dirLayout=flat:  obsidian/
fn obsidian_sub_path(layout: &str, vault: &str, note_path: &str, today: &str) -> String {
    let v = normalize_sub(vault);
    let note_dir = dirname_of(&normalize_sub(note_path));
    match layout {
        "date" => join_nonempty(&["obsidian", &v, today]),
        "vault" => join_nonempty(&["obsidian", &v]),
        "flat" => "obsidian".to_string(),
        _ => join_nonempty(&["obsidian", &v, &note_dir]),
    }
}

// ---------------- index 查询（移植 store.ts） ----------------

fn find_by_path<'a>(items: &'a [ImageItem], root_id: &str, p: &str) -> Option<&'a ImageItem> {
    items.iter().find(|i| i.root_id == root_id && i.path == p)
}

fn find_by_sha<'a>(items: &'a [ImageItem], root_id: &str, sha1: &str) -> Option<&'a ImageItem> {
    if sha1.is_empty() {
        return None;
    }
    items.iter().find(|i| i.root_id == root_id && i.sha1 == sha1)
}

/// 索引中某目录直属文件名（用于 seq 计数）
fn names_in_dir(items: &[ImageItem], root_id: &str, dir: &str) -> Vec<String> {
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    items
        .iter()
        .filter(|i| i.root_id == root_id && i.path.starts_with(&prefix) && !i.path[prefix.len()..].contains('/'))
        .map(|i| i.path[prefix.len()..].to_string())
        .collect()
}

fn item_id(root_id: &str, path: &str) -> String {
    let h = sha1_hex(format!("{root_id}:{path}").as_bytes());
    h[..16].to_string()
}

// ---------------- 请求结构 ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MultipartFile {
    #[serde(default)]
    filename: String,
    #[serde(rename = "dataB64", default)]
    data_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Multipart {
    #[serde(default)]
    fields: Map<String, Value>,
    #[serde(default)]
    files: Vec<MultipartFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HttpReq {
    #[serde(default)]
    method: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    query: Map<String, Value>,
    #[serde(default)]
    authorization: String,
    #[serde(rename = "bodyB64", default)]
    body_b64: String,
    #[serde(default)]
    multipart: Option<Multipart>,
    #[serde(default)]
    origin: String,
}

#[derive(Debug, Clone, Deserialize)]
struct State {
    #[serde(default)]
    http: Option<HttpReq>,
    #[serde(default)]
    now: Value,
    #[serde(rename = "reqId", default)]
    req_id: String,
    /// 面板动作模式下要记住的：动作名与表单快照（第二轮才能读到配置、据此合成请求）
    #[serde(default)]
    action: String,
    #[serde(default)]
    form: Value,
    // 上传/扫描续算数据
    #[serde(default)]
    cfg: Value,
    #[serde(default)]
    index: Vec<ImageItem>,
    #[serde(default)]
    root: Option<ImageRoot>,
    #[serde(default)]
    dir: String,
    #[serde(default)]
    files: Vec<MultipartFile>,
    #[serde(rename = "sha1s", default)]
    sha1s: Vec<String>,
    #[serde(rename = "sizes", default)]
    sizes: Vec<i64>,
}

fn base_state(http: &Option<HttpReq>, now: &Value, req_id: &str) -> Value {
    json!({ "http": http, "now": now, "reqId": req_id })
}

/// 面板动作模式的状态：额外带上动作名与表单快照
fn action_state(http: &Option<HttpReq>, now: &Value, req_id: &str, action: &str, form: &Value) -> Value {
    let mut st = base_state(http, now, req_id);
    st["action"] = json!(action);
    st["form"] = form.clone();
    st
}

// ---------------- 入口 ----------------

fn main() {
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return;
    }
    let resp = handle(&input).unwrap_or_else(|e| json!({"id": 1, "error": {"message": e}}));
    let mut out = std::io::stdout();
    let _ = out.write_all(&serde_json::to_vec(&resp).unwrap_or_default());
    let _ = out.flush();
}

fn handle(input: &[u8]) -> Result<Value, String> {
    let req: Value = serde_json::from_slice(input).map_err(|e| format!("请求解析失败: {e}"))?;
    let id = req.get("id").and_then(|v| v.as_u64()).unwrap_or(1);
    let method = s(req.get("method"));
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    match method.as_str() {
        "manifest" => Ok(json!({
            "id": id,
            "manifest": {
                "id": "imagehost",
                "name": "图床",
                "version": env!("CARGO_PKG_VERSION"),
                "description": "基于 rclone 的图床：上传、去重、索引、直链代理",
            }
        })),
        "request" => handle_request(id, params),
        other => Err(format!("未知 method: {other}")),
    }
}

fn ok(id: u64, body: Value) -> Value {
    json!({"id": id, "ops": [], "result": {"status": 200, "body": body}})
}

fn ok_with_ops(id: u64, ops: Vec<Value>, body: Value) -> Value {
    json!({"id": id, "ops": ops, "result": {"status": 200, "body": body}})
}

fn err(id: u64, status: u16, msg: &str) -> Value {
    json!({"id": id, "ops": [], "result": {"status": status, "body": {"error": msg}}})
}

// ---------------- 面板动作模式 ----------------
// 宿主渲染 config.json 里的控件，按下按钮时把控件 key 当 action 递进来。
// 这里把每个 action 翻译成等价的 HTTP 请求，交给既有路由处理，再把结果摊成
// 宿主认识的 status / lamp / checks / hints / values / toast。

/// 面板能出现的全部 action（config.json 的 key + 协议保留字）
const ACTIONS: &[&str] = &[
    "state",
    "autostart",
    "save",
    "reload",
    "refresh",
    "scan",
    "upload",
    "add-token",
    "copy-link",
    "refresh-remotes",
    "browse",
    "shutdown",
    "exit",
    "http",
];

/// 统一的面板响应：ops 照旧带上，body 由各动作自己填
fn panel_response(id: u64, ops: Vec<Value>, status: u16, body: Value) -> Value {
    json!({
        "id": id,
        "ops": ops,
        "result": {"status": status, "body": body},
    })
}

fn human_size(n: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n.max(0) as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn guess_content_type(path: &str) -> String {
    let ext = split_ext(path).1.to_ascii_lowercase();
    let ct = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    ct.to_string()
}

/// 宿主可能把本机服务地址放在 params.live 里（字符串，或 {"origin": ...}）
fn live_origin(live: Option<&Value>) -> String {
    let v = match live {
        Some(v) if !v.is_null() => v,
        _ => return String::new(),
    };
    let o = s(v.get("origin"));
    if !o.is_empty() {
        return o.trim_end_matches('/').to_string();
    }
    if let Some(x) = v.as_str() {
        if x.starts_with("http") {
            return x.trim_end_matches('/').to_string();
        }
    }
    String::new()
}

/// GET /config 的 body 与 PUT /config 的入参同形，回读时统一走这里
fn cfg_from_body(body: &Value, fallback: &ImagehostConfig) -> ImagehostConfig {
    let mut c: ImagehostConfig =
        serde_json::from_value(body.clone()).unwrap_or_else(|_| fallback.clone());
    if c.roots.is_empty() {
        c.roots = fallback.roots.clone();
    }
    c.normalize();
    c
}

/// 把面板表单（值全是字符串）拼成 PUT /config 的 body
fn form_to_config_body(form: &Value, cfg: &ImagehostConfig) -> Value {
    let get = |k: &str| s(form.get(k)).trim().to_string();
    // 表单里压根没这个键才沿用旧值；给了但被清空就算真空（PUT /config 会报错提醒）
    let mut roots = match form.get("roots") {
        None => cfg.roots.clone(),
        Some(_) => parse_roots(&get("roots"), &cfg.roots),
    };
    // 「存储的远程 + 路径」是默认根的人话视图，和现状不同才覆盖
    apply_store_fields(&mut roots, &get("remote"), &get("path"));
    let naming = {
        let m = get("naming");
        if m.is_empty() {
            cfg.naming.mode.clone()
        } else {
            m
        }
    };
    let seq_pad = {
        let p = get("seqpad");
        let n = p.parse::<i64>().unwrap_or(cfg.naming.seq_pad);
        n.clamp(1, 9)
    };
    let dedupe = {
        let d = get("dedupe");
        if d.is_empty() {
            cfg.dedupe
        } else {
            d == "true" || d == "1" || d == "yes"
        }
    };
    let layout = {
        let o = get("obsidian");
        if o.is_empty() {
            cfg.obsidian.dir_layout.clone()
        } else {
            o
        }
    };
    let mut c = cfg.clone();
    c.roots = roots;
    c.naming.mode = naming;
    c.naming.seq_pad = seq_pad;
    c.dedupe = dedupe;
    c.obsidian.dir_layout = layout;
    c.trash_dir = get("trash");
    c.base = get("base").trim_end_matches('/').to_string();
    c.normalize();
    c.to_value()
}

/// 把面板动作翻译成等价 HTTP 请求；None 表示该动作没有对应路由
fn synth_http(
    action: &str,
    form: &Value,
    cfg: &ImagehostConfig,
    provided: &Value,
    origin: &str,
) -> Result<Option<HttpReq>, String> {
    let get = |k: &str| s(form.get(k)).trim().to_string();
    // 面板走的是本机 ui，没有 HTTP 令牌；这里带一个合法令牌，避开 check_token
    let authorization = cfg
        .tokens
        .first()
        .map(|t| format!("Bearer {}", t.token))
        .unwrap_or_default();
    let mut req = HttpReq {
        method: "GET".to_string(),
        path: "/config".to_string(),
        query: Map::new(),
        authorization,
        body_b64: String::new(),
        multipart: None,
        origin: origin.to_string(),
    };
    let body = match action {
        "state" | "autostart" | "reload" => Value::Null,
        "save" => {
            req.method = "PUT".to_string();
            form_to_config_body(form, cfg)
        }
        "scan" | "refresh" => {
            req.method = "POST".to_string();
            req.path = "/scan".to_string();
            json!({"rootId": get("root")})
        }
        "add-token" => {
            req.method = "POST".to_string();
            req.path = "/token".to_string();
            let label = get("tokenlabel");
            json!({"name": label, "label": label})
        }
        "upload" => {
            req.method = "POST".to_string();
            req.path = "/upload".to_string();
            // 第一轮已经请宿主把文件读成 base64，这里拼成 multipart
            let cap = unwrap_result(&provided.get("file").cloned().unwrap_or(Value::Null));
            let data_b64 = s(cap.get("data_b64"));
            if data_b64.is_empty() {
                return Err("没读到文件内容".to_string());
            }
            let path = get("file");
            let filename = {
                let n = basename_of(&path);
                if n.is_empty() {
                    "upload.png".to_string()
                } else {
                    n
                }
            };
            let mut fields = Map::new();
            fields.insert("rootId".to_string(), Value::String(get("root")));
            fields.insert("subPath".to_string(), Value::String(get("subdir")));
            for (k, name) in [("naming", "naming"), ("seqpad", "seqPad")] {
                let v = get(k);
                if !v.is_empty() {
                    fields.insert(name.to_string(), Value::String(v));
                }
            }
            req.multipart = Some(Multipart {
                fields,
                files: vec![MultipartFile { filename, data_b64 }],
            });
            Value::Null
        }
        _ => return Ok(None),
    };
    req.body_b64 = if body.is_null() {
        String::new()
    } else {
        b64encode(body.to_string().as_bytes())
    };
    Ok(Some(req))
}

/// 面板首屏的通用 body：一句话状态 + 指示灯 + 明细 + 提示 + 表单回填
fn panel_state_body(cfg: &ImagehostConfig, index: &[ImageItem], extra: i64) -> Value {
    let bytes: i64 = index.iter().map(|i| i.size.max(0)).sum();
    let count = index.len() as i64 + extra.max(0);
    let mut checks = vec![
        json!({"label": "存储根", "value": roots_to_text(&cfg.roots), "ok": !cfg.roots.is_empty()}),
        json!({"label": "已索引", "value": format!("{} 张 · {}", count, human_size(bytes)), "ok": count > 0}),
        json!({
            "label": "公网地址",
            "value": if cfg.base.is_empty() { "未设置（用本机服务）".to_string() } else { cfg.base.clone() },
            "ok": !cfg.base.is_empty(),
        }),
    ];
    for r in cfg.roots.iter().take(5) {
        let n = index.iter().filter(|i| i.root_id == r.id).count();
        checks.push(json!({
            "label": format!("根 {}", r.id),
            "value": format!("{} · {} 张", r.remote, n),
            "ok": n > 0,
        }));
    }
    let lamp = if cfg.roots.is_empty() {
        "err"
    } else if count == 0 {
        "warn"
    } else {
        "ok"
    };
    json!({
        "status": format!("{} 个存储根 · {} 张图片", cfg.roots.len(), count),
        "lamp": lamp,
        "checks": checks,
        "hints": {"tip": "直链形如 <公网地址>/i/<根 id>/<路径>；根 id 可在存储根里自定义。"},
        "values": config_values(cfg),
    })
}

/// 宿主把 rclone.call 的结果放在 `provided.<key>`，形状不固定：裸数组、
/// `{"list":[…]}`, `{"remotes":[…]}`, 或外面再包一层 `{"result":{…}}`。
fn call_list(v: &Value) -> Vec<Value> {
    if v.is_null() {
        return Vec::new();
    }
    let u = unwrap_result(v);
    if let Some(a) = u.as_array() {
        return a.clone();
    }
    for k in ["list", "remotes", "dirs"] {
        if let Some(a) = u.get(k).and_then(|x| x.as_array()) {
            return a.clone();
        }
    }
    Vec::new()
}

/// provided.remotes → 去重后的远程名列表（裸名字，不带 `:类型`）
fn remotes_names(provided: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in call_list(&provided.get("remotes").cloned().unwrap_or(Value::Null)) {
        let raw = match item {
            Value::String(x) => x,
            other => s(Some(&other)).to_string(),
        };
        // rclone 的 config/listremotes 回 "名字:类型"；面板里只认裸名字
        let name = raw.trim().split(':').next().unwrap_or("").trim().to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// 刷新远程：`rclone config/listremotes` 的结果 → 面板下拉候选
fn remotes_body(id: u64, provided: &Value, form: &Value) -> Value {
    let remotes = remotes_names(provided);
    let cur = s(form.get("remote")).trim().to_string();
    let picked = if remotes.contains(&cur) {
        cur
    } else {
        remotes.first().cloned().unwrap_or_default()
    };    let empty = remotes.is_empty();
    panel_response(
        id,
        vec![],
        200,
        json!({
            "status": if empty {
                "没读到远程：rclone 没在运行，也读不到 rclone.conf".to_string()
            } else {
                format!("{} 个远程", remotes.len())
            },
            "lamp": if empty { "warn" } else { "ok" },
            "choices": {"remote": remotes},
            "values": {"remote": picked},
            "hints": {"tip": "远程列表由本体提供：先问 rclone 的 RC，问不到再只读解析 rclone.conf。选好远程后填路径，点「浏览」会列出它的子目录。"},
        }),
    )
}

/// 浏览路径：列出当前路径下的子目录（外加回到上一层），塞进 path 字段的 ▾
fn dirs_body(id: u64, provided: &Value, form: &Value) -> Value {
    let path = normalize_sub(&s(form.get("path")));
    let mut cands: Vec<String> = Vec::new();
    let parent = dirname_of(&path);
    if !path.is_empty() && !parent.is_empty() && parent != path {
        cands.push(parent);
    }
    let mut n = 0i64;
    for item in call_list(&provided.get("dirs").cloned().unwrap_or(Value::Null)) {
        if !item
            .get("IsDir")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let name = s(item.get("Name")).trim().to_string();
        if name.is_empty() {
            continue;
        }
        let next = join_nonempty(&[&path, &name]);
        if !cands.contains(&next) {
            cands.push(next);
            n += 1;
        }
    }
    panel_response(
        id,
        vec![],
        200,
        json!({
            "status": if n == 0 {
                format!("{} 下没有子目录", if path.is_empty() { "远端根目录".to_string() } else { path.clone() })
            } else {
                format!("{n} 个子目录")
            },
            "lamp": if n == 0 { "warn" } else { "ok" },
            "choices": {"path": cands},
            "values": {"path": path},
            "hints": {"tip": "点路径框右侧 ▾ 选子目录（含上一层）；回到根目录把它清空即可。"},
        }),
    )
}

/// 上传成功后面板该显示什么
fn panel_upload_body(body: &Value) -> Value {
    let items = body
        .get("items")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let first = items.first().cloned().unwrap_or(Value::Null);
    let link = s(first.get("url"));
    if link.is_empty() {
        return json!({"status": "上传完成，但没拿到直链", "lamp": "warn"});
    }
    let deduped = first.get("deduped").and_then(|x| x.as_bool()).unwrap_or(false);
    let absolute = link.starts_with("http");
    let mut b = json!({
        "status": link,
        "lamp": if absolute { "ok" } else { "warn" },
        "values": {"lastlink": link},
        "checks": [
            {"label": "文件", "value": s(first.get("name")), "ok": true},
            {"label": "存储路径", "value": s(first.get("storedPath")), "ok": true},
            {"label": "直链", "value": link, "ok": absolute},
        ],
        "hints": {"tip": "Markdown 里直接粘贴即可引用；直链不是 http 开头说明还没填公网地址。"},
        "toast": {"level": "info", "message": if deduped { "同内容的图片已存在，直接复用" } else { "上传成功" }},
    });
    if !absolute {
        b["status"] = json!(format!("{link}（相对直链，填了公网地址才是完整链接）"));
    }
    b
}

/// 路由结果 → 面板 body（未知 action 一律走通用首屏）
fn to_panel(action: &str, status: u16, body: Value, cfg: &ImagehostConfig, index: &[ImageItem]) -> Value {
    if status != 200 {
        let msg = {
            let e = s(body.get("error"));
            if e.is_empty() {
                format!("操作失败（{status}）")
            } else {
                e
            }
        };
        return json!({
            "status": msg,
            "lamp": "err",
            "toast": {"level": "error", "message": msg},
        });
    }
    match action {
        "save" => {
            let c = cfg_from_body(&body, cfg);
            json!({
                "status": "已保存",
                "lamp": "ok",
                "values": config_values(&c),
                "checks": [
                    {"label": "存储根", "value": roots_to_text(&c.roots), "ok": !c.roots.is_empty()},
                    {"label": "命名", "value": c.naming.mode, "ok": true},
                    {"label": "去重", "value": if c.dedupe { "开" } else { "关" }, "ok": true},
                ],
                "toast": {"level": "info", "message": "已保存"},
            })
        }
        "scan" | "refresh" => {
            let added = body.get("added").and_then(|x| x.as_i64()).unwrap_or(0);
            let total = body.get("total").and_then(|x| x.as_i64()).unwrap_or(0);
            let mut b = panel_state_body(cfg, index, added);
            b["status"] = json!(format!("已扫描：新增 {added} 张 · 根内共 {total} 张"));
            b["toast"] = json!({"level": "info", "message": format!("新增 {added} 张图片")});
            b
        }
        "upload" => panel_upload_body(&body),
        "add-token" => {
            let token = s(body.get("token"));
            let msg = format!("已生成令牌：{token}");
            json!({
                "status": msg,
                "lamp": "ok",
                "values": {"tokenlabel": ""},
                "checks": [{"label": "令牌", "value": token, "ok": true}],
                "hints": {"token-tip": "客户端请求时带上 Authorization: Bearer <令牌>。"},
                "toast": {"level": "info", "message": "令牌已生成，记得保存"},
            })
        }
        _ => panel_state_body(cfg, index, 0),
    }
}

/// 第一轮：先把 kv 要过来（配置和索引都在里面）；上传动作顺手把文件读成 base64
fn first_round(id: u64, params: &Value, action: &str) -> Result<Value, String> {
    let now = normalize_now(&params.get("now").cloned().unwrap_or(Value::Null));
    let req_id = {
        let a = s(params.get("req_id"));
        if a.is_empty() {
            s(params.get("reqId"))
        } else {
            a
        }
    };
    let form = params.get("form").cloned().unwrap_or(json!({}));
    let mut http: Option<HttpReq> = match params.get("http") {
        Some(v) if !v.is_null() => Some(
            serde_json::from_value(v.clone()).map_err(|e| format!("http 参数解析失败: {e}"))?,
        ),
        _ => None,
    };
    if action == "http" && http.is_none() {
        return Ok(err(id, 400, "缺少 http 参数"));
    }
    // 动作模式宿主不给 http，但直链要用 origin：看 params.live 里有没有
    if http.is_none() {
        let origin = live_origin(params.get("live"));
        if !origin.is_empty() {
            http = Some(HttpReq {
                method: "GET".to_string(),
                path: "/config".to_string(),
                query: Map::new(),
                authorization: String::new(),
                body_b64: String::new(),
                multipart: None,
                origin,
            });
        }
    }
    let mut need = vec![json!({"cap": "kv.get_all", "key": "kv", "params": {}})];
    if action == "upload" {
        let file = s(form.get("file")).trim().to_string();
        if file.is_empty() {
            return Ok(panel_response(
                id,
                vec![],
                400,
                json!({
                    "status": "请先选择文件",
                    "lamp": "err",
                    "toast": {"level": "warn", "message": "请先选择文件"},
                }),
            ));
        }
        // 宿主把文件读成 base64 递回来（上限 64MB）
        need.push(json!({
            "cap": "fs.read_b64",
            "key": "file",
            "params": {"path": file, "max_bytes": 67108864},
        }));
    }
    // 「刷新远程」「浏览路径」：直接问宿主/ rclone 要列表，拿回来填面板下拉；
    // 面板首屏（state）也顺手取一次远程列表，否则下拉框里只有当前值。
    if action == "refresh-remotes" || action == "state" {
        need.push(json!({
            "cap": "rclone.remotes",
            "key": "remotes",
            "params": {},
        }));
    }
    if action == "browse" {
        let remote = s(form.get("remote")).trim().to_string();
        let path = s(form.get("path")).trim().to_string();
        if remote.is_empty() {
            return Ok(panel_response(
                id,
                vec![],
                400,
                json!({
                    "status": "先选一个远程，再浏览路径",
                    "lamp": "warn",
                    "toast": {"level": "warn", "message": "先选一个远程，再浏览路径"},
                }),
            ));
        }
        need.push(json!({
            "cap": "rclone.call",
            "key": "dirs",
            "params": {
                "method": "operations/list",
                "params": {"fs": join_spec(&remote, &path), "remote": "", "options": {"dirsOnly": true}},
            },
        }));
    }
    Ok(json!({
        "id": id,
        "need": need,
        "state": action_state(&http, &now, &req_id, action, &form),
    }))
}

/// 复制直链：没有对应路由，直接请宿主写剪贴板
fn copy_link_phase(
    id: u64,
    form: &Value,
    state: &State,
    now: &Value,
    req_id: &str,
    provided: &Value,
) -> Value {
    let done = provided.get("copy").cloned().unwrap_or(Value::Null);
    if done.is_null() {
        let link = s(form.get("lastlink")).trim().to_string();
        if link.is_empty() {
            return panel_response(
                id,
                vec![],
                400,
                json!({
                    "status": "还没有可复制的直链",
                    "lamp": "warn",
                    "toast": {"level": "warn", "message": "先上传一张图片，再来复制"},
                }),
            );
        }
        let mut st = action_state(&state.http, now, req_id, "copy-link", form);
        st["copy"] = json!(link);
        return json!({
            "id": id,
            "need": [{"cap": "ui.copy", "key": "copy", "params": {"text": link}}],
            "state": st,
        });
    }
    let e = s(done.get("error"));
    if !e.is_empty() {
        return panel_response(
            id,
            vec![],
            500,
            json!({"status": e, "lamp": "warn", "toast": {"level": "error", "message": e}}),
        );
    }
    let link = s(form.get("lastlink")).trim().to_string();
    panel_response(
        id,
        vec![],
        200,
        json!({
            "status": "已复制",
            "lamp": "ok",
            "values": {"lastlink": link},
            "toast": {"level": "info", "message": "直链已复制到剪贴板"},
        }),
    )
}

/// 老协议：宿主直接给一条 HTTP 请求
/// 第一轮：请求 kv；第二轮起：路由分发（第三轮带 provided.entries）
fn handle_request(id: u64, params: Value) -> Result<Value, String> {
    let provided = params.get("provided").cloned().unwrap_or(Value::Null);
    // 老宿主只发 {"http":{...}}，新宿主发 {"action":"save","form":{...}}
    let action = {
        let a = s(params.get("action"));
        if !a.is_empty() {
            a
        } else if params.get("http").map(|v| !v.is_null()).unwrap_or(false) {
            "http".to_string()
        } else {
            String::new()
        }
    };

    // 宿主自己退出：插件什么都不用做，必须立刻返回、不申请任何 cap
    if action == "shutdown" || action == "exit" {
        return Ok(panel_response(id, vec![], 200, json!({"status": "ok"})));
    }
    if !ACTIONS.contains(&action.as_str()) {
        let msg = if action.is_empty() {
            "未知操作".to_string()
        } else {
            format!("未知操作：{action}")
        };
        return Ok(panel_response(
            id,
            vec![],
            400,
            json!({"status": "未知操作", "lamp": "err", "toast": {"level": "error", "message": msg}}),
        ));
    }

    // 有的宿主第一轮也会塞一个空的 provided，等价于没给
    let provided_empty = provided.is_null()
        || provided
            .as_object()
            .map(|m| m.is_empty())
            .unwrap_or(false);
    if provided_empty {
        return first_round(id, &params, &action);
    }

    let kv = provided.get("kv").cloned().unwrap_or(json!({}));
    // 两个「只读 rclone 列表」的动作在拿到 cap 结果后就完事，不用再合成 HTTP 请求
    if action == "refresh-remotes" || action == "browse" {
        let form = params
            .get("form")
            .cloned()
            .or_else(|| params.get("state").and_then(|s| s.get("form")).cloned())
            .unwrap_or(json!({}));
        return Ok(if action == "refresh-remotes" {
            remotes_body(id, &provided, &form)
        } else {
            dirs_body(id, &provided, &form)
        });
    }
    let entries: Option<Vec<Value>> = match provided.get("entries") {
        Some(v) => {
            // rclone.call 的结果可能是裸数组、{"list":[...]} 或 {"result":{"list":[...]}}
            let raw = if v.is_array() {
                v.clone()
            } else {
                let u = unwrap_result(v);
                if u.is_array() {
                    u
                } else {
                    u.get("list").cloned().unwrap_or(Value::Null)
                }
            };
            Some(serde_json::from_value(raw).map_err(|e| format!("entries 解析失败: {e}"))?)
        }
        None => None,
    };
    let state: State = serde_json::from_value(params.get("state").cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("state 解析失败: {e}"))?;
    let now = state.now.clone();
    let req_id = if state.req_id.is_empty() {
        s(params.get("req_id"))
    } else {
        state.req_id.clone()
    };
    // 第三轮宿主可能只回 provided，表单要从 state 里捡回来
    let form = {
        let f = params.get("form").cloned().unwrap_or(Value::Null);
        if f.is_null() {
            state.form.clone()
        } else {
            f
        }
    };
    // 第三轮 kv 不在 provided 中，优先用 state 快照（第二轮保存）
    let cfg = match serde_json::from_value::<ImagehostConfig>(state.cfg.clone()) {
        Ok(c) => c,
        Err(_) => load_config(&kv),
    };
    let index = if state.index.is_empty() {
        load_index(&kv)
    } else {
        state.index.clone()
    };

    if action == "http" {
        let http = state.http.clone().ok_or("缺少 http 参数")?;
        return dispatch_http(id, &http, cfg, index, state, entries, &provided);
    }
    if action == "copy-link" {
        return Ok(copy_link_phase(id, &form, &state, &now, &req_id, &provided));
    }
    // 其余动作：翻译成等价 HTTP 请求，复用既有路由
    let origin = state
        .http
        .as_ref()
        .map(|h| h.origin.clone())
        .unwrap_or_default();
    // 上传动作：第三轮宿主未必再回 fs.read_b64，用 state 里存的文件兜底
    let mut caps = provided.clone();
    if action == "upload" && caps.get("file").is_none() {
        if let Some(f) = state.files.first() {
            let size = b64decode(&f.data_b64).map(|d| d.len()).unwrap_or(0);
            caps["file"] = json!({"data_b64": f.data_b64, "size": size});
        }
    }
    let http = match synth_http(&action, &form, &cfg, &caps, &origin) {
        Ok(Some(h)) => h,
        Ok(None) => {
            return Ok(panel_response(
                id,
                vec![],
                400,
                json!({"status": "未知操作", "lamp": "err"}),
            ));
        }
        Err(e) => {
            return Ok(panel_response(
                id,
                vec![],
                400,
                json!({
                    "status": e,
                    "lamp": "err",
                    "toast": {"level": "error", "message": e},
                }),
            ));
        }
    };
    let res = dispatch_http(id, &http, cfg.clone(), index.clone(), state, entries, &provided)?;
    if res.get("need").is_some() {
        // 还有下一轮（扫描/上传要问 entries），原样递回去
        return Ok(res);
    }
    let status = res
        .pointer("/result/status")
        .and_then(|x| x.as_u64())
        .unwrap_or(500) as u16;
    let body = res.pointer("/result/body").cloned().unwrap_or(Value::Null);
    let ops = res
        .get("ops")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let mut panel = to_panel(&action, status, body, &cfg, &index);
    // 首屏：把 rclone 的远程列表塞进「存储的远程」下拉
    if action == "state" {
        let names = remotes_names(&provided);
        if !names.is_empty() {
            // 已经填好的值优先：配置里默认根的远程 > 表单里的值 > 列表第一个。
            // 顺序不能反 —— 表单里可能还是上一轮存下的空串，直接取首个候选会把
            // 用户配好的远程悄悄改掉。
            let known = s(panel.pointer("/values/remote")).trim().to_string();
            let from_form = s(form.get("remote")).trim().to_string();
            let pick = [known, from_form]
                .into_iter()
                .find(|v| !v.is_empty() && names.iter().any(|n| n == v))
                .unwrap_or_else(|| names[0].clone());
            panel["choices"] = json!({"remote": names});
            if !pick.is_empty() {
                panel["values"]["remote"] = json!(pick);
            }
        }
    }
    Ok(panel_response(id, ops, status, panel))
}

/// 既有 HTTP 路由表：http 模式原样调用，动作模式由 synth_http 合成的请求调用
fn dispatch_http(
    id: u64,
    http: &HttpReq,
    mut cfg: ImagehostConfig,
    mut index: Vec<ImageItem>,
    state: State,
    entries: Option<Vec<Value>>,
    provided: &Value,
) -> Result<Value, String> {
    let now = state.now.clone();
    let req_id = state.req_id.clone();
    let path = http.path.trim().trim_end_matches('/').to_string();
    let path = if path.is_empty() { "/".to_string() } else { path };
    let method = http.method.to_uppercase();

    // token 校验（配置了 token 时，仅对上传/扫描生效）
    let needs_auth = !cfg.tokens.is_empty() && matches!((method.as_str(), path.as_str()), ("POST", "/upload") | ("POST", "/scan"));
    if needs_auth && !check_token(&http.authorization, &cfg) {
        return Ok(err(id, 401, "unauthorized"));
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/config") => Ok(ok(id, cfg.to_value())),
        ("PUT", "/config") => put_config(id, http, cfg),
        ("POST", "/token") => post_token(id, http, &mut cfg, &req_id, &now),
        ("DELETE", p) if p.starts_with("/token/") => delete_token(id, &p["/token/".len()..], cfg),
        ("GET", "/files") => Ok(ok(id, query_files(&index, &http.query))),
        ("GET", "/stats") => Ok(ok(id, stats(&index))),
        ("POST", "/files/delete") => files_delete(id, http, &cfg, &mut index, &now),
        ("POST", "/files/rename") => files_rename(id, http, &cfg, &mut index),
        ("POST", "/scan") => scan_phase(id, http, cfg, index, state, entries),
        ("POST", "/upload") => upload_phase(id, http, cfg, index, state, entries),
        ("GET", p) if p.starts_with("/i/") => image_phase(id, p, &cfg, &state, provided),
        _ => Ok(err(id, 404, "路由不存在")),
    }
}

/// GET /i/<root>/<path>：让宿主 rclone.cat 把远端文件读成 base64 再回给浏览器
fn image_phase(
    id: u64,
    path: &str,
    cfg: &ImagehostConfig,
    state: &State,
    provided: &Value,
) -> Result<Value, String> {
    // 直链可能带查询串或锚点（?v=1），这些不属于路径
    let rest = path["/i/".len()..]
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_string();
    let (root_id, raw_rel) = match rest.split_once('/') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (rest.to_string(), String::new()),
    };
    let root = match cfg.roots.iter().find(|r| r.id == root_id) {
        Some(r) => r,
        None => return Ok(err(id, 404, "root 不存在")),
    };
    // ../ 之类越界路径一律拒，别让一个直链读到 remote 外面去
    let rel = match normalize_rel_path(&raw_rel) {
        Ok(p) => p,
        Err(e) => return Ok(err(id, 400, &e)),
    };
    if rel.is_empty() {
        return Ok(err(id, 400, "图片路径不能为空"));
    }
    let img = provided.get("img").cloned().unwrap_or(Value::Null);
    if img.is_null() {
        // 第二轮：请宿主把远端文件读回来
        let st = base_state(&state.http, &state.now, &state.req_id);
        return Ok(json!({
            "id": id,
            "need": [{
                "cap": "rclone.cat",
                "key": "img",
                "params": {"remote": root.remote, "path": rel, "max_bytes": 33554432},
            }],
            "state": st,
        }));
    }
    let img = unwrap_result(&img);
    let e = s(img.get("error"));
    if !e.is_empty() {
        return Ok(err(id, 404, &e));
    }
    let data_b64 = s(img.get("data_b64"));
    if data_b64.is_empty() {
        return Ok(err(id, 404, "图片不存在"));
    }
    let ct = {
        let c = s(img.get("content_type"));
        if c.is_empty() {
            guess_content_type(&rel)
        } else {
            c
        }
    };
    Ok(image_ok(id, &ct, &data_b64))
}

/// 图片响应：宿主可能读 result.content_type，也可能读 result.body，两层都填
fn image_ok(id: u64, content_type: &str, body_b64: &str) -> Value {
    json!({
        "id": id,
        "ops": [],
        "result": {
            "status": 200,
            "content_type": content_type,
            "body_b64": body_b64,
            "body": {"status": 200, "content_type": content_type, "body_b64": body_b64},
        },
    })
}

fn check_token(authorization: &str, cfg: &ImagehostConfig) -> bool {
    let t = authorization
        .strip_prefix("Bearer ")
        .or_else(|| authorization.strip_prefix("bearer "))
        .map(|x| x.trim().to_string())
        .unwrap_or_default();
    !t.is_empty() && cfg.tokens.iter().any(|x| x.token == t)
}

// ---------------- /config ----------------

fn put_config(id: u64, http: &HttpReq, old: ImagehostConfig) -> Result<Value, String> {
    let body = b64decode(&http.body_b64)?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("请求体解析失败: {e}"))?;
    let roots_empty = v.get("roots").and_then(|r| r.as_array()).map(|a| a.is_empty()).unwrap_or(true);
    if roots_empty {
        return Ok(err(id, 400, "至少需要一个 root"));
    }
    let roots: Vec<ImageRoot> = match serde_json::from_value(v.get("roots").cloned().unwrap_or(Value::Null)) {
        Ok(r) => r,
        Err(_) => return Ok(err(id, 400, "root 需要 id 与 remote")),
    };
    for r in &roots {
        if r.id.is_empty() || r.remote.is_empty() {
            return Ok(err(id, 400, "root 需要 id 与 remote"));
        }
    }
    let naming = v.get("naming").cloned().unwrap_or(Value::Null);
    let mode = {
        let m = s(naming.get("mode"));
        if ["timestamp", "seq", "keep", "hash"].contains(&m.as_str()) { m } else { old.naming.mode.clone() }
    };
    let cfg = ImagehostConfig {
        roots,
        naming: Naming {
            mode,
            seq_pad: naming.get("seqPad").and_then(|x| x.as_i64()).unwrap_or(old.naming.seq_pad),
        },
        dedupe: v.get("dedupe").and_then(|x| x.as_bool()).unwrap_or(old.dedupe),
        trash_dir: if v.get("trashDir").is_some() { s(v.get("trashDir")) } else { old.trash_dir.clone() },
        obsidian: ObsidianCfg {
            dir_layout: {
                let l = s(v.get("obsidian").and_then(|o| o.get("dirLayout")));
                if l.is_empty() { old.obsidian.dir_layout.clone() } else { l }
            },
        },
        tokens: match v.get("tokens") {
            Some(Value::Array(_)) => {
                serde_json::from_value(v.get("tokens").cloned().unwrap_or(Value::Null)).unwrap_or(old.tokens.clone())
            }
            _ => old.tokens.clone(),
        },
        base: if v.get("base").is_some() {
            s(v.get("base")).trim().to_string()
        } else {
            old.base.clone()
        },
        // 直链不是配置项，跟着老值走；只有上传成功才会改写
        lastlink: old.lastlink.clone(),
    };
    let mut cfg = cfg;
    cfg.normalize();
    Ok(ok_with_ops(id, vec![json!({"op": "kv.set", "key": "config", "value": cfg.to_value()})], cfg.to_value()))
}

// ---------------- /token ----------------

fn post_token(id: u64, http: &HttpReq, cfg: &mut ImagehostConfig, req_id: &str, now: &Value) -> Result<Value, String> {
    let body = b64decode(&http.body_b64)?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let name = {
        let n = s(v.get("name"));
        if n.trim().is_empty() { "未命名".to_string() } else { n }
    };
    // 随机性来自宿主 req_id（uuid hex）+ 时间戳；epoch_ms 可能是数字，也可能是字符串
    let raw = |k: &str| now.get(k).map(|x| x.to_string().trim_matches('"').to_string()).unwrap_or_default();
    let mut stamp = raw("epoch_ms");
    if stamp.is_empty() {
        stamp = raw("epoch");
    }
    let mut seed = format!("{req_id}{stamp}");
    seed.truncate(24);
    // 宿主既没给 req_id 也没给时间戳：至少别生成空令牌
    if seed.is_empty() {
        seed = s(now.get("iso")).replace(['-', ':', '+', 'T'], "");
        seed.truncate(24);
    }
    if seed.is_empty() {
        seed = "token".to_string();
    }
    let token = Token {
        id: req_id.to_string(),
        name,
        token: seed,
        created_at: s(now.get("iso")),
    };
    cfg.tokens.push(token.clone());
    Ok(ok_with_ops(
        id,
        vec![json!({"op": "kv.set", "key": "config", "value": cfg.to_value()})],
        serde_json::to_value(&token).unwrap_or(Value::Null),
    ))
}

fn delete_token(id: u64, tid: &str, mut cfg: ImagehostConfig) -> Result<Value, String> {
    let before = cfg.tokens.len();
    cfg.tokens.retain(|t| t.id != tid);
    if cfg.tokens.len() == before {
        return Ok(err(id, 404, "token 不存在"));
    }
    Ok(ok_with_ops(
        id,
        vec![json!({"op": "kv.set", "key": "config", "value": cfg.to_value()})],
        json!({"ok": true}),
    ))
}

// ---------------- /files 查询与统计 ----------------

fn query_files(index: &[ImageItem], query: &Map<String, Value>) -> Value {
    let get_q = |k: &str| s(query.get(k));
    let root_id = get_q("rootId");
    let q = get_q("q").to_lowercase();
    let page = get_q("page").parse::<usize>().unwrap_or(1).max(1);
    let page_size = get_q("pageSize").parse::<usize>().unwrap_or(60).clamp(1, 200);

    let mut list: Vec<&ImageItem> = index
        .iter()
        .filter(|i| root_id.is_empty() || i.root_id == root_id)
        .filter(|i| q.is_empty() || i.name.to_lowercase().contains(&q) || i.path.to_lowercase().contains(&q))
        .collect();
    list.sort_by(|a, b| b.uploaded_at.cmp(&a.uploaded_at));
    let total = list.len();
    let items: Vec<&ImageItem> = list
        .into_iter()
        .skip((page - 1) * page_size)
        .take(page_size)
        .collect();
    json!({
        "items": items,
        "total": total,
        "page": page,
        "pageSize": page_size,
    })
}

fn stats(index: &[ImageItem]) -> Value {
    let mut map: std::collections::BTreeMap<String, (i64, i64)> = std::collections::BTreeMap::new();
    let mut count = 0i64;
    let mut bytes = 0i64;
    for i in index {
        let e = map.entry(i.root_id.clone()).or_insert((0, 0));
        e.0 += 1;
        e.1 += i.size;
        count += 1;
        bytes += i.size;
    }
    let roots: Vec<Value> = map
        .into_iter()
        .map(|(root_id, (c, b))| json!({"rootId": root_id, "count": c, "bytes": b}))
        .collect();
    json!({"count": count, "bytes": bytes, "roots": roots})
}

// ---------------- root 选择 ----------------

fn pick_root<'a>(cfg: &'a ImagehostConfig, root_id: &str) -> Option<&'a ImageRoot> {
    if !root_id.is_empty() {
        return cfg.roots.iter().find(|r| r.id == root_id);
    }
    cfg.roots.iter().find(|r| r.is_default).or_else(|| cfg.roots.first())
}

// ---------------- /files/delete、/files/rename ----------------

fn files_delete(id: u64, http: &HttpReq, cfg: &ImagehostConfig, index: &mut Vec<ImageItem>, now: &Value) -> Result<Value, String> {
    let body = b64decode(&http.body_b64)?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let root_id = s(v.get("rootId"));
    let root = match pick_root(cfg, &root_id) {
        Some(r) => r,
        None => return Ok(err(id, 400, "root 不存在")),
    };
    let paths: Vec<String> = v
        .get("paths")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|y| y.to_string())).collect())
        .unwrap_or_default();
    if paths.is_empty() {
        return Ok(err(id, 400, "paths 不能为空"));
    }
    let to_trash = v.get("toTrash").and_then(|x| x.as_bool()).unwrap_or(false) && !cfg.trash_dir.is_empty();
    let today = s(now.get("date_iso"));
    let mut deleted = Vec::new();
    let mut ops = Vec::new();
    for p in &paths {
        let clean = normalize_sub(p);
        if clean.is_empty() {
            continue;
        }
        if to_trash {
            let name = basename_of(&clean);
            let dst = format!("{}/{}/{}", normalize_sub(&cfg.trash_dir), today, name);
            ops.push(json!({
                "op": "rclone.call",
                "method": "operations/movefile",
                "params": {"srcFs": root.remote, "srcRemote": clean, "dstFs": root.remote, "dstRemote": dst},
            }));
        } else {
            ops.push(json!({
                "op": "rclone.call",
                "method": "operations/deletefile",
                "params": {"fs": root.remote, "remote": clean},
            }));
        }
        deleted.push(clean);
    }
    index.retain(|i| !(i.root_id == root.id && deleted.contains(&i.path)));
    ops.push(json!({"op": "kv.set", "key": "index", "value": {"items": index}}));
    Ok(ok_with_ops(id, ops, json!({"deleted": deleted})))
}

fn files_rename(id: u64, http: &HttpReq, cfg: &ImagehostConfig, index: &mut Vec<ImageItem>) -> Result<Value, String> {
    let body = b64decode(&http.body_b64)?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let root_id = s(v.get("rootId"));
    let root = match pick_root(cfg, &root_id) {
        Some(r) => r,
        None => return Ok(err(id, 400, "root 不存在")),
    };
    let from = normalize_sub(&s(v.get("from")));
    let to = s(v.get("to"));
    if from.is_empty() || to.is_empty() {
        return Ok(err(id, 400, "from/to 必填"));
    }
    if to.contains('/') || to.contains('\\') || to == "." || to == ".." {
        return Ok(err(id, 400, "to 只能是文件名"));
    }
    let dir = dirname_of(&from);
    let to_path = if dir.is_empty() { to.clone() } else { format!("{dir}/{to}") };
    let ops = vec![
        json!({
            "op": "rclone.call",
            "method": "operations/movefile",
            "params": {"srcFs": root.remote, "srcRemote": from, "dstFs": root.remote, "dstRemote": to_path},
        }),
        json!({"op": "kv.set", "key": "index", "value": {"items": index}}),
    ];
    if let Some(it) = index.iter_mut().find(|i| i.root_id == root.id && i.path == from) {
        it.path = to_path.clone();
        it.name = to;
    }
    Ok(ok_with_ops(id, ops, json!({"path": to_path})))
}

// ---------------- /scan：三轮协商 ----------------

fn scan_phase(id: u64, http: &HttpReq, cfg: ImagehostConfig, index: Vec<ImageItem>, state: State, entries: Option<Vec<Value>>) -> Result<Value, String> {
    let root = match &state.root {
        Some(r) => r.clone(),
        None => {
            // 第二轮：解析 root，请求递归列表
            let body = b64decode(&http.body_b64)?;
            let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let root_id = s(v.get("rootId"));
            match pick_root(&cfg, &root_id) {
                Some(r) => r.clone(),
                None => return Ok(err(id, 400, "root 不存在")),
            }
        }
    };

    if entries.is_none() {
        let mut st = action_state(&state.http, &state.now, &state.req_id, &state.action, &state.form);
        st["cfg"] = cfg.to_value();
        st["index"] = serde_json::to_value(&index).unwrap_or(Value::Null);
        st["root"] = serde_json::to_value(&root).unwrap_or(Value::Null);
        return Ok(json!({
            "id": id,
            "need": [{
                "cap": "rclone.call",
                "key": "entries",
                "params": {"method": "operations/list", "params": {"fs": root.remote, "remote": "", "options": {"recurse": true}}},
            }],
            "state": st,
        }));
    }

    // 第三轮：回填索引
    let mut index = index;
    let entries = entries.unwrap_or_default();
    let now = state.now.clone();
    let mut added = 0i64;
    for e in entries.iter().take(5000) {
        let is_dir = e.get("IsDir").and_then(|x| x.as_bool()).unwrap_or(false);
        let path = normalize_sub(&s(e.get("Path")));
        if is_dir || path.is_empty() {
            continue;
        }
        if find_by_path(&index, &root.id, &path).is_some() {
            continue;
        }
        let name = s(e.get("Name"));
        let (_, ext) = split_ext(&name);
        index.push(ImageItem {
            id: item_id(&root.id, &path),
            root_id: root.id.clone(),
            path: path.clone(),
            name,
            size: e.get("Size").and_then(|x| x.as_i64()).unwrap_or(0),
            sha1: String::new(),
            ext: ext.to_lowercase(),
            uploaded_at: {
                let m = s(e.get("ModTime"));
                if m.is_empty() { s(now.get("iso")) } else { m }
            },
            source: "scan".into(),
            tags: vec![],
        });
        added += 1;
    }
    let total = index.iter().filter(|i| i.root_id == root.id).count();
    let ops = vec![json!({"op": "kv.set", "key": "index", "value": {"items": index}})];
    Ok(ok_with_ops(id, ops, json!({"added": added, "total": total})))
}

// ---------------- /upload：三轮协商（移植 pipeline.ts） ----------------

fn upload_phase(
    id: u64,
    http: &HttpReq,
    cfg: ImagehostConfig,
    index: Vec<ImageItem>,
    state: State,
    entries: Option<Vec<Value>>,
) -> Result<Value, String> {
    // 文件来源：multipart（网页/应用上传）或 raw body（Obsidian 等）
    let (fields, files): (Map<String, Value>, Vec<MultipartFile>) = if let Some(mp) = &http.multipart {
        (mp.fields.clone(), mp.files.clone())
    } else {
        let filename = s(http.query.get("filename"));
        if filename.is_empty() {
            return Ok(err(id, 400, "raw 模式缺少 filename 参数"));
        }
        if http.body_b64.is_empty() {
            return Ok(err(id, 400, "请求体为空"));
        }
        let mut f = Map::new();
        for k in ["rootId", "subPath", "naming", "seqPad", "app", "vault", "notePath"] {
            if let Some(v) = http.query.get(k) {
                f.insert(k.to_string(), v.clone());
            }
        }
        (f, vec![MultipartFile { filename: basename_of(&filename), data_b64: http.body_b64.clone() }])
    };
    if files.is_empty() {
        return Ok(err(id, 400, "缺少文件"));
    }

    let getf = |k: &str| s(fields.get(k));
    let root_id = getf("rootId");
    let root = match pick_root(&cfg, &root_id) {
        Some(r) => r.clone(),
        None => return Ok(err(id, 400, "root 不存在")),
    };

    let now = state.now.clone();

    if state.root.is_none() {
        // 第二轮：算 sha1 / 目标目录，请求目标目录列表
        let dir = if getf("app") == "obsidian" {
            obsidian_sub_path(&cfg.obsidian.dir_layout, &getf("vault"), &getf("notePath"), &s(now.get("date_iso")))
        } else {
            normalize_sub(&getf("subPath"))
        };
        let mut sha1s = Vec::new();
        let mut sizes = Vec::new();
        for f in &files {
            match b64decode(&f.data_b64) {
                Ok(d) => {
                    sizes.push(d.len() as i64);
                    sha1s.push(sha1_hex(&d));
                }
                Err(_) => {
                    sha1s.push(String::new());
                    sizes.push(0);
                }
            }
        }
        let mut st = action_state(&state.http, &state.now, &state.req_id, &state.action, &state.form);
        st["cfg"] = cfg.to_value();
        st["index"] = serde_json::to_value(&index).unwrap_or(Value::Null);
        st["root"] = serde_json::to_value(&root).unwrap_or(Value::Null);
        st["dir"] = Value::String(dir.clone());
        st["files"] = serde_json::to_value(&files).unwrap_or(Value::Null);
        st["sha1s"] = serde_json::to_value(&sha1s).unwrap_or(Value::Null);
        st["sizes"] = serde_json::to_value(&sizes).unwrap_or(Value::Null);
        return Ok(json!({
            "id": id,
            "need": [{
                "cap": "rclone.call",
                "key": "entries",
                "params": {"method": "operations/list", "params": {"fs": root.remote, "remote": dir}},
            }],
            "state": st,
        }));
    }

    // 第三轮：命名 → 冲突 → ops（上传 + 索引）→ 结果
    let mut index = index;
    let dir_entries = entries.unwrap_or_default();
    let dir = state.dir.clone();
    let files = state.files;
    let sha1s = state.sha1s;
    let sizes = state.sizes;

    let raw_mode = {
        let m = getf("naming");
        if ["timestamp", "seq", "keep", "hash"].contains(&m.as_str()) { m } else { cfg.naming.mode.clone() }
    };
    let seq_pad = {
        let p = getf("seqPad").parse::<i64>().unwrap_or(0);
        if p > 0 { p } else { cfg.naming.seq_pad }
    };

    // 目标目录现状（文件名集合）
    let mut existing: std::collections::HashSet<String> = dir_entries
        .iter()
        .filter(|e| !e.get("IsDir").and_then(|x| x.as_bool()).unwrap_or(false))
        .filter_map(|e| e.get("Name").and_then(|x| x.as_str()).map(|x| x.to_string()))
        .collect();
    for n in names_in_dir(&index, &root.id, &dir) {
        existing.insert(n);
    }

    let mut results: Vec<Value> = Vec::new();
    let mut ops: Vec<Value> = Vec::new();
    let mut new_items: Vec<ImageItem> = Vec::new();

    for (i, f) in files.iter().enumerate() {
        let orig_name = {
            let n = basename_of(&f.filename);
            if n.is_empty() { "file".to_string() } else { n }
        };
        let sha1 = sha1s.get(i).cloned().unwrap_or_default();
        let data_b64 = f.data_b64.clone();
        let size = sizes.get(i).copied().unwrap_or(0);

        // 1) 去重秒传
        if cfg.dedupe {
            if let Some(exist) = find_by_sha(&index, &root.id, &sha1) {
                results.push(to_result(exist, &root, &cfg.base, &http.origin, true));
                continue;
            }
        }

        // 4) 命名
        let (orig_base, orig_ext) = split_ext(&orig_name);
        let ext = orig_ext.to_lowercase();
        let base = match raw_mode.as_str() {
            "keep" => orig_base,
            "hash" => sha1.chars().take(12).collect::<String>(),
            "seq" => {
                let names: Vec<String> = existing.iter().cloned().collect();
                next_seq_name(&names, seq_pad)
            }
            _ => timestamp_name(&now),
        };

        // 5) 冲突
        let mut final_name = if ext.is_empty() { base.clone() } else { format!("{base}.{ext}") };
        if existing.contains(&final_name) {
            let stored_candidate = if dir.is_empty() { final_name.clone() } else { format!("{dir}/{final_name}") };
            if let Some(conflict) = find_by_path(&index, &root.id, &stored_candidate) {
                if conflict.sha1 == sha1 {
                    results.push(to_result(conflict, &root, &cfg.base, &http.origin, true));
                    continue;
                }
            }
            final_name = resolve_conflict_name(&existing, &base, &ext);
        }
        existing.insert(final_name.clone());

        let stored_path = if dir.is_empty() { final_name.clone() } else { format!("{dir}/{final_name}") };

        // 6) 上传 op
        ops.push(json!({
            "op": "rclone.upload_file",
            "fs": root.remote,
            "remote_path": stored_path,
            "filename": final_name,
            "data_b64": data_b64,
        }));

        // 7) 索引
        let item = ImageItem {
            id: item_id(&root.id, &stored_path),
            root_id: root.id.clone(),
            path: stored_path,
            name: final_name,
            size,
            sha1,
            ext,
            uploaded_at: s(now.get("iso")),
            source: "upload".into(),
            tags: vec![],
        };
        results.push(to_result(&item, &root, &cfg.base, &http.origin, false));
        new_items.push(item);
    }

    for it in new_items {
        if let Some(pos) = index.iter().position(|x| x.root_id == it.root_id && x.path == it.path) {
            index[pos] = it;
        } else {
            index.push(it);
        }
    }
    ops.push(json!({"op": "kv.set", "key": "index", "value": {"items": index}}));
    // 持久化当前生效配置（含默认值），顺带记住最近直链供面板预填
    let mut saved = cfg.to_value();
    if let Some(link) = results
        .iter()
        .rev()
        .map(|r| s(r.get("url")))
        .find(|u| !u.is_empty())
    {
        saved["lastlink"] = json!(link);
    }
    ops.push(json!({"op": "kv.set", "key": "config", "value": saved}));
    Ok(ok_with_ops(id, ops, json!({"items": results})))
}

/// 模板渲染：{path} 按段 encode_uri_component 后以 / 连接；{filename} 编码文件名
fn render_public_url(template: &str, stored_path: &str, name: &str) -> String {
    let p = stored_path
        .split('/')
        .map(encode_uri_component)
        .collect::<Vec<_>>()
        .join("/");
    template
        .replace("{path}", &p)
        .replace("{filename}", &encode_uri_component(name))
}

fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'!' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn to_result(item: &ImageItem, root: &ImageRoot, base: &str, origin: &str, deduped: bool) -> Value {
    // 填了公网地址就用它打头，否则退回本次请求的 origin
    let url = build_link(base, origin, &root.id, &item.path);
    let public_url = if root.public_url_template.is_empty() {
        Value::Null
    } else {
        Value::String(render_public_url(&root.public_url_template, &item.path, &item.name))
    };
    json!({
        "name": item.name,
        "storedPath": item.path,
        "url": url,
        "publicUrl": public_url,
        "size": item.size,
        "sha1": item.sha1,
        "deduped": deduped,
        "markdown": format!("![{}]({})", item.name, url),
    })
}

// ---------------- 测试：自己演一遍宿主，把面板每个动作跑到底 ----------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 透明 PNG，拿来冒充“用户选的文件”和“远端读回来的图片”
    const PNG_1PX: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn env_now() -> Value {
        json!({
            "ymd": "20261024",
            "hms": "153012",
            "iso": "2026-10-24T15:30:12+08:00",
            "epoch": 1761291012i64,
            "epoch_ms": 1761291012000i64,
        })
    }

    fn handle_value(v: Value) -> Value {
        let bytes = serde_json::to_vec(&v).expect("请求序列化失败");
        handle(&bytes).expect("插件回了个错误字符串")
    }

    /// 假宿主：按 need 造 provided，最多跑 5 轮，返回最后一条消息
    fn run_host(base: Map<String, Value>, kv: Value, cap: &dyn Fn(&str, &Value) -> Value) -> Value {
        let mut provided: Map<String, Value> = Map::new();
        let mut state = Value::Null;
        for _ in 0..5 {
            let mut params = Value::Object(base.clone());
            params["now"] = env_now();
            params["live"] = json!({"origin": "http://127.0.0.1:8080"});
            if !provided.is_empty() {
                params["provided"] = Value::Object(provided.clone());
            }
            if !state.is_null() {
                params["state"] = state.clone();
            }
            let msg = handle_value(json!({"id": 2, "method": "request", "params": params}));
            if msg.get("result").is_some() {
                return msg;
            }
            let need: Vec<Value> = msg
                .get("need")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            assert!(!need.is_empty(), "既没有 result 也没有 need：{msg}");
            // 只回这一轮新要的 cap，模拟真实宿主
            provided = Map::new();
            for n in need {
                let c = s(n.get("cap"));
                let key = s(n.get("key"));
                let val = if c == "kv.get_all" { kv.clone() } else { cap(&c, &n) };
                provided.insert(key, val);
            }
            state = msg.get("state").cloned().unwrap_or(Value::Null);
        }
        panic!("5 轮之内没拿到 result");
    }

    fn fake_cap(cap: &str, need: &Value) -> Value {
        match cap {
            // rclone RC 的回复一般裹一层 result
            "rclone.call" => json!({"result": {"list": [
                {"Path": "20260101_120000_1.png", "Name": "20260101_120000_1.png",
                 "Size": 68, "IsDir": false, "ModTime": "2026-01-01T12:00:00Z"},
                {"Path": "20260101_120000_2.jpg", "Name": "20260101_120000_2.jpg",
                 "Size": 128, "IsDir": false, "ModTime": "2026-01-01T12:00:01Z"},
            ]}}),
            "rclone.cat" => json!({"data_b64": PNG_1PX, "size": 68, "content_type": "image/png"}),
            // 本体回的是 rclone.conf 里的原样条目（"名字:类型"）
            "rclone.remotes" => json!({
                "remotes": ["AWS-bucket:s3", "k-vault:crypt"],
                "source": "conf",
            }),
            "fs.read_b64" => json!({"data_b64": PNG_1PX, "size": 68}),
            "ui.copy" => json!({"ok": true}),
            _ => json!({"error": format!("测试里没准备这个 cap：{cap}，need={need}")}),
        }
    }

    fn panel(action: &str, form: Value, kv: Value) -> Value {
        let mut base = Map::new();
        base.insert("action".to_string(), json!(action));
        base.insert("form".to_string(), form);
        base.insert("req_id".to_string(), json!("a1b2c3d4e5f60718"));
        run_host(base, kv, &fake_cap)
    }

    fn http_call(method: &str, path: &str, kv: Value) -> Value {
        let mut base = Map::new();
        base.insert(
            "http".to_string(),
            json!({"method": method, "path": path, "origin": "http://127.0.0.1:8080"}),
        );
        base.insert("req_id".to_string(), json!("a1b2c3d4e5f60718"));
        run_host(base, kv, &fake_cap)
    }

    fn lamp_ok(v: &Value) -> bool {
        matches!(v.as_str(), Some("ok") | Some("warn") | Some("err"))
    }

    #[test]
    fn manifest_version_matches_config() {
        let m = handle_value(json!({"id": 1, "method": "manifest"}));
        assert_eq!(m["manifest"]["id"], json!("imagehost"));
        // 面板要写剪贴板，声明里的 capabilities 必须有 ui
        let decl: Value = serde_json::from_str(include_str!("../config.json")).expect("config.json 解析失败");
        // 版本号只维护一处：代码里的 manifest 与 config.json 必须一致
        assert_eq!(m["manifest"]["version"], decl["version"]);
        let caps = decl["capabilities"].to_string();
        assert!(caps.contains("\"ui\""), "capabilities 缺 ui：{caps}");
        // 关键控件得在声明里
        let ctl = decl["plugin_setting"]["controls"].to_string();
        for key in [
            "remote",
            "path",
            "refresh-remotes",
            "browse",
            "base",
            "lastlink",
            "copy-link",
        ] {
            assert!(ctl.contains(&format!("\"{key}\"")), "缺 {key} 控件");
        }
    }

    #[test]
    fn state_returns_status_and_lamp() {
        let r = panel("state", json!({}), json!({}));
        assert_eq!(r["result"]["status"], json!(200));
        let body = &r["result"]["body"];
        let status = body["status"].as_str().unwrap_or("");
        assert!(status.contains("存储根"), "状态文本：{status}");
        assert!(status.contains("张图片"), "状态文本：{status}");
        assert!(lamp_ok(&body["lamp"]), "lamp 不合法：{}", body["lamp"]);
        assert!(body["checks"].is_array());
        assert!(!body["values"]["roots"].as_str().unwrap_or("").is_empty());
        assert!(r.get("need").is_none());
    }

    /// 首屏回填不能把用户配置好的远程改掉：表单里是空串时，必须以配置里的
    /// 默认根为准，而不是远程候选列表的第一个。
    #[test]
    fn state_keeps_configured_remote_when_form_is_empty() {
        let kv = json!({"config": {"roots": [
            {"id": "default", "name": "默认图床", "remote": "k-vault:image_bed", "isDefault": true}
        ]}});
        let r = panel("state", json!({"remote": ""}), kv);
        let body = &r["result"]["body"];
        assert_eq!(body["values"]["remote"], json!("k-vault"));
        let choices = body["choices"]["remote"].to_string();
        assert!(choices.contains("k-vault"), "候选：{choices}");
        assert!(choices.contains("AWS-bucket"), "候选：{choices}");
    }

    #[test]
    fn save_normalizes_roots() {        let form = json!({
            "roots": "local=D:\\pics", "naming": "seq", "seqpad": "3",
            "dedupe": "true", "obsidian": "note", "trash": "", "base": "",
        });
        let r = panel("save", form, json!({}));
        assert_eq!(r["result"]["status"], json!(200));
        let body = &r["result"]["body"];
        assert_eq!(body["values"]["roots"], json!("local=D:\\pics"));
        assert_eq!(body["values"]["naming"], json!("seq"));
        assert_eq!(body["values"]["seqpad"], json!("3"));
        assert_eq!(body["values"]["dedupe"], json!("true"));
        assert_eq!(body["status"], json!("已保存"));
        // 配置要落盘
        let ops = r["ops"].as_array().cloned().unwrap_or_default();
        assert!(
            ops.iter()
                .any(|o| o["op"] == json!("kv.set") && o["key"] == json!("config")),
            "缺少 kv.set config：{ops:?}"
        );
    }

    #[test]
    fn save_empty_roots_is_400() {
        let form = json!({
            "roots": "", "naming": "timestamp", "seqpad": "3",
            "dedupe": "true", "obsidian": "note", "trash": "", "base": "",
        });
        let r = panel("save", form, json!({}));
        assert_eq!(r["result"]["status"], json!(400));
        let body = &r["result"]["body"];
        assert_eq!(body["lamp"], json!("err"));
        assert!(lamp_ok(&body["lamp"]));
        let status = body["status"].as_str().unwrap_or("");
        assert!(status.contains("root"), "状态文本：{status}");
    }

    #[test]
    fn upload_asks_for_file_then_returns_link() {
        // 第一轮就该把 fs.read_b64 递上去
        let msg = handle_value(json!({"id": 2, "method": "request", "params": {
            "action": "upload",
            "form": {"file": "D:\\pics\\shot.png"},
            "now": env_now(),
            "live": {"origin": "http://127.0.0.1:8080"},
        }}));
        let caps: Vec<String> = msg["need"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|n| s(n.get("cap")))
            .collect();
        assert!(caps.contains(&"kv.get_all".to_string()), "need：{caps:?}");
        assert!(caps.contains(&"fs.read_b64".to_string()), "need：{caps:?}");

        let form = json!({"file": "D:\\pics\\shot.png", "root": "", "subdir": "",
                          "naming": "timestamp", "seqpad": "3"});
        let r = panel("upload", form, json!({}));
        assert_eq!(r["result"]["status"], json!(200));
        let body = &r["result"]["body"];
        let link = body["values"]["lastlink"].as_str().unwrap_or("");
        assert!(link.starts_with("http://127.0.0.1:8080/i/"), "直链不对：{link}");
        assert!(link.ends_with(".png"), "直链不对：{link}");
        assert_eq!(body["lamp"], json!("ok"));
        let ops = r["ops"].as_array().cloned().unwrap_or_default();
        assert!(
            ops.iter().any(|o| o["op"] == json!("rclone.upload_file")),
            "缺少上传 op：{ops:?}"
        );
    }

    #[test]
    fn upload_without_file_is_400() {
        let r = panel("upload", json!({"file": ""}), json!({}));
        assert_eq!(r["result"]["status"], json!(400));
        assert_eq!(r["result"]["body"]["status"], json!("请先选择文件"));
        assert_eq!(r["result"]["body"]["lamp"], json!("err"));
        assert!(r.get("need").is_none(), "不该再向宿主要东西");
    }

    #[test]
    fn base_url_leads_the_link() {
        let kv = json!({"config": {
            "roots": [{"id": "cdn", "name": "CDN", "remote": "r2:图床", "isDefault": true}],
            "base": "https://img.example.com/",
        }});
        let form = json!({"file": "D:\\pics\\a.png", "root": "cdn", "naming": "keep"});
        let r = panel("upload", form, kv);
        assert_eq!(r["result"]["status"], json!(200));
        assert_eq!(
            r["result"]["body"]["values"]["lastlink"],
            json!("https://img.example.com/i/cdn/a.png")
        );
    }

    #[test]
    fn scan_returns_counts() {
        let r = panel("scan", json!({"root": ""}), json!({}));
        assert_eq!(r["result"]["status"], json!(200));
        let body = &r["result"]["body"];
        let status = body["status"].as_str().unwrap_or("");
        assert!(status.contains("已扫描"), "状态文本：{status}");
        assert!(status.contains('2'), "状态文本：{status}");
        let ops = r["ops"].as_array().cloned().unwrap_or_default();
        assert!(
            ops.iter()
                .any(|o| o["op"] == json!("kv.set") && o["key"] == json!("index")),
            "缺少 kv.set index：{ops:?}"
        );
    }

    #[test]
    fn add_token_returns_token_string() {
        let r = panel("add-token", json!({"tokenlabel": "我的手机"}), json!({}));
        assert_eq!(r["result"]["status"], json!(200));
        let body = &r["result"]["body"];
        let status = body["status"].as_str().unwrap_or("");
        let token = status.trim_start_matches("已生成令牌：");
        assert!(status.starts_with("已生成令牌："), "状态文本：{status}");
        assert!(!token.is_empty() && token != status, "没生成出令牌：{status}");
        assert_eq!(body["lamp"], json!("ok"));
        assert_eq!(body["checks"][0]["label"], json!("令牌"));
    }

    #[test]
    fn copy_link_asks_ui_copy() {
        let link = json!("https://img.example.com/i/default/a.png");
        let r = panel("copy-link", json!({"lastlink": link}), json!({}));
        assert_eq!(r["result"]["status"], json!(200));
        assert_eq!(r["result"]["body"]["status"], json!("已复制"));
        assert_eq!(r["result"]["body"]["values"]["lastlink"], link);
        // 没有直链时给个 warn，别报错
        let e = panel("copy-link", json!({"lastlink": ""}), json!({}));
        assert_eq!(e["result"]["status"], json!(400));
        assert_eq!(e["result"]["body"]["lamp"], json!("warn"));
    }

    #[test]
    fn image_route_rejects_escape_and_serves_bytes() {
        // ../ 不能爬出存储根
        let bad = http_call("GET", "/i/default/../secret.txt", json!({}));
        assert_eq!(bad["result"]["status"], json!(400));
        assert!(bad["result"]["body"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("越界"));

        // 正常路径：把 rclone.cat 的结果原样交出去
        let good = http_call("GET", "/i/default/2026/a.png", json!({}));
        assert_eq!(good["result"]["status"], json!(200));
        assert_eq!(good["result"]["content_type"], json!("image/png"));
        assert_eq!(good["result"]["body_b64"], json!(PNG_1PX));

        // 根不存在
        let nf = http_call("GET", "/i/nope/a.png", json!({}));
        assert_eq!(nf["result"]["status"], json!(404));
    }

    #[test]
    fn unknown_action_is_400() {
        let r = panel("bogus", json!({}), json!({}));
        assert_eq!(r["result"]["status"], json!(400));
        assert_eq!(r["result"]["body"]["status"], json!("未知操作"));
        assert_eq!(r["result"]["body"]["lamp"], json!("err"));
        assert!(r.get("need").is_none());
    }

    #[test]
    fn shutdown_and_exit_need_nothing() {
        for action in ["shutdown", "exit"] {
            let msg = handle_value(json!({"id": 2, "method": "request", "params": {"action": action}}));
            assert!(msg.get("need").is_none(), "{action} 还在要能力：{msg}");
            assert_eq!(msg["result"]["status"], json!(200));
            assert_eq!(msg["result"]["body"]["status"], json!("ok"));
        }
    }

    #[test]
    fn legacy_bed_config_is_migrated() {
        // 老版本把图床配置写在 `bed` 里；升级后第一次跑要自动搬过来
        let kv = json!({"bed": {"remote": "k-vault", "path": "image_bed", "dedupe": false, "lastlink": "https://x/y.png"}});
        let cfg = load_config(&kv);
        assert_eq!(cfg.roots.len(), 1);
        assert_eq!(cfg.roots[0].remote, "k-vault:image_bed");
        assert!(!cfg.dedupe);
        assert_eq!(cfg.lastlink, "https://x/y.png");
        // 有了新配置就不再迁移
        let kv2 = json!({"bed": {"remote": "old:bed"}, "config": {"roots": [{"id": "a", "remote": "r:a", "isDefault": true}]}});
        assert_eq!(load_config(&kv2).roots[0].remote, "r:a");
    }

    #[test]
    fn legacy_http_calls_still_work() {
        let cfgv = http_call("GET", "/config", json!({}));
        assert_eq!(cfgv["result"]["status"], json!(200));
        assert!(cfgv["result"]["body"]["roots"].is_array());
        let st = http_call("GET", "/stats", json!({}));
        assert_eq!(st["result"]["status"], json!(200));
        assert!(st["result"]["body"]["count"].is_number());
        let nf = http_call("GET", "/nope", json!({}));
        assert_eq!(nf["result"]["status"], json!(404));
    }

    #[test]
    fn normalize_rel_path_cases() {
        assert_eq!(normalize_rel_path("a/./b\\c.png").unwrap(), "a/b/c.png");
        assert_eq!(normalize_rel_path("/a//b.png").unwrap(), "a/b.png");
        assert_eq!(normalize_rel_path("D:\\pics\\x.png").unwrap(), "pics/x.png");
        assert_eq!(normalize_rel_path("a/../b.png").unwrap(), "b.png");
        assert!(normalize_rel_path("../x.png").is_err());
        assert!(normalize_rel_path("a/../../x.png").is_err());
    }
}
