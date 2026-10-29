//! egui font installation: binds the resolved system-font faces to egui's families.
//!
//! Only the math fallbacks are bundled; the user-configured UI/editor families
//! (including large CJK defaults) remain system-resolved via [`FontCatalog`].
//!
//! The egui-free catalog lives in `typ_edit_engine::fonts`.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};
use typ_edit_engine::fonts::{FontCatalog, ResolvedFont};

/// The custom egui font family used by the code editor.
pub const EDITOR_FAMILY: &str = "editor";

/// Platform UI family used for fresh settings when the OS default cannot be
/// queried (fontdb exposes installed families, not the live UI default).
///
/// This is a deliberate deviation from reading the live system default:
/// Windows uses Microsoft YaHei and macOS uses PingFang SC; both ship with
/// the respective OS. If the family is absent, the installer falls back to
/// the best installed CJK/sans family.
pub fn default_ui_font_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "PingFang SC"
    } else if cfg!(target_os = "windows") {
        "Microsoft YaHei"
    } else {
        "Noto Sans SC"
    }
}

/// Platform editor family used for fresh settings. Like the UI default, this is
/// a manual platform mapping because `fontdb` does not expose the OS default.
pub fn default_editor_font_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

/// Default UI family: the platform UI family if installed, else the best
/// CJK/system sans fallback.
pub fn default_ui_family(catalog: &FontCatalog) -> String {
    let preferred: &[&str] = if cfg!(target_os = "macos") {
        &[
            "PingFang SC",
            "PingFang TC",
            "Hiragino Sans GB",
            "Helvetica Neue",
            "Arial",
            "Noto Sans SC",
            "Noto Sans CJK SC",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            "Microsoft YaHei",
            "Microsoft YaHei UI",
            "Segoe UI",
            "Arial",
            "Noto Sans SC",
            "Noto Sans CJK SC",
        ]
    } else {
        &[
            "Noto Sans SC",
            "Noto Sans CJK SC",
            "Noto Sans",
            "DejaVu Sans",
            "Liberation Sans",
            "Arial",
        ]
    };
    for name in preferred {
        if let Some(found) = find_family(catalog, name) {
            return found;
        }
    }

    // Any family whose name advertises CJK coverage.
    const CJK_HINTS: &[&str] = &[
        "cjk",
        "noto sans sc",
        "noto sans tc",
        "noto serif sc",
        "source han",
        "yahei",
        "simsun",
        "simhei",
        "pingfang",
        "hiragino",
        "yu gothic",
        "meiryo",
        "malgun",
        "wenquanyi",
        "dengxian",
        "fangsong",
        "kaiti",
        "mingliu",
    ];
    for family in catalog.families() {
        let lower = family.to_lowercase();
        if CJK_HINTS.iter().any(|hint| lower.contains(hint)) {
            return family.clone();
        }
    }

    // Must return SOMETHING: the first installed family, else a generic name that
    // egui/typst will fall back from.
    catalog
        .families()
        .first()
        .cloned()
        .unwrap_or_else(|| "sans-serif".to_owned())
}

/// Default editor family: the platform monospace if installed, else the best
/// available monospace fallback.
pub fn default_editor_family(catalog: &FontCatalog) -> String {
    let preferred: &[&str] = if cfg!(target_os = "macos") {
        &[
            "Menlo",
            "Monaco",
            "Courier New",
            "JetBrains Maple Mono",
            "JetBrains Mono",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            "Consolas",
            "Cascadia Mono",
            "Courier New",
            "JetBrains Maple Mono",
            "JetBrains Mono",
        ]
    } else {
        &[
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "JetBrains Maple Mono",
            "JetBrains Mono",
        ]
    };
    for name in preferred {
        if let Some(found) = find_family(catalog, name) {
            return found;
        }
    }

    catalog
        .monospaced_family()
        .unwrap_or_else(|| "monospace".to_owned())
}

/// Case-insensitive exact lookup that returns the installed spelling.
fn find_family(catalog: &FontCatalog, name: &str) -> Option<String> {
    let key = name.to_lowercase();
    catalog
        .families()
        .iter()
        .find(|family| family.to_lowercase() == key)
        .cloned()
}

