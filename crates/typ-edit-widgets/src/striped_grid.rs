//! A striped two/three-column table for settings panes and multi-parameter
//! dialogs: an item label column and a value column, with alternating row
//! stripes and full-width bands.
//!
//! The stripe colour is the globally installed `faint_bg_color` token
//! (`crate::theme::install_card_style` / the root crate's equivalent), so a row
//! band always matches the app's table look.
//!
//! The table is one taffy grid per page ([`tui_grid`]/[`TuiGrid`]): column
//! `auto` sizes to the widest label on the page, so the label column and the
//! value column line up across every row. The pre-P6 legacy `egui::Grid`
//! entry points (`striped_grid`/`striped_grid3`/`grid_row`/`grid_row3`) had no
//! remaining consumers once the dialogs moved to `tui_grid` (P3) and were
//! removed in P6.

use egui::{Color32, CornerRadius, RichText, Sense, TextStyle};

use crate::layout_probe;

use crate::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{
        AlignItems, Dimension, Display, GridAutoFlow, GridTemplateComponent, JustifyContent,
        LengthPercentage, Size, Style, style_helpers,
    },
};

/// Fixed height of one table row; fits the tallest control (a 28-high segmented
/// selector or input).
const ROW_H: f32 = 28.0;

/// Column gap of the tui tables (matches the legacy `egui::Grid` spacing).
const TUI_COL_GAP: f32 = 12.0;
/// Row gap of the tui tables (matches the legacy `egui::Grid` spacing).
const TUI_ROW_GAP: f32 = 8.0;

/// Maximum width of the item label column. Long localized labels truncate
/// here (with the full text on hover) instead of forcing the whole settings
/// Modal to grow.
const LABEL_MAX_W: f32 = 180.0;

/// One page's tui table: [`tui_grid`]/[`tui_grid3`] open the grid tree and hand
/// this builder to the page closure, which appends one row at a time.
///
/// Node ids derive from `id_salt` + row index + cell role, so they are unique
/// within the page and stable across frames (taffy state and widget ids never
/// drift).
pub struct TuiGrid<'t> {
    tree: &'t mut Tui,
    id_salt: &'static str,
    row: usize,
    /// Right edge of the table, used to fill row bands even when a cell's
    /// localized content is narrower (or temporarily wider) than its column.
    table_right: f32,
}

/// Draws a striped two-column table (item | value) as one taffy grid tree.
///
/// The tree only reserves the page width: the height follows the rows inside
/// the settings `ScrollArea` (never `reserve_available_space`, which would be
/// infinite there).
pub fn tui_grid(ui: &mut egui::Ui, id_salt: &'static str, add: impl FnOnce(&mut TuiGrid<'_>)) {
    tui_table(ui, id_salt, 2, add);
}

/// Three-column variant of [`tui_grid`]: item | value | actions.
pub fn tui_grid3(ui: &mut egui::Ui, id_salt: &'static str, add: impl FnOnce(&mut TuiGrid<'_>)) {
    tui_table(ui, id_salt, 3, add);
}

