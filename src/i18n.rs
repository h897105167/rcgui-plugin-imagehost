//! 插件自带的界面多语言。
//!
//! # 为什么翻译表在插件里
//!
//! 界面文案全部由插件产生（`config.json` 的声明式控件 + 运行期回包里的
//! `status` / `toast` / `hints` / `checks` / manifest），宿主只负责显示。
//! 译文要是放在宿主侧，就等于「宿主必须认识每个插件的每一句话」——插件改一句
//! 话宿主就得跟着发版，而且宿主删掉一条文案就会把别的插件一起弄错。
//! 所以译文表跟着插件走：`src/i18n.json` 由本仓库维护，编译进 wasm。
//!
//! # 语言从哪来
//!
//! 宿主每次调用都在 `params.lang` 里带当前界面语言（`zh` / `en`）。
//! 插件只收**语言标记**，一个字的译文都不从宿主拿。
//!
//! 规则：
//! - `zh*`（以及宿主没带 `lang` 的老版本）→ 中文原文，消息 id 本身就是中文
//! - 其它一切（`en`、`en-US`、`fr`、将来新增的语言）→ 英文
//!
//! 也就是**没有对应语言时回落到英文**。
//!
//! # 消息 id
//!
//! 消息 id 就是中文原文，占位符与宿主 `rcgui_i18n::f` 同形状：
//!
//! ```ignore
//! i18n::t("图床");                                  // en: "Image host"
//! i18n::f("上传失败：{}", &[&e]);                    // 顺序占位
//! i18n::f("上传失败：{e}", &[&e]);                   // 命名占位，参数按出现顺序传
//! ```
//!
//! 表里查不到的消息 id 原样返回中文：宁可漏翻一句，也不能把界面弄成空白。
//! `tests::every_entry_is_a_real_translation` 等测试会在提交前把漏翻 / 占位符
//! 不一致的条目揪出来。

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// 编译期镶进来的译文表：`{"中文消息 id": "translation"}`。
const TABLE_JSON: &str = include_str!("i18n.json");

/// 未收到语言标记。
const LANG_UNSET: u8 = 0;
/// 中文（原文）。
const LANG_ZH: u8 = 1;
/// 英文。
const LANG_EN: u8 = 2;

static LANG: AtomicU8 = AtomicU8::new(LANG_UNSET);

/// 记下宿主这一轮的语言标记。
///
/// 一次调用就是一个新进程，所以入口处调一次即可。
pub fn set_lang(tag: Option<&str>) {
    let code = match tag {
        None => LANG_ZH, // 老宿主不带 lang：那时候界面本来就是中文
        Some(tag) if tag.trim().is_empty() => LANG_ZH,
        Some(tag) if tag.to_ascii_lowercase().starts_with("zh") => LANG_ZH,
        // 没有对应语言就回落英文
        Some(_) => LANG_EN,
    };
    LANG.store(code, Ordering::Relaxed);
}

/// 当前是否英文界面。
fn is_english() -> bool {
    LANG.load(Ordering::Relaxed) == LANG_EN
}

fn table() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| serde_json::from_str(TABLE_JSON).unwrap_or_default())
}

/// 把一条中文消息 id 翻成当前语言；查不到就原样返回。
pub fn t(msgid: &str) -> String {
    if !is_english() {
        return msgid.to_string();
    }
    match table().get(msgid) {
        Some(en) => en.clone(),
        None => msgid.to_string(),
    }
}

