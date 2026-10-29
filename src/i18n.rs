//! Resolves the persisted language preference to a runtime locale.
//!
//! `LanguagePref::System` is detected once per settings change via `sys-locale`
//! (falling back to the `LANG` / `LC_ALL` environment variables, then Chinese).

use crate::settings::{LanguagePref, Settings};
use typ_edit_i18n::{Lang, set_language};

/// Applies the settings' language preference to the global i18n runtime.
pub fn apply_language(settings: &Settings) {
    set_language(resolve(settings.language));
}

/// Maps one preference to a shipped language, detecting the system locale for
/// [`LanguagePref::System`].
fn resolve(pref: LanguagePref) -> Lang {
    match pref {
        LanguagePref::ZhCn => Lang::ZhCn,
        LanguagePref::EnUs => Lang::EnUs,
        LanguagePref::System => detect_system(),
    }
}

/// Detects the operating system locale, then the POSIX environment, then Chinese.
fn detect_system() -> Lang {
    if let Some(code) = sys_locale::get_locale() {
        return Lang::from_code(&code);
    }
    for var in ["LANG", "LC_ALL"] {
        if let Ok(code) = std::env::var(var)
            && !code.is_empty()
        {
            return Lang::from_code(&code);
        }
    }
    Lang::ZhCn
}
