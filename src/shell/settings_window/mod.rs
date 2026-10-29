//! Settings window: a centered modal overlay whose content mirrors the main window's card
//! language — a single floating surface-2 panel, which is itself the outermost surface (no window
//! chrome or board ring). Close via the 关闭 button, Escape, or clicking the dimming backdrop
//! veil. The card holds a vertical tab strip (the shared [`TabBarOrientation::Vertical`] tab
//! bar) on the left and a scrollable settings pane plus a footer (hairline + reset button)
//! on the right.
//!
//! Each category pane lives in its own submodule; [`grid`] holds the shared grid, separators and
//! font-warning helpers, and [`subtabs`] draws the nested vertical sub-page rail.

use std::borrow::Cow;

use egui::{Color32, NumExt};
use typ_edit_engine::fonts::FontCatalog;
use typ_edit_i18n::t;
use typ_edit_widgets::button::icon_button_painted_danger;
use typ_edit_widgets::icons::WindowGlyph;
use typ_edit_widgets::tab_bar::{TabAction, TabBarOrientation, TabItem};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, TuiBuilderLogic,
    taffy::{Dimension, LengthPercentage, Size, Style},
};

mod cache;
mod collab;
mod debug;
mod editor;
mod experimental;
mod export;
mod git;
mod grid;
mod icons;
mod images;
mod interface;
pub(crate) mod package_view;
pub(crate) mod packages;
mod preview;
mod shortcuts;
mod subtabs;

use cache::cache_ui;
use collab::collab_ui;
use debug::debug_ui;
use editor::editor_ui;
use experimental::experimental_ui;
use export::export_ui;
use git::git_ui;
use grid::hairline;
use interface::interface_ui;
use preview::preview_ui;
use shortcuts::shortcuts_ui;

/// Gap between the tab column and the content pane.
const CONTENT_GAP: f32 = Theme::LIGHT.spacing.settings_gap;
/// Width of the fixed-size settings overlay. 687 px lets the English Interface
/// and Shortcuts pages fit without locally widening the Modal.
const OVERLAY_W: f32 = Theme::LIGHT.spacing.settings_w;
/// Height of the fixed-size settings overlay.
const OVERLAY_H: f32 = Theme::LIGHT.spacing.settings_h;
/// Stable category rail width for the body font. The English category labels
/// are wider than the Chinese ones; using one width avoids moving the page
/// column when the language changes.
const CATEGORY_RAIL_W: f32 = 116.0;
/// Maximum settings page width. The content column is capped so a wider
/// English label or a previously widened Area can never feed back into the
/// page minimum. 531 px is the measured minimum for the English Interface
/// page at the body font; it leaves the Shortcuts page room for its actions.
const PAGE_W: f32 = 531.0;
/// Horizontal page frame padding (left + right).
const PAGE_PAD_X: f32 = 16.0;

/// Selected settings category, kept in egui memory without adding another field to the
/// frozen `show` signature.
fn category_id() -> egui::Id {
    egui::Id::new("typ_edit_settings_category")
}

/// Stable id of the settings overlay area. A single area keeps the modal
/// visible while switching categories; width is kept stable by the card layout,
/// not by giving each category its own Area.
fn overlay_id() -> egui::Id {
    egui::Id::new("typ_edit_settings_overlay")
}

/// Stable id of the shortcut-capture state in egui temp memory.
fn capture_id() -> egui::Id {
    egui::Id::new("typ_edit_shortcut_capture")
}

/// Index into `COMMANDS` of the command currently recording a new binding.
pub(super) fn capturing_index(ctx: &egui::Context) -> Option<usize> {
    ctx.data(|data| data.get_temp::<Option<usize>>(capture_id()).flatten())
}

/// Enters (`Some`) or leaves (`None`) shortcut-capture mode.
pub(super) fn set_capturing(ctx: &egui::Context, index: Option<usize>) {
    ctx.data_mut(|data| data.insert_temp(capture_id(), index));
}

/// Leaves shortcut-capture mode.
pub(super) fn clear_capture(ctx: &egui::Context) {
    set_capturing(ctx, None);
}

