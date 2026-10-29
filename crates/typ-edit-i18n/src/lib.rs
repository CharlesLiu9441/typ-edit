//! Minimal hand-rolled i18n runtime for the Typst editor.
//!
//! Two languages are shipped as embedded JSON packs (`locales/zh-CN.json` and
//! `locales/en-US.json`), parsed once into leaked `&'static str` maps so a
//! translation lookup can hand back a borrowed [`Cow`] with zero per-frame
//! allocation. The active language is a global [`RwLock`] read on every lookup,
//! so switching a language takes effect on the next frame without restarting.
//!
//! There are exactly two languages and no plural rules or hot reload, so a
//! plain `t(key)` function beats a macro-based framework and keeps the call
//! sites trivially greppable.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// A shipped UI language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    /// Simplified Chinese (the source language; also the fallback default).
    #[default]
    ZhCn,
    /// English (US).
    EnUs,
}

impl Lang {
    /// The BCP-47 code of this language, used as the locale pack name.
    pub fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }

    /// Maps a locale code onto a shipped language.
    ///
    /// Accepts the full tags (`"zh-CN"`, `"en-US"`) and the common short forms
    /// (`"zh"`, `"zh-Hans"`, `"en"`). Any unrecognised locale falls back to
    /// [`Lang::ZhCn`].
    pub fn from_code(code: &str) -> Lang {
        let lower = code.to_ascii_lowercase();
        let primary = lower.split(['-', '_']).next().unwrap_or_default();
        if primary == "en" {
            Self::EnUs
        } else {
            Self::ZhCn
        }
    }
}

/// The process-wide active language.
static LOCALE: RwLock<Lang> = RwLock::new(Lang::ZhCn);

/// The parsed `zh-CN` pack, loaded once on first use.
static ZH_PACK: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
/// The parsed `en-US` pack, loaded once on first use.
static EN_PACK: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

/// Switches the active language. Poisoned locks are recovered from, so a panic
/// in an unrelated thread never bricks translation.
pub fn set_language(lang: Lang) {
    match LOCALE.write() {
        Ok(mut guard) => *guard = lang,
        Err(poisoned) => *poisoned.into_inner() = lang,
    }
}

/// The currently active language.
pub fn language() -> Lang {
    match LOCALE.read() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// Translates `key` in the active language.
///
/// Lookup order: the active language, then `en-US`, then the key itself. A
/// hit in a static pack is borrowed (no allocation); only a miss allocates.
pub fn t(key: &str) -> Cow<'static, str> {
    t_in(language(), key)
}

/// Translates `key` for an explicit language.
///
/// Used where a layout must reserve space for both shipped languages at once
/// (for example the settings rail), independent of the active UI language.
pub fn t_in(lang: Lang, key: &str) -> Cow<'static, str> {
    let active = match lang {
        Lang::ZhCn => zh_pack(),
        Lang::EnUs => en_pack(),
    };
    if let Some(value) = active.get(key).copied() {
        return Cow::Borrowed(value);
    }
    if let Some(value) = en_pack().get(key).copied() {
        return Cow::Borrowed(value);
    }
    Cow::Owned(key.to_owned())
}

/// Translates `key` and returns a `'static` reference.
///
/// For APIs that must store a `&'static str` (a value baked in once at
/// startup). A key absent from both packs is leaked once; all callers pass a
/// shipped key, so that path is unreachable in practice.
pub fn t_static(key: &str) -> &'static str {
    let active = match language() {
        Lang::ZhCn => zh_pack(),
        Lang::EnUs => en_pack(),
    };
    if let Some(value) = active.get(key).copied() {
        return value;
    }
    if let Some(value) = en_pack().get(key).copied() {
        return value;
    }
    Box::leak(key.to_owned().into_boxed_str())
}

/// Translates `key` and substitutes every `{name}` placeholder from `args`.
///
/// A placeholder with no matching argument is left as-is; braces are not
/// escaped (no translation needs a literal brace).
pub fn t_with(key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t(key).into_owned();
    for (name, value) in args {
        let needle = format!("{{{name}}}");
        if out.contains(&needle) {
            out = out.replace(&needle, value);
        }
    }
    out
}

