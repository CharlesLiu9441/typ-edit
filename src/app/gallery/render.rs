//! The template gallery window shell: the modal, its header, search box and
//! status line. The template list lives in [`templates`].
//!
//! Every user interaction is pushed into [`GalleryAction`] and applied by the
//! caller after the modal closes. The modal content is a plain egui `Ui` host,
//! so the header and the template rows open independent top-level tui trees
//! (they are not nested inside another tui).

use super::super::state::GalleryState;
use super::GalleryAction;
use egui::NumExt;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::text_input::form_input;
use typ_edit_widgets::tui::{self, TuiBuilderLogic, taffy::AlignItems};

mod templates;

impl GalleryState {
    /// Renders the gallery window if open. Close/Refresh are applied here; a
    /// chosen template is returned for the caller to apply.
    pub(in crate::app) fn ui(&mut self, ui: &mut egui::Ui) -> Option<GalleryAction> {
        if !self.open {
            return None;
        }
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        let size = gallery_size(&ctx);
        let modal = egui::Modal::new(egui::Id::new("template-gallery"))
            .area(egui::Modal::default_area(egui::Id::new("template-gallery")).default_size(size))
            .frame(egui::Frame::NONE)
            .show(&ctx, |ui| {
                // The former fixed-size content region, now an explicit size on
                // the modal's plain Ui host.
                ui.set_width(size.x);
                ui.set_height(size.y);
                egui::Frame::new()
                    .fill(crate::theme::card(ui))
                    .stroke(egui::Stroke::new(1.0, crate::theme::card_stroke(ui)))
                    .corner_radius(egui::CornerRadius::same(10))
                    .shadow(ui.visuals().window_shadow)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        gallery_frame(ui, self, &mut actions);
                    });
            });
        let should_close = modal.should_close();
        if should_close {
            actions.push(GalleryAction::Close);
        }
        let mut use_template = None;
        for action in actions {
            if let Some(action) = self.apply_action(action, &ctx) {
                use_template = Some(action);
            }
        }
        use_template
    }
}

/// The gallery's fixed size, clamped so it never spills off the content rect.
fn gallery_size(ctx: &egui::Context) -> egui::Vec2 {
    let max = (ctx.content_rect().size() - egui::Vec2::splat(24.0)).at_least(egui::Vec2::ZERO);
    egui::vec2(760.0, 560.0).at_most(max)
}

/// Header, search box, status line, and the template body.
fn gallery_frame(ui: &mut egui::Ui, gallery: &mut GalleryState, actions: &mut Vec<GalleryAction>) {
    // One top-level tui row for the title and the two actions: title on the
    // left, a flexible spring, then refresh and close (close stays rightmost).
    let width = ui.available_width().max(120.0);
    let base = ui.id().with("typ_edit_gallery_header");
    let mut header = tui::row_gap(ui.spacing().item_spacing.x);
    header.align_items = Some(AlignItems::Center);
    tui::fill_width(&mut header);
    tui::tui(ui, base)
        .reserve_width(width)
        .style(header)
        .show(|tree| {
            tree.style(tui::label_leaf())
                .id(base.with("title"))
                .wrap_mode(egui::TextWrapMode::Extend)
                .label(egui::RichText::new(t("gallery.title")).size(15.0).strong());
            tree.style(tui::flexible())
                .id(base.with("spring"))
                .add_empty();
            if tree
                .id(base.with("refresh"))
                .ui(|ui| {
                    button(
                        ui,
                        &t("gallery.refresh"),
                        ButtonVariant::Quiet,
                        !gallery.loading,
                    )
                })
                .clicked()
            {
                actions.push(GalleryAction::Refresh);
            }
            if tree
                .id(base.with("close"))
                .ui(|ui| button(ui, &t("gallery.close"), ButtonVariant::Secondary, true))
                .clicked()
            {
                actions.push(GalleryAction::Close);
            }
        });

    let width = ui.available_width().max(120.0);
    form_input(ui, &mut gallery.query, &t("gallery.search"), width);

    if gallery.loading {
        ui.label(egui::RichText::new(t("gallery.loading")).weak());
    }
    if let Some(error) = &gallery.error {
        ui.colored_label(
            crate::theme::error(ui),
            typ_edit_i18n::t_with("gallery.error", &[("err", error)]),
        );
    }
    if let Some((message, at)) = &gallery.status
        && at.elapsed() < std::time::Duration::from_secs(6)
    {
        ui.label(egui::RichText::new(message).weak().small());
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }

    separator(ui);
    templates::body(ui, gallery, actions);
}