/// Whether the shortcuts settings page is recording a key combo. While true the
/// global shortcut handler must consume nothing so the capture sees the key.
pub(crate) fn is_shortcut_capturing(ui: &egui::Ui) -> bool {
    capturing_index(ui.ctx()).is_some()
}

/// Backdrop veil behind the overlay: black at `opacity` (clamped to 0..=1, rounded to u8).
fn backdrop_color(opacity: f32) -> Color32 {
    Color32::from_black_alpha((opacity.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The overlay's fixed size, clamped so it never spills off the app content rect.
fn overlay_size(ctx: &egui::Context) -> egui::Vec2 {
    let max = (ctx.content_rect().size() - egui::Vec2::splat(24.0)).at_least(egui::Vec2::ZERO);
    egui::vec2(OVERLAY_W, OVERLAY_H).at_most(max)
}

/// Settings categories shown in the vertical tab strip, in display order.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Category {
    #[default]
    Interface,
    Shortcuts,
    Editor,
    Export,
    Packages,
    Preview,
    Git,
    Collab,
    Experimental,
    Debug,
    Cache,
}

impl Category {
    const ALL: [Self; 11] = [
        Self::Interface,
        Self::Shortcuts,
        Self::Editor,
        Self::Export,
        Self::Packages,
        Self::Preview,
        Self::Git,
        Self::Collab,
        Self::Experimental,
        Self::Debug,
        Self::Cache,
    ];

    fn label(self) -> Cow<'static, str> {
        match self {
            Self::Interface => t("settings.category.interface"),
            Self::Shortcuts => t("settings.category.shortcuts"),
            Self::Editor => t("settings.category.editor"),
            Self::Export => t("settings.category.export"),
            Self::Packages => t("settings.category.packages"),
            Self::Preview => t("settings.category.preview"),
            Self::Git => t("settings.category.git"),
            Self::Collab => t("settings.category.collab"),
            Self::Experimental => t("settings.category.experimental"),
            Self::Debug => t("settings.category.debug"),
            Self::Cache => t("settings.category.cache"),
        }
    }
}

/// Draws the settings card shell: one tui row `tab rail | content column`.
///
/// The card itself is a single top-level tui tree (the Modal only supplies the
/// centered area and backdrop). Settings pages are rendered inside the
/// `ScrollArea` on a plain egui `Ui`, so each page opens its own top-level table
/// tree there; nothing below this point nests `tui()` inside a tui node.
fn shell_node_id(name: &'static str) -> egui::Id {
    egui::Id::new((
        "typ_edit_settings_shell",
        name,
        typ_edit_i18n::language().code(),
    ))
}

/// Card content root: one row filling the card.
fn card_row_style() -> Style {
    let mut style = tui::row();
    tui::fill(&mut style);
    style.gap = Size {
        width: LengthPercentage::length(CONTENT_GAP),
        height: LengthPercentage::length(0.0),
    };
    style
}

/// Fixed-width tab rail spanning the card height.
fn tab_col_style(width: f32) -> Style {
    let mut style = tui::fixed(width);
    style.size = Size {
        width: Dimension::length(width),
        height: Dimension::auto(),
    };
    style
}

/// Width of a vertical tab rail derived from its longest label plus padding.
fn vertical_tab_width<'a>(ui: &egui::Ui, labels: impl Iterator<Item = &'a str>) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut widest = 0.0_f32;
    for label in labels {
        let galley =
            ui.painter()
                .layout_no_wrap(label.to_owned(), font.clone(), egui::Color32::PLACEHOLDER);
        widest = widest.max(galley.size().x);
    }
    (widest + 28.0).clamp(96.0, 260.0)
}