/// Bundled math fallback for proportional (UI/docs) text. Embedded so symbol
/// glyphs render even on systems without a math font installed.
const NOTO_SANS_MATH: &[u8] = include_bytes!("../assets/fonts/NotoSansMath-Regular.ttf");

/// Bundled math fallback for monospace text (editor + `.monospace()` tooltips).
const FIRA_MATH: &[u8] = include_bytes!("../assets/fonts/FiraMath-Regular.otf");

/// Family name reported by the bundled proportional math face.
pub const MATH_UI_FAMILY: &str = "Noto Sans Math";

/// Family name reported by the bundled monospace math face.
pub const MATH_MONO_FAMILY: &str = "Fira Math";

/// Clamps the persisted whole-interface zoom factor.
pub fn ui_zoom_factor(ui_scale: f32) -> f32 {
    ui_scale.clamp(0.5, 2.5)
}

/// Installs the chosen UI + editor fonts into egui.
///
/// Fallback priority: the configured family when installed, else the
/// platform/CJK fallback for that role; the bundled math fonts fill the
/// glyphs those families lack; egui's built-in defaults stay last.
///
/// Returns `(ui_ok, editor_ok)`: whether each **requested** family was
/// resolved. A `false` value can still have a usable fallback face installed.
pub fn install_egui_fonts(
    ctx: &egui::Context,
    ui_family: &str,
    editor_family: &str,
    catalog: &FontCatalog,
) -> (bool, bool) {
    let ui_requested = if ui_family.trim().is_empty() {
        None
    } else {
        catalog.resolve(ui_family)
    };
    let editor_requested = if editor_family.trim().is_empty() {
        None
    } else {
        catalog.resolve(editor_family)
    };
    let ui_ok = ui_requested.is_some();
    let editor_ok = editor_requested.is_some();
    // Keep the app usable when a persisted/default family is missing: install
    // the best platform/CJK fallback as the face, but report the requested
    // family as unresolved so the settings warning and log remain truthful.
    let ui = ui_requested.or_else(|| {
        let fallback = default_ui_family(catalog);
        catalog.resolve(&fallback)
    });
    let editor = editor_requested.or_else(|| {
        let fallback = default_editor_family(catalog);
        catalog.resolve(&fallback)
    });
    let defs = build_font_definitions(ui.as_ref(), editor.as_ref());
    ctx.set_fonts(defs);
    (ui_ok, editor_ok)
}

/// Builds egui font data + family chains free of `egui::Context`, so fallback
/// ordering is unit-testable headlessly.
fn build_font_definitions(
    ui: Option<&ResolvedFont>,
    editor: Option<&ResolvedFont>,
) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let ui_key = insert_face(&mut defs, "ui", ui);
    let editor_key = insert_face(&mut defs, "editor", editor);
    let math_ui_key = format!("math-ui:{MATH_UI_FAMILY}");
    let math_mono_key = format!("math-mono:{MATH_MONO_FAMILY}");
    defs.font_data.insert(
        math_ui_key.clone(),
        Arc::new(FontData::from_static(NOTO_SANS_MATH)),
    );
    defs.font_data.insert(
        math_mono_key.clone(),
        Arc::new(FontData::from_static(FIRA_MATH)),
    );

    // Proportional: configured UI font first, then the math glyph fallback,
    // then egui's built-in Latin/emoji defaults.
    let ui_defaults = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let configured_ui: Vec<String> = ui_key.iter().cloned().collect();
    defs.families.insert(
        FontFamily::Proportional,
        build_family_chain(&configured_ui, Some(math_ui_key.as_str()), &ui_defaults),
    );

    // Monospace: configured editor font, configured UI font (CJK coverage in
    // code and tooltips), math fallback, then egui's built-in mono defaults.
    let mono_defaults = defs
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let configured_mono: Vec<String> = editor_key.iter().chain(ui_key.iter()).cloned().collect();
    let mono_chain = build_family_chain(
        &configured_mono,
        Some(math_mono_key.as_str()),
        &mono_defaults,
    );
    defs.families
        .insert(FontFamily::Monospace, mono_chain.clone());
    // The editor family must always be bound (egui's layouter panics otherwise)
    // and mirrors the mono chain so editor text and `.monospace()` tooltips
    // share the same symbol-glyph fallback.
    defs.families
        .insert(FontFamily::Name(EDITOR_FAMILY.into()), mono_chain);

    defs
}