/// The `zh-CN` pack.
fn zh_pack() -> &'static HashMap<&'static str, &'static str> {
    ZH_PACK.get_or_init(|| parse_pack(include_str!("../locales/zh-CN.json")))
}

/// The `en-US` pack.
fn en_pack() -> &'static HashMap<&'static str, &'static str> {
    EN_PACK.get_or_init(|| parse_pack(include_str!("../locales/en-US.json")))
}

/// Parses one embedded locale pack and leaks every string exactly once.
///
/// The packs are a few hundred entries and parsed a single time per process, so
/// leaking them is the cheapest way to obtain the `'static` keys the lookup
/// maps are built from.
fn parse_pack(raw: &str) -> HashMap<&'static str, &'static str> {
    let value: serde_json::Value = serde_json::from_str(raw).expect("locale pack is valid JSON");
    let object = value.as_object().expect("locale pack is a JSON object");
    let mut map = HashMap::with_capacity(object.len());
    for (key, value) in object {
        let key: &'static str = Box::leak(key.clone().into_boxed_str());
        let value: &'static str = Box::leak(
            value
                .as_str()
                .expect("locale values are strings")
                .to_owned()
                .into_boxed_str(),
        );
        map.insert(key, value);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both packs define exactly the same key set.
    #[test]
    fn packs_define_the_same_keys() {
        let zh = zh_pack();
        let en = en_pack();
        let mut missing_in_en: Vec<&&str> = zh.keys().filter(|k| !en.contains_key(**k)).collect();
        let mut missing_in_zh: Vec<&&str> = en.keys().filter(|k| !zh.contains_key(**k)).collect();
        missing_in_en.sort();
        missing_in_zh.sort();
        assert!(
            missing_in_en.is_empty(),
            "missing in en-US: {missing_in_en:?}"
        );
        assert!(
            missing_in_zh.is_empty(),
            "missing in zh-CN: {missing_in_zh:?}"
        );
    }

    /// An unknown key resolves to itself.
    #[test]
    fn unknown_key_falls_back_to_the_key() {
        assert_eq!(t("this.key.does.not.exist"), "this.key.does.not.exist");
    }

    /// `from_code` accepts the shipped tags and their short forms.
    #[test]
    fn from_code_maps_known_tags() {
        assert_eq!(Lang::from_code("zh-CN"), Lang::ZhCn);
        assert_eq!(Lang::from_code("zh"), Lang::ZhCn);
        assert_eq!(Lang::from_code("zh-Hans"), Lang::ZhCn);
        assert_eq!(Lang::from_code("en-US"), Lang::EnUs);
        assert_eq!(Lang::from_code("en"), Lang::EnUs);
        assert_eq!(Lang::from_code("EN_us"), Lang::EnUs);
        assert_eq!(Lang::from_code("fr-FR"), Lang::ZhCn);
    }

    /// `t_with` substitutes every provided placeholder.
    #[test]
    fn t_with_substitutes_placeholders() {
        let rendered = t_with("toast.saved", &[("name", "chapter.typ")]);
        assert!(rendered.contains("chapter.typ"), "got {rendered:?}");
        assert!(!rendered.contains("{name}"), "got {rendered:?}");
    }

    /// Switching the language changes `t` immediately; restored afterwards so
    /// other tests observe the process default.
    #[test]
    fn set_language_switches_lookup_at_runtime() {
        let original = language();
        set_language(Lang::EnUs);
        assert_eq!(t("menu.file.save"), "Save");
        set_language(Lang::ZhCn);
        assert_eq!(t("menu.file.save"), "保存");
        set_language(original);
    }

    /// Every key's value is non-empty in both packs.
    #[test]
    fn pack_values_are_non_empty() {
        for (lang, pack) in [("zh-CN", zh_pack()), ("en-US", en_pack())] {
            for (key, value) in pack {
                assert!(!value.trim().is_empty(), "{lang}: empty value for {key}");
            }
        }
    }

    mod i18n_scanner;
    mod i18n_workspace_scan;
}