fn tui_table(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    columns: usize,
    add: impl FnOnce(&mut TuiGrid<'_>),
) {
    let width = ui.available_width();
    let table_right = ui.max_rect().right();
    tui::tui(
        ui,
        egui::Id::new((
            "typ_edit_tui_grid",
            id_salt,
            typ_edit_i18n::language().code(),
        )),
    )
    .reserve_width(width)
    .style(table_style(columns))
    .show(|tree| {
        add(&mut TuiGrid {
            tree,
            id_salt,
            row: 0,
            table_right,
        })
    });
}

impl TuiGrid<'_> {
    /// The page `Ui` the table was opened on (theme / style lookups).
    pub fn egui_ui(&self) -> &egui::Ui {
        self.tree.egui_ui()
    }

    /// Appends one row: a right-aligned label and a value cell hosting exactly
    /// one control (the control gets the whole value column width).
    pub fn row(&mut self, label: &str, add_value: impl FnOnce(&mut egui::Ui)) {
        let (row, stripe, salt) = self.next_row();
        self.label_cell(row, label, stripe);
        self.value_cell(row, stripe, TUI_COL_GAP * 0.5, |tree| {
            tree.style(tui::flexible())
                .id(cell_id(salt, row, "value_leaf"))
                .ui(add_value);
        });
    }

    /// Appends one row whose value cell hosts tui children directly.
    ///
    /// Escape hatch for rows with several side-by-side controls: the closure
    /// adds one tui leaf per control to the value cell (a row with the default
    /// item gap, vertically centred), instead of a single egui `Ui` leaf.
    pub fn row_tui(&mut self, label: &str, add_value: impl FnOnce(&mut Tui)) {
        let (row, stripe, _salt) = self.next_row();
        self.label_cell(row, label, stripe);
        self.value_cell(row, stripe, TUI_COL_GAP * 0.5, add_value);
    }

    /// Appends one row whose value cell is a control plus an inline note.
    ///
    /// The note wraps inside the same value cell (and therefore the same
    /// striped band) instead of becoming a separate full-width row.
    pub fn row_note(&mut self, label: &str, note: &str, add_value: impl FnOnce(&mut egui::Ui)) {
        let (row, stripe, salt) = self.next_row();
        let table_right = self.table_right;
        self.label_cell(row, label, stripe);
        let gap = self.tree.egui_ui().spacing().item_spacing.x;
        let note = note.to_owned();
        self.tree
            .style(value_cell_style(gap))
            .id(cell_id(salt, row, "value"))
            .add_with_background_ui(
                move |ui, container| {
                    paint_stripe(
                        ui,
                        container.full_container(),
                        stripe,
                        TUI_COL_GAP * 0.5,
                        table_right,
                    )
                },
                |tree, _| {
                    let mut column = tui::column_gap(2.0);
                    tui::fill_width(&mut column);
                    tree.style(column)
                        .id(cell_id(salt, row, "value_col"))
                        .add(|tree| {
                            let mut value_row = tui::row();
                            tui::fill_width(&mut value_row);
                            tree.style(value_row)
                                .id(cell_id(salt, row, "value_row"))
                                .add(|tree| {
                                    tree.style(tui::flexible())
                                        .id(cell_id(salt, row, "value_leaf"))
                                        .ui(add_value);
                                });
                            let mut note_style = tui::label_leaf();
                            tui::fill_width(&mut note_style);
                            tree.style(note_style)
                                .wrap_mode(egui::TextWrapMode::Wrap)
                                .id(cell_id(salt, row, "value_note"))
                                .ui(|ui| {
                                    ui.label(
                                        RichText::new(note).small().color(
                                            crate::theme::Theme::from_ui(ui).colors.text_muted,
                                        ),
                                    );
                                });
                        });
                    layout_probe::record(tree.egui_ui(), cell_id(salt, row, "value"), false);
                },
            );
    }

    /// Appends one three-column row: label | value (tui children) | actions
    /// (tui children, pushed to the cell's right edge).
    pub fn row3(
        &mut self,
        label: &str,
        add_value: impl FnOnce(&mut Tui),
        add_actions: impl FnOnce(&mut Tui),
    ) {
        let (row, stripe, salt) = self.next_row();
        self.label_cell(row, label, stripe);
        self.value_cell(row, stripe, TUI_COL_GAP * 0.5, add_value);
        self.actions_cell(row, stripe, salt, add_actions);
    }

    fn next_row(&mut self) -> (usize, bool, &'static str) {
        let row = self.row;
        self.row += 1;
        (row, row % 2 == 1, self.id_salt)
    }

    /// The right-aligned label cell: measures the label with the body font,
    /// reports that width to the grid's `auto` track, and paints the galley at
    /// the cell's right edge, vertically centred.
    fn label_cell(&mut self, row: usize, label: &str, stripe: bool) {
        let salt = self.id_salt;
        let table_right = self.table_right;
        self.tree
            .style(label_cell_style())
            .id(cell_id(salt, row, "label"))
            .ui(|ui| {
                let rect = ui.max_rect();
                paint_stripe(ui, rect, stripe, 2.0, table_right);
                let mut job = egui::text::LayoutJob::simple_singleline(
                    label.to_owned(),
                    TextStyle::Body.resolve(ui.style()),
                    Color32::PLACEHOLDER,
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width(LABEL_MAX_W);
                let galley = ui.painter().layout_job(job);
                let size = galley.size();
                // The allocation is what the node reports as its content size,
                // i.e. the label column's `auto` width. Cap it so a long
                // localized label cannot widen the whole Modal.
                // tui-whitelist: 单叶 allocate_exact_size（只上报标签宽高供 taffy auto 列量宽）。
                let (_, response) = ui.allocate_exact_size(size, Sense::hover());
                if galley.size().x >= LABEL_MAX_W - 0.5 {
                    response.on_hover_text(label);
                }
                ui.painter().galley(
                    egui::pos2(rect.right() - size.x, rect.center().y - size.y * 0.5),
                    galley,
                    ui.visuals().text_color(),
                );
                layout_probe::record(ui, cell_id(salt, row, "label"), false);
            });
    }

    /// The value cell: a stretched row container (the band is painted by its
    /// background callback) whose children are centred vertically. `row` puts
    /// one flexible leaf inside; `row_tui` adds the tui children directly.
    fn value_cell(
        &mut self,
        row: usize,
        stripe: bool,
        expand_left: f32,
        add_value: impl FnOnce(&mut Tui),
    ) {
        let salt = self.id_salt;
        let gap = self.tree.egui_ui().spacing().item_spacing.x;
        let table_right = self.table_right;
        self.tree
            .style(value_cell_style(gap))
            .id(cell_id(salt, row, "value"))
            .add_with_background_ui(
                move |ui, container| {
                    paint_stripe(
                        ui,
                        container.full_container(),
                        stripe,
                        expand_left,
                        table_right,
                    )
                },
                |tree, _| add_value(tree),
            );
        layout_probe::record(self.tree.egui_ui(), cell_id(salt, row, "value"), false);
    }

    /// The third cell of [`TuiGrid::row3`]: right-aligned actions.
    fn actions_cell(
        &mut self,
        row: usize,
        stripe: bool,
        salt: &'static str,
        add_actions: impl FnOnce(&mut Tui),
    ) {
        let gap = self.tree.egui_ui().spacing().item_spacing.x;
        let table_right = self.table_right;
        self.tree
            .style(actions_cell_style(gap))
            .id(cell_id(salt, row, "actions"))
            .add_with_background_ui(
                move |ui, container| {
                    paint_stripe(
                        ui,
                        container.full_container(),
                        stripe,
                        TUI_COL_GAP * 0.5,
                        table_right,
                    )
                },
                |tree, _| add_actions(tree),
            );
        layout_probe::record(self.tree.egui_ui(), cell_id(salt, row, "actions"), false);
    }
}

/// Stable id of one grid cell (or of a cell's inner leaf).
fn cell_id(id_salt: &str, row: usize, role: &'static str) -> egui::Id {
    egui::Id::new((
        "typ_edit_tui_grid",
        id_salt,
        typ_edit_i18n::language().code(),
        row,
        role,
    ))
}

/// Root grid style: `[auto, 1fr]` (`[auto, auto, 1fr]` for three columns), one
/// page width, content height, 12×8 gaps. No padding anywhere (taffy 0.9.2 adds
/// padding on top of the distributed size).
fn table_style(columns: usize) -> Style {
    let grid_template_columns: Vec<GridTemplateComponent<String>> = if columns == 3 {
        vec![
            style_helpers::auto(),
            style_helpers::auto(),
            style_helpers::fr(1.0_f32),
        ]
    } else {
        vec![style_helpers::auto(), style_helpers::fr(1.0_f32)]
    };
    Style {
        display: Display::Grid,
        grid_template_columns,
        grid_auto_flow: GridAutoFlow::Row,
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::auto(),
        },
        gap: Size {
            width: LengthPercentage::length(TUI_COL_GAP),
            height: LengthPercentage::length(TUI_ROW_GAP),
        },
        ..Default::default()
    }
}