/// 带占位符的翻译：`f("共 {} 个远程", &[&n])`。
///
/// 模板里的占位符按出现顺序被 `args` 依次填掉，两种写法等价：
/// `{}`（顺序）与 `{name}`（命名，名字只用于阅读，参数照样按顺序传）。
/// `{{` / `}}` 是字面花括号。
///
/// 格式说明符（`{:?}` / `{:.1}` / `{n:>3}`）**不受支持**：整段原样留下，
/// 需要精度的参数请先自己 `format!` 成字符串再传进来。
pub fn f(msgid: &str, args: &[&dyn fmt::Display]) -> String {
    let tmpl = t(msgid);
    let mut out = String::with_capacity(tmpl.len() + 24);
    let mut next = 0usize;
    let mut chars = tmpl.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                // 收集占位符正文：遇到 `}` 结束，遇到 `:` 说明带格式说明符
                let mut body = String::new();
                let mut spec = false;
                let mut closed = false;
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ch == '}' {
                        closed = true;
                        break;
                    }
                    if ch == ':' {
                        spec = true;
                    }
                    body.push(ch);
                }
                if spec || !closed {
                    // 不认识的写法：原样输出，别静默吃掉内容
                    out.push('{');
                    out.push_str(&body);
                    if closed {
                        out.push('}');
                    }
                    continue;
                }
                match args.get(next) {
                    Some(a) => {
                        out.push_str(&a.to_string());
                        next += 1;
                    }
                    // 参数不够：原样留下占位符，别静默吞掉整句
                    None => {
                        out.push('{');
                        out.push_str(&body);
                        out.push('}');
                    }
                }
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
                out.push('}');
            }
            _ => out.push(c),
        }
    }
    out
}

/// 模板里的占位符序列（`{}` 与 `{name}` 都归一成 `{}`），测试用来比对中英一致。
#[cfg(test)]
fn placeholders(tmpl: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = tmpl.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            continue;
        }
        let mut body = String::new();
        let mut closed = false;
        while let Some(&ch) = chars.peek() {
            chars.next();
            if ch == '}' {
                closed = true;
                break;
            }
            body.push(ch);
        }
        if closed {
            match body.split_once(':') {
                Some((name, _)) => out.push(format!("{name} (带格式说明符)")),
                None => out.push("{}".to_string()),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试会并发跑，语言标记是全局的，所以每个用语言的测试先自己设一遍。
    fn with_lang<T>(tag: &str, f: impl FnOnce() -> T) -> T {
        set_lang(Some(tag));
        f()
    }

    #[test]
    fn table_loads_and_is_non_empty() {
        assert!(table().len() > 30, "译文表太小，八成是 i18n.json 没打进来");
    }

    #[test]
    fn every_entry_is_a_real_translation() {
        for (k, v) in table() {
            assert!(!k.trim().is_empty(), "空消息 id");
            assert!(!v.trim().is_empty(), "空译文：{k}");
            assert_ne!(k, v, "译文与原文相同：{k}");
        }
    }

    #[test]
    fn placeholders_match_between_zh_and_en() {
        for (k, v) in table() {
            assert_eq!(placeholders(k), placeholders(v), "占位符对不上：{k} -> {v}");
        }
    }

    #[test]
    fn zh_keeps_the_msgid_and_en_translates() {
        with_lang("zh-Hans", || {
            assert_eq!(t("图床"), "图床");
            assert_eq!(f("共 {} 个远程", &[&3]), "共 3 个远程");
        });
        with_lang("en", || {
            let en = t("图床");
            assert_ne!(en, "图床");
            assert!(!en.is_empty());
        });
        // 未知语言回落英文
        with_lang("fr", || assert_ne!(t("图床"), "图床"));
        // 老宿主不带标记 → 中文
        set_lang(None);
        assert_eq!(t("图床"), "图床");
    }

    #[test]
    fn named_and_positional_placeholders_both_fill() {
        with_lang("zh-Hans", || {
            assert_eq!(f("上传失败：{e}", &[&"boom"]), "上传失败：boom");
            assert_eq!(f("共 {} 个，第一个 {}", &[&2, &"a"]), "共 2 个，第一个 a");
            assert_eq!(f("花括号 {{}} 保留", &[] as &[&dyn fmt::Display]), "花括号 {} 保留");
            // 格式说明符不支持：原样留下，参数不前进
            assert_eq!(f("值 {:.1}", &[&1.5]), "值 {:.1}");
        });
    }
}