/// Modal header: settings gear + title on the left, close control on the right.
fn settings_header(ui: &mut egui::Ui, open: &mut bool, theme: &Theme) {
    let close_h = theme.spacing.icon_btn;
    ui.horizontal(|ui| {
        let (icon_rect, _) = ui.allocate_exact_size(egui::Vec2::splat(16.0), egui::Sense::hover());
        typ_edit_widgets::icons::settings(ui, icon_rect, theme.colors.text_muted);
        ui.add_space(4.0);
        ui.label(egui::RichText::new(t("menu.settings")).strong());
        let remaining = ui.available_width();
        ui.allocate_ui_with_layout(
            egui::vec2(remaining, close_h),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let close = icon_button_painted_danger(
                    ui,
                    theme.spacing.icon_btn,
                    true,
                    |ui, rect, color| {
                        typ_edit_widgets::icons::window_glyph(ui, rect, WindowGlyph::Close, color)
                    },
                )
                .on_hover_text(t("settings.close"));
                if close.clicked() {
                    *open = false;
                }
            },
        );
    });
}

/// Content column: the page scroll fills the card.
fn content_col_style(width: f32) -> Style {
    let mut style = tui::column();
    style.flex_basis = Dimension::length(width);
    style.flex_grow = 0.0;
    style.flex_shrink = 0.0;
    style.min_size = Size {
        width: Dimension::length(width),
        height: Dimension::length(0.0),
    };
    style.size = Size {
        width: Dimension::length(width),
        height: Dimension::auto(),
    };
    style.gap = Size {
        width: LengthPercentage::length(0.0),
        height: LengthPercentage::length(0.0),
    };
    style
}

/// Shows the settings window. Returns `true` when any setting changed (the caller then
/// re-installs egui fonts and saves settings). Switching a category is not a change.
///
/// `package_pane` is the borrowed package-manager state; `package_requests`
/// collects the actions the 包管理 pane wants the app layer to run after the frame.
pub fn show(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut crate::settings::Settings,
    catalog: &FontCatalog,
    package_pane: &mut packages::PackagePane<'_>,
    package_requests: &mut Vec<packages::PackageRequest>,
) -> bool {
    if !*open {
        clear_capture(ctx);
        return false;
    }
    let mut changed = false;
    let mut selected: usize = ctx.data(|data| data.get_temp(category_id())).unwrap_or(0);
    if selected >= Category::ALL.len() {
        selected = 0;
    }

    let was_capturing = capturing_index(ctx).is_some();
    let size = overlay_size(ctx);
    let pos = ctx.content_rect().center() - size * 0.5;
    let modal = egui::Modal::new(overlay_id())
        .area(
            egui::Modal::default_area(overlay_id())
                .default_size(size)
                .fixed_pos(pos),
        )
        .backdrop_color(backdrop_color(settings.appearance.overlay_opacity))
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            // The overlay is the floating surface itself (no board ring around it).
            egui::Frame::new()
                .fill(crate::theme::raised(ui))
                .stroke(egui::Stroke::new(1.0, crate::theme::border_default(ui)))
                .corner_radius(egui::CornerRadius::same(10))
                .shadow(ui.visuals().window_shadow)
                .inner_margin(egui::Margin {
                    left: 6,
                    right: 6,
                    top: 6,
                    bottom: 6,
                })
                .show(ui, |ui| {
                    let theme = Theme::from_ui(ui);
                    ui.spacing_mut().interact_size.y = theme.spacing.control_h;
                    ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
                    settings_header(ui, &mut *open, theme);
                    hairline(ui);
                    ui.add_space(4.0);

                    let items: Vec<TabItem> = Category::ALL
                        .iter()
                        .map(|category| TabItem {
                            label: category.label().into_owned(),
                            path: None,
                            dirty: false,
                        })
                        .collect();
                    let tab_col_w = CATEGORY_RAIL_W.max(vertical_tab_width(
                        ui,
                        items.iter().map(|item| item.label.as_str()),
                    ));
                    let avail = ui.available_size();
                    let page_w =
                        PAGE_W.min((avail.x - tab_col_w - CONTENT_GAP - PAGE_PAD_X).max(0.0));
                    let content_w = page_w + PAGE_PAD_X;

                    tui::tui(ui, shell_node_id("card"))
                        .reserve_space(avail)
                        .style(card_row_style())
                        .show(|tree| {
                            tree.style(tab_col_style(tab_col_w))
                                .id(shell_node_id("tab_col"))
                                .ui(|ui| {
                                    let action = typ_edit_widgets::tab_bar::tab_bar(
                                        ui,
                                        "typ-edit-settings-tabs",
                                        &items,
                                        selected,
                                        TabBarOrientation::Vertical,
                                    );
                                    if let Some(TabAction::Activate(index)) = action {
                                        selected = index;
                                    }
                                });
                            tree.style(content_col_style(content_w))
                                .id(shell_node_id("content_col"))
                                .add(|tree| {
                                    let mut scroll_style = tui::flexible();
                                    scroll_style.size = Size {
                                        width: Dimension::length(content_w),
                                        height: Dimension::auto(),
                                    };
                                    tree.style(scroll_style)
                                        .id(shell_node_id("scroll"))
                                        .ui(|ui| {
                                            ui.spacing_mut().scroll =
                                                egui::style::ScrollStyle::floating();
                                            egui::ScrollArea::vertical()
                                                .id_salt("typ_edit_settings_scroll")
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    ui.set_width(page_w);
                                                    ui.set_min_width(page_w);
                                                    ui.set_max_width(page_w);
                                                    egui::Frame::new()
                                                        .inner_margin(egui::Margin {
                                                            left: 8,
                                                            right: 8,
                                                            top: 6,
                                                            bottom: 8,
                                                        })
                                                        .show(ui, |ui| {
                                                            let category = Category::ALL[selected];
                                                            changed |= pane_ui(
                                                                ui,
                                                                category,
                                                                settings,
                                                                catalog,
                                                                package_pane,
                                                                package_requests,
                                                            );
                                                            typ_edit_widgets::layout_probe::record(
                                                                ui,
                                                                shell_node_id("page"),
                                                                true,
                                                            );
                                                        });
                                                });
                                        });
                                });
                        });
                });
        });
    if modal.should_close() && !was_capturing {
        *open = false;
        clear_capture(ctx);
    }

    ctx.data_mut(|data| data.insert_temp(category_id(), selected));
    changed
}

