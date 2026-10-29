//! Context-menu handling for the horizontal tab strip: records which tab the
//! right-click targeted (in egui memory, so the menu can read it back) and
//! resolves the menu into the context-scoped [`TabAction`]s — the close
//! variants, [`TabAction::CopyPath`] and [`TabAction::RevealInExplorer`].

use super::{TabAction, TabItem};
use crate::menu::{menu_begin_sized, menu_item};
use typ_edit_i18n::t;

/// The context-menu actions resolved for one frame. At most one field is set;
/// [`TabContext::action`] maps them to a [`TabAction`] in menu dispatch order.
#[derive(Default)]
pub(super) struct TabContext {
    close: Option<usize>,
    close_others: Option<usize>,
    close_all: bool,
    copy: Option<usize>,
    reveal: Option<usize>,
}

impl TabContext {
    /// The context-scoped action, if any, in the menu's dispatch order.
    pub(super) fn action(&self) -> Option<TabAction> {
        if let Some(index) = self.close {
            Some(TabAction::Close(index))
        } else if let Some(index) = self.close_others {
            Some(TabAction::CloseOthers(index))
        } else if self.close_all {
            Some(TabAction::CloseAll)
        } else if let Some(index) = self.copy {
            Some(TabAction::CopyPath(index))
        } else {
            self.reveal.map(TabAction::RevealInExplorer)
        }
    }
}

/// Records the right-clicked tab under `id` so the menu can read it back.
pub(super) fn record_target(
    ui: &egui::Ui,
    response: &egui::Response,
    id: egui::Id,
    rects: &[egui::Rect],
) {
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(index) = rects.iter().position(|rect| rect.contains(pos))
    {
        ui.ctx().memory_mut(|m| m.data.insert_temp(id, index));
    }
}

/// Shows the tab context menu and resolves it into a [`TabContext`].
pub(super) fn context_menu(
    response: &egui::Response,
    id: egui::Id,
    tabs: &[TabItem],
) -> TabContext {
    let close = t("tab.close");
    let close_others = t("tab.close_others");
    let close_all = t("tab.close_all");
    let copy_path = t("tab.copy_path");
    let reveal = t("tab.reveal");
    let mut context = TabContext::default();
    // tui-whitelist: egui context-menu popup 是允许的容器机制；行由宽度测量与
    // egui 菜单按钮组成，打开/关闭/焦点仍归 egui（与 P4 的主菜单弹层一致）。
    response.context_menu(|ui| {
        let Some(index) = ui.ctx().memory(|m| m.data.get_temp::<usize>(id)) else {
            ui.close();
            return;
        };
        if index >= tabs.len() {
            ui.close();
            return;
        }
        menu_begin_sized(
            ui,
            &[
                (close.as_ref(), None),
                (close_others.as_ref(), None),
                (close_all.as_ref(), None),
                (copy_path.as_ref(), None),
                (reveal.as_ref(), None),
            ],
            false,
        );
        if menu_item(ui, close.as_ref(), None, true).clicked() {
            context.close = Some(index);
            ui.close();
        }
        if menu_item(ui, close_others.as_ref(), None, true).clicked() {
            context.close_others = Some(index);
            ui.close();
        }
        if menu_item(ui, close_all.as_ref(), None, true).clicked() {
            context.close_all = true;
            ui.close();
        }
        let has_path = tabs[index].path.is_some();
        if menu_item(ui, copy_path.as_ref(), None, has_path).clicked() {
            context.copy = Some(index);
            ui.close();
        }
        if menu_item(ui, reveal.as_ref(), None, has_path).clicked() {
            context.reveal = Some(index);
            ui.close();
        }
    });
    context
}