/// Label cell: at least one row high (the widest label sets the column width).
fn label_cell_style() -> Style {
    Style {
        min_size: Size {
            width: Dimension::auto(),
            height: Dimension::length(ROW_H),
        },
        ..Default::default()
    }
}

/// Value/actions cell: a horizontal, vertically centred row of controls.
fn value_cell_style(gap: f32) -> Style {
    let mut style = tui::row();
    style.align_items = Some(AlignItems::Center);
    style.gap = Size {
        width: LengthPercentage::length(gap),
        height: LengthPercentage::length(0.0),
    };
    style
}

/// Actions cell: like [`value_cell_style`], pushed to the right edge.
fn actions_cell_style(gap: f32) -> Style {
    let mut style = value_cell_style(gap);
    style.justify_content = Some(JustifyContent::End);
    style
}

/// Paints the alternating row band behind a cell.
///
/// The band is expanded by half of each adjacent gap (and by 2pt at the outer
/// page edges) and rounded by 2pt, so the label/value/actions cells of one row
/// form the same continuous, rounded band `egui::Grid::striped` painted.
// tui-whitelist: 纯绘制（行带背景），不参与子元素排布。
fn paint_stripe(ui: &egui::Ui, rect: egui::Rect, stripe: bool, expand_left: f32, table_right: f32) {
    if stripe {
        let rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() - expand_left, rect.top() - TUI_ROW_GAP * 0.5),
            egui::pos2(
                table_right.max(rect.left()),
                rect.bottom() + TUI_ROW_GAP * 0.5,
            ),
        );
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), ui.visuals().faint_bg_color);
    }
}