/// Pane contents for one category; returns whether any setting in it changed.
fn pane_ui(
    ui: &mut egui::Ui,
    category: Category,
    settings: &mut crate::settings::Settings,
    catalog: &FontCatalog,
    package_pane: &mut packages::PackagePane<'_>,
    package_requests: &mut Vec<packages::PackageRequest>,
) -> bool {
    if category != Category::Shortcuts {
        clear_capture(ui.ctx());
    }
    match category {
        Category::Interface => interface_ui(ui, settings, catalog),
        Category::Shortcuts => shortcuts_ui(ui, settings),
        Category::Editor => editor_ui(ui, settings, catalog),
        Category::Export => export_ui(ui, settings),
        Category::Packages => packages::packages_ui(ui, package_pane, package_requests),
        Category::Preview => preview_ui(ui, settings),
        Category::Git => git_ui(ui, settings),
        Category::Collab => collab_ui(ui, settings),
        Category::Experimental => experimental_ui(ui, settings),
        Category::Debug => debug_ui(ui, settings),
        Category::Cache => cache_ui(ui),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings tests share the process-wide i18n language. Serialize the
    /// tests that read localized widths while a language-switch test mutates it.
    fn language_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Owned package state for the headless tests.
    #[derive(Default)]
    struct TestPanes {
        groups: Vec<crate::shell::settings_window::package_view::PackageGroup>,
        installed: std::collections::BTreeMap<String, Vec<String>>,
        selected_versions: std::collections::BTreeMap<String, usize>,
        query: String,
        installed_only: bool,
    }

    impl TestPanes {
        fn pane(&mut self) -> packages::PackagePane<'_> {
            packages::PackagePane {
                groups: &self.groups,
                installed: &self.installed,
                selected_versions: &mut self.selected_versions,
                query: &mut self.query,
                installed_only: &mut self.installed_only,
                loaded: true,
                loading: false,
                op_in_flight: false,
                error: None,
                status: None,
            }
        }
    }

    #[test]
    fn every_category_renders_headless() {
        crate::settings::set_config_dir_override(
            std::env::temp_dir().join(format!("typ-edit-settings-test-{}", std::process::id())),
        );
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        for index in 0..Category::ALL.len() {
            ctx.data_mut(|data| data.insert_temp(category_id(), index));
            let mut open = true;
            let mut settings = crate::settings::Settings::default();
            let mut panes = TestPanes::default();
            let mut requests = Vec::new();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..Default::default()
            };
            for _ in 0..2 {
                ctx.begin_pass(input.clone());
                let changed = show(
                    &ctx,
                    &mut open,
                    &mut settings,
                    &catalog,
                    &mut panes.pane(),
                    &mut requests,
                );
                assert!(!changed, "a fresh render must not report changes");
                ctx.end_pass().drop_without_applying_deltas();
            }
            assert!(open, "no input may close the overlay");
        }
    }

    #[test]
    fn vertical_tab_width_tracks_the_longest_label() {
        let ctx = egui::Context::default();
        let mut narrow = 0.0;
        let mut wide = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            narrow = vertical_tab_width(ui, ["通用"].into_iter());
            wide = vertical_tab_width(ui, ["Cursor & Selection"].into_iter());
        });
        assert!(
            wide > narrow,
            "a longer localized tab label must widen the rail"
        );
        output.textures_delta.clear();
    }

    #[test]
    fn language_switching_keeps_settings_width_stable() {
        let _guard = language_test_lock();
        crate::settings::set_config_dir_override(
            std::env::temp_dir().join(format!("typ-edit-settings-test-{}", std::process::id())),
        );
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        let mut open = true;
        let mut settings = crate::settings::Settings::default();
        let mut panes = TestPanes::default();
        let mut requests = Vec::new();
        let mut widths = Vec::new();
        for lang in [
            typ_edit_i18n::Lang::ZhCn,
            typ_edit_i18n::Lang::EnUs,
            typ_edit_i18n::Lang::ZhCn,
            typ_edit_i18n::Lang::EnUs,
            typ_edit_i18n::Lang::ZhCn,
        ] {
            typ_edit_i18n::set_language(lang);
            for _ in 0..3 {
                ctx.data_mut(|data| data.insert_temp(category_id(), 0));
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    ..Default::default()
                };
                ctx.begin_pass(input);
                let _ = show(
                    &ctx,
                    &mut open,
                    &mut settings,
                    &catalog,
                    &mut panes.pane(),
                    &mut requests,
                );
                ctx.end_pass().drop_without_applying_deltas();
            }
            widths.push(
                ctx.memory(|memory| memory.area_rect(overlay_id()))
                    .expect("settings overlay has an area rect")
                    .width(),
            );
        }
        typ_edit_i18n::set_language(typ_edit_i18n::Lang::EnUs);
        let mut category_widths = Vec::new();
        for index in 0..Category::ALL.len() {
            let category_ctx = egui::Context::default();
            category_ctx
                .all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
            for _ in 0..3 {
                category_ctx.data_mut(|data| data.insert_temp(category_id(), index));
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    ..Default::default()
                };
                category_ctx.begin_pass(input);
                let _ = show(
                    &category_ctx,
                    &mut open,
                    &mut settings,
                    &catalog,
                    &mut panes.pane(),
                    &mut requests,
                );
                category_ctx.end_pass().drop_without_applying_deltas();
            }
            category_widths.push(
                category_ctx
                    .memory(|memory| memory.area_rect(overlay_id()))
                    .expect("settings overlay has an area rect")
                    .width(),
            );
        }
        typ_edit_i18n::set_language(typ_edit_i18n::Lang::ZhCn);
        let baseline = widths[0];
        for width in widths {
            assert!(
                (width - baseline).abs() <= 1.0,
                "language switch changed the settings width: {width} vs {baseline}"
            );
        }
        for (index, width) in category_widths.into_iter().enumerate() {
            assert!(
                (width - OVERLAY_W).abs() <= 1.0,
                "category {index}: English overlay width {width} differs from {OVERLAY_W}"
            );
        }
    }

    #[test]
    fn every_settings_page_layout_fits_its_containers() {
        let _guard = language_test_lock();
        crate::settings::set_config_dir_override(
            std::env::temp_dir().join(format!("typ-edit-settings-test-{}", std::process::id())),
        );
        typ_edit_widgets::layout_probe::enable();
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        for index in 0..Category::ALL.len() {
            let (salt, page_count) = match Category::ALL[index] {
                Category::Interface => (Some("interface"), 3),
                Category::Editor => (Some("editor"), 3),
                Category::Preview => (Some("preview"), 2),
                _ => (None, 1),
            };
            for page in 0..page_count {
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
                if let Some(salt) = salt {
                    ctx.data_mut(|data| {
                        data.insert_temp(egui::Id::new(("typ_edit_settings_subtabs", salt)), page);
                    });
                }
                let mut open = true;
                let mut settings = crate::settings::Settings::default();
                let mut panes = TestPanes::default();
                let mut requests = Vec::new();
                for frame in 0..3 {
                    if frame == 2 {
                        typ_edit_widgets::layout_probe::clear();
                    }
                    ctx.data_mut(|data| data.insert_temp(category_id(), index));
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 800.0),
                        )),
                        ..Default::default()
                    };
                    ctx.begin_pass(input);
                    let _ = show(
                        &ctx,
                        &mut open,
                        &mut settings,
                        &catalog,
                        &mut panes.pane(),
                        &mut requests,
                    );
                    ctx.end_pass().drop_without_applying_deltas();
                }
                let records = typ_edit_widgets::layout_probe::records();
                assert!(
                    !records.is_empty(),
                    "settings page {index}/{page} produced no layout probe records"
                );
                for record in records {
                    let tol = 0.5;
                    assert!(
                        record.used.min.x >= record.container.min.x - tol
                            && record.used.max.x <= record.container.max.x + tol,
                        "horizontal child bounds escaped container {:?}: {:?} vs {:?}",
                        record.id,
                        record.used,
                        record.container
                    );
                    if !record.scrollable {
                        assert!(
                            record.used.min.y >= record.container.min.y - tol
                                && record.used.max.y <= record.container.max.y + tol,
                            "vertical child bounds escaped container {:?}: {:?} vs {:?}",
                            record.id,
                            record.used,
                            record.container
                        );
                    }
                }
            }
        }
        typ_edit_widgets::layout_probe::disable();
    }

    #[test]
    fn settings_overlay_width_is_stable() {
        let _guard = language_test_lock();
        crate::settings::set_config_dir_override(
            std::env::temp_dir().join(format!("typ-edit-settings-test-{}", std::process::id())),
        );
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        let mut widths = Vec::new();
        for index in 0..Category::ALL.len() {
            let mut open = true;
            let mut settings = crate::settings::Settings::default();
            let mut panes = TestPanes::default();
            let mut requests = Vec::new();
            for _ in 0..3 {
                ctx.data_mut(|data| data.insert_temp(category_id(), index));
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    ..Default::default()
                };
                ctx.begin_pass(input);
                let _ = show(
                    &ctx,
                    &mut open,
                    &mut settings,
                    &catalog,
                    &mut panes.pane(),
                    &mut requests,
                );
                ctx.end_pass().drop_without_applying_deltas();
            }
            let width = ctx
                .memory(|memory| memory.area_rect(overlay_id()))
                .expect("settings overlay has an area rect")
                .width();
            widths.push((index, width));
        }
        let baseline = widths[0].1;
        for (index, width) in widths {
            assert!(
                (width - baseline).abs() <= 1.0,
                "category {index}: overlay width {width} differs from {baseline}"
            );
        }
    }

    #[test]
    fn cache_page_width_does_not_grow_between_frames() {
        let _guard = language_test_lock();
        crate::settings::set_config_dir_override(
            std::env::temp_dir().join(format!("typ-edit-settings-test-{}", std::process::id())),
        );
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        let mut widths = Vec::new();
        let mut open = true;
        let mut settings = crate::settings::Settings::default();
        let mut panes = TestPanes::default();
        let mut requests = Vec::new();
        let cache_index = Category::ALL
            .iter()
            .position(|category| *category == Category::Cache)
            .expect("cache category");
        for _ in 0..10 {
            ctx.data_mut(|data| data.insert_temp(category_id(), cache_index));
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..Default::default()
            };
            ctx.begin_pass(input);
            let _ = show(
                &ctx,
                &mut open,
                &mut settings,
                &catalog,
                &mut panes.pane(),
                &mut requests,
            );
            ctx.end_pass().drop_without_applying_deltas();
            widths.push(
                ctx.memory(|memory| memory.area_rect(overlay_id()))
                    .expect("settings overlay has an area rect")
                    .width(),
            );
        }
        // The first two frames are the Modal/Area measurement passes; from the
        // third frame onward the cache page must not change width anymore.
        let first = widths[2];
        for (frame, width) in widths.into_iter().enumerate().skip(2) {
            assert!(
                (width - first).abs() <= 1.0,
                "cache frame {frame}: width {width} differs from {first}"
            );
        }
        assert!(
            (first - OVERLAY_W).abs() <= 1.0,
            "cache page width {first} differs from the fixed overlay {OVERLAY_W}"
        );
    }

    #[test]
    fn tables_span_the_overlay_width() {
        let _guard = language_test_lock();
        use std::collections::HashMap;

        crate::settings::set_config_dir_override(
            std::env::temp_dir().join(format!("typ-edit-settings-test-{}", std::process::id())),
        );
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        let dark_band = Color32::from_white_alpha(typ_edit_widgets::tokens::BAND_WHITE_ALPHA);
        let light_band = Color32::from_black_alpha(typ_edit_widgets::tokens::BAND_BLACK_ALPHA);
        for index in 0..Category::ALL.len() {
            ctx.data_mut(|data| data.insert_temp(category_id(), index));
            let mut open = true;
            let mut settings = crate::settings::Settings::default();
            let mut panes = TestPanes::default();
            let mut requests = Vec::new();
            // Row top (half-point bucket) -> horizontal extent of its bands.
            let mut band_rows: HashMap<i32, (f32, f32)> = HashMap::new();
            for frame in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    time: Some(frame as f64),
                    ..Default::default()
                };
                ctx.begin_pass(input);
                let changed = show(
                    &ctx,
                    &mut open,
                    &mut settings,
                    &catalog,
                    &mut panes.pane(),
                    &mut requests,
                );
                assert!(!changed, "a fresh render must not report changes");
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                for clipped in &output.shapes {
                    if let egui::epaint::Shape::Rect(rect) = &clipped.shape
                        && (rect.fill == dark_band || rect.fill == light_band)
                    {
                        let top = (rect.rect.top() * 2.0).round() as i32;
                        let extent = band_rows
                            .entry(top)
                            .or_insert((f32::INFINITY, f32::NEG_INFINITY));
                        extent.0 = extent.0.min(rect.rect.left());
                        extent.1 = extent.1.max(rect.rect.right());
                    }
                }
            }
            let widest = band_rows
                .values()
                .map(|(left, right)| right - left)
                .fold(0.0_f32, f32::max);
            assert!(open, "no input may close the overlay");
            // A one-row page has only the unstripped first row (and the cache
            // page has no table), so there is no band to measure there.
            if matches!(
                Category::ALL[index],
                Category::Export | Category::Experimental | Category::Preview | Category::Cache
            ) {
                continue;
            }
            // Interface/Editor/Preview reserve a nested vertical rail; their
            // tables fill the remaining content column rather than the whole
            // overlay width.
            let has_sub_tabs = matches!(
                Category::ALL[index],
                Category::Interface | Category::Editor | Category::Preview
            );
            let min_width = if Category::ALL[index] == Category::Interface {
                390.0
            } else if has_sub_tabs {
                300.0
            } else {
                450.0
            };
            assert!(
                widest >= min_width,
                "category {index}: widest striped band {widest} < {min_width}"
            );
        }
    }

    #[test]
    fn language_switch_keeps_stripe_extent_stable() {
        let _guard = language_test_lock();
        crate::settings::set_config_dir_override(
            std::env::temp_dir().join(format!("typ-edit-settings-test-{}", std::process::id())),
        );
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        let dark_band = Color32::from_white_alpha(typ_edit_widgets::tokens::BAND_WHITE_ALPHA);
        let light_band = Color32::from_black_alpha(typ_edit_widgets::tokens::BAND_BLACK_ALPHA);
        for index in 0..Category::ALL.len() {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
            let mut open = true;
            let mut settings = crate::settings::Settings::default();
            let mut panes = TestPanes::default();
            let mut requests = Vec::new();
            let mut extents = Vec::new();
            for lang in [
                typ_edit_i18n::Lang::ZhCn,
                typ_edit_i18n::Lang::EnUs,
                typ_edit_i18n::Lang::ZhCn,
            ] {
                typ_edit_i18n::set_language(lang);
                let mut right = f32::NEG_INFINITY;
                for frame in 0..3 {
                    ctx.data_mut(|data| data.insert_temp(category_id(), index));
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 800.0),
                        )),
                        time: Some(frame as f64),
                        ..Default::default()
                    };
                    ctx.begin_pass(input);
                    let _ = show(
                        &ctx,
                        &mut open,
                        &mut settings,
                        &catalog,
                        &mut panes.pane(),
                        &mut requests,
                    );
                    let mut output = ctx.end_pass();
                    output.textures_delta.clear();
                    for clipped in &output.shapes {
                        if let egui::epaint::Shape::Rect(rect) = &clipped.shape
                            && (rect.fill == dark_band || rect.fill == light_band)
                        {
                            right = right.max(rect.rect.right());
                        }
                    }
                }
                extents.push(right);
            }
            if extents[0].is_finite() {
                assert!(
                    (extents[0] - extents[2]).abs() <= 1.0,
                    "category {index}: stripe right edge did not return: {} -> {}",
                    extents[0],
                    extents[2]
                );
            }
        }
        typ_edit_i18n::set_language(typ_edit_i18n::Lang::ZhCn);
    }

    #[test]
    fn backdrop_color_scales_with_opacity_and_clamps() {
        assert_eq!(backdrop_color(0.0).a(), 0);
        assert_eq!(backdrop_color(1.0).a(), 255);
        assert_eq!(backdrop_color(2.0).a(), 255);
        assert_eq!(backdrop_color(-1.0).a(), 0);
        let alpha = i32::from(backdrop_color(0.7).a());
        assert!(
            (alpha - 179).abs() <= 1,
            "alpha {alpha} not within 1 of 179"
        );
    }

    fn escape_input(screen: egui::Rect) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn escape_while_capturing_cancels_the_recording_without_closing() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| crate::theme::install_card_style(&mut style.visuals));
        let catalog = typ_edit_engine::fonts::FontCatalog::new();
        ctx.data_mut(|data| data.insert_temp(category_id(), Category::Shortcuts as usize));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        let mut settings = crate::settings::Settings::default();
        let mut panes = TestPanes::default();
        let mut requests = Vec::new();

        let mut open = true;
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        });
        show(
            &ctx,
            &mut open,
            &mut settings,
            &catalog,
            &mut panes.pane(),
            &mut requests,
        );
        ctx.end_pass().drop_without_applying_deltas();
        assert!(open, "no input may close the overlay");

        set_capturing(&ctx, Some(0));
        assert!(capturing_index(&ctx).is_some());

        ctx.begin_pass(escape_input(screen));
        show(
            &ctx,
            &mut open,
            &mut settings,
            &catalog,
            &mut panes.pane(),
            &mut requests,
        );
        ctx.end_pass().drop_without_applying_deltas();
        assert!(open, "Escape while recording must not close the window");
        assert!(
            capturing_index(&ctx).is_none(),
            "Escape must cancel the recording"
        );

        ctx.begin_pass(escape_input(screen));
        show(
            &ctx,
            &mut open,
            &mut settings,
            &catalog,
            &mut panes.pane(),
            &mut requests,
        );
        ctx.end_pass().drop_without_applying_deltas();
        assert!(
            !open,
            "Escape with no recording active still closes the window"
        );
    }
}