/// Registers one face under a prefix-keyed egui font key.
fn insert_face(
    defs: &mut FontDefinitions,
    prefix: &str,
    font: Option<&ResolvedFont>,
) -> Option<String> {
    let font = font?;
    let key = format!("{prefix}:{}", font.family);
    defs.font_data
        .insert(key.clone(), Arc::new(to_egui_font_data(font)));
    Some(key)
}

/// Assembles a family chain by priority: `configured` first (they win),
/// `math_fallback` next (only when resolved), `existing` egui defaults last.
/// Duplicate keys keep their earliest position.
fn build_family_chain(
    configured: &[String],
    math_fallback: Option<&str>,
    existing: &[String],
) -> Vec<String> {
    let mut chain: Vec<String> = Vec::with_capacity(configured.len() + existing.len() + 1);
    for key in configured.iter().map(String::as_str).chain(math_fallback) {
        if !chain.iter().any(|seen| seen.as_str() == key) {
            chain.push(key.to_owned());
        }
    }
    for key in existing {
        if !chain.iter().any(|seen| seen == key) {
            chain.push(key.clone());
        }
    }
    chain
}

/// Converts a resolved face into egui's font data (one copy of the bytes).
fn to_egui_font_data(font: &ResolvedFont) -> FontData {
    let mut data = FontData::from_owned(font.data.as_ref().clone());
    data.index = font.index;
    data
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui::FontFamily;
    use typ_edit_engine::fonts::ResolvedFont;

    use super::{
        EDITOR_FAMILY, FIRA_MATH, FontCatalog, MATH_MONO_FAMILY, MATH_UI_FAMILY, NOTO_SANS_MATH,
        build_font_definitions, default_editor_family, default_editor_font_name, default_ui_family,
        default_ui_font_name, install_egui_fonts, ui_zoom_factor,
    };

    /// A resolved-face stub; empty bytes are fine, nothing rasterizes in tests.
    fn fake(family: &str) -> ResolvedFont {
        ResolvedFont {
            family: family.to_owned(),
            data: Arc::new(Vec::new()),
            index: 0,
        }
    }

    #[test]
    fn defaults_resolve_to_installed_families() {
        let catalog = FontCatalog::new();
        assert!(!default_ui_family(&catalog).is_empty());
        assert!(!default_editor_family(&catalog).is_empty());
    }

    #[test]
    fn default_ui_font_name_is_platform_specific() {
        #[cfg(target_os = "macos")]
        assert_eq!(default_ui_font_name(), "PingFang SC");
        #[cfg(target_os = "windows")]
        assert_eq!(default_ui_font_name(), "Microsoft YaHei");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(default_ui_font_name(), "Noto Sans SC");
    }

    #[test]
    fn default_editor_font_name_is_platform_specific() {
        #[cfg(target_os = "macos")]
        assert_eq!(default_editor_font_name(), "Menlo");
        #[cfg(target_os = "windows")]
        assert_eq!(default_editor_font_name(), "Consolas");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(default_editor_font_name(), "DejaVu Sans Mono");
    }

    #[test]
    fn missing_configured_fonts_install_a_valid_fallback_chain() {
        let ctx = egui::Context::default();
        let catalog = FontCatalog::empty();
        let (ui_ok, editor_ok) = install_egui_fonts(
            &ctx,
            "Definitely Missing UI",
            "Definitely Missing Mono",
            &catalog,
        );
        assert!(!ui_ok, "the requested UI family is absent");
        assert!(!editor_ok, "the requested editor family is absent");

        // Setting the fallback chain must still be frame-safe even when the
        // catalog has no installed families (math + egui defaults remain).
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            ui.label("fallback");
        });
        output.textures_delta.clear();
    }

    #[test]
    fn proportional_chain_puts_math_after_configured_font() {
        let ui = fake("Noto Sans SC");

        let defs = build_font_definitions(Some(&ui), None);
        let chain = &defs.families[&FontFamily::Proportional];

        let configured = chain
            .iter()
            .position(|key| key == "ui:Noto Sans SC")
            .expect("configured UI font present");
        let math_pos = chain
            .iter()
            .position(|key| key == &format!("math-ui:{MATH_UI_FAMILY}"))
            .expect("Noto Sans Math present");
        assert_eq!(configured, 0, "configured UI font keeps priority");
        assert!(
            math_pos > configured,
            "math fallback must follow the configured font"
        );
    }

    #[test]
    fn mono_and_editor_chains_put_fira_math_after_configured_font() {
        let ui = fake("Noto Sans SC");
        let editor = fake("JetBrains Mono");

        let defs = build_font_definitions(Some(&ui), Some(&editor));
        let mono = &defs.families[&FontFamily::Monospace];
        let editor_chain = &defs.families[&FontFamily::Name(EDITOR_FAMILY.into())];

        assert_eq!(mono, editor_chain, "editor family mirrors the mono chain");
        let configured = mono
            .iter()
            .position(|key| key == "editor:JetBrains Mono")
            .expect("configured editor font present");
        let math_pos = mono
            .iter()
            .position(|key| key == &format!("math-mono:{MATH_MONO_FAMILY}"))
            .expect("Fira Math present");
        assert_eq!(configured, 0, "configured editor font keeps priority");
        assert!(
            math_pos > configured,
            "Fira Math must follow the configured font"
        );
    }

    #[test]
    fn bundled_math_fonts_render_without_configured_fonts() {
        let defs = build_font_definitions(None, None);

        assert!(
            defs.families[&FontFamily::Proportional]
                .iter()
                .any(|key| key == &format!("math-ui:{MATH_UI_FAMILY}"))
        );
        assert!(
            defs.families[&FontFamily::Monospace]
                .iter()
                .any(|key| key == &format!("math-mono:{MATH_MONO_FAMILY}"))
        );
        let editor_chain = &defs.families[&FontFamily::Name(EDITOR_FAMILY.into())];
        assert!(
            !editor_chain.is_empty(),
            "editor family must always be bound"
        );
    }

    #[test]
    fn bundled_math_chain_covers_the_pop_out_glyph() {
        // The pane pop-out button draws `⧉` (U+29C9); the bundled Noto Sans Math
        // fallback must supply it or the button renders a tofu box. egui only
        // realizes the font set during a frame, so drive one empty pass first.
        //
        // `has_glyphs` is unusable here: egui uses `◻` as the replacement char and
        // the math font *contains* `◻`, so it becomes the replacement face and
        // every glyph that face renders reports as absent (even `A`). `glyph_width`
        // instead returns the advance, which is `0.0` when no face owns the glyph.
        let ctx = egui::Context::default();
        ctx.set_fonts(build_font_definitions(None, None));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |_ui| {});
        // No real painter applies the font/texture deltas; epaint asserts they
        // were handled when the `FullOutput` drops.
        output.textures_delta.clear();

        let font_id = egui::FontId::proportional(18.0);
        // Negative control: an unassigned code point has no glyph, so the test
        // genuinely fails if the math fallback drops out of the chain.
        let missing = ctx.fonts_mut(|f| f.glyph_width(&font_id, '\u{10FFFD}'));
        assert_eq!(missing, 0.0, "an unassigned glyph must measure zero");
        let pop_out = ctx.fonts_mut(|f| f.glyph_width(&font_id, '⧉'));
        assert!(
            pop_out > 0.0,
            "the proportional fallback chain must cover U+29C9 (got width {pop_out})"
        );
    }

    #[test]
    fn ui_scale_maps_to_whole_interface_zoom() {
        assert_eq!(ui_zoom_factor(1.0), 1.0);
        assert_eq!(ui_zoom_factor(2.0), 2.0);
        assert_eq!(ui_zoom_factor(0.0), 0.5, "tiny scales clamp at 50%");
        assert_eq!(ui_zoom_factor(100.0), 2.5, "huge scales clamp at 250%");
    }

    #[test]
    fn embedded_math_fonts_parse_and_report_family_names() {
        let mut db = fontdb::Database::new();
        db.load_font_data(NOTO_SANS_MATH.to_vec());
        db.load_font_data(FIRA_MATH.to_vec());

        let families: Vec<String> = db
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();

        assert!(
            families.iter().any(|name| name == "Noto Sans Math"),
            "embedded Noto Sans Math family not found: {families:?}"
        );
        assert!(
            families.iter().any(|name| name == "Fira Math"),
            "embedded Fira Math family not found: {families:?}"
        );
    }
}
