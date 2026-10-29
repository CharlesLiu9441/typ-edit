//! tui 约定层:egui_taffy 的统一入口与样式预设。
//!
//! 迁移范围内(见 `docs/design/egui-taffy-migration-plan.md`)的所有布局代码
//! 只经由此模块使用 egui_taffy,避免各 crate 直接依赖其类型。
//!
//! # 四条规则(计划 §2.2)
//!
//! 1. **Id**:每个 tui 容器的 `Id` 由父 `ui.id()` + 语义 salt 派生
//!    (例如 `ui.id().with("shell_columns")`),禁止裸 `Id::new`。
//! 2. **换行**:文本节点显式给宽或设 [`egui::TextWrapMode`](短标签 `Truncate`/
//!    `Extend`,块文本填满父宽),不依赖 egui 默认最小宽换行。
//! 3. **滚动**:长列表/页面滚动继续用 `egui::ScrollArea` + 现有自绘滚动条;
//!    不采用 egui_taffy 的 overflow 滚动节点。taffy 只管滚动内容内部的排布。
//! 4. **多 pass**:egui 0.36 `max_passes` 默认 2;多 pass 首 pass 消费输入事件、
//!    后续 pass 不重放。事件驱动动作安全;每 pass 都执行的非事件副作用必须幂等;
//!    frame 级副作用留在 `frame_logic`。
//!
//! # 审计口径(计划 §2.6)
//!
//! 禁止:多子元素排布 API(`ui.horizontal*`、`ui.vertical*`、`ui.with_layout`、
//! `Layout::*`、`ui.allocate_ui*`、`ui.new_child`、`ui.scope_builder`、`ui.columns`)
//! 与逐子手工矩形排布。允许(白名单):纯绘制、单叶 `allocate_exact_size`/
//! `allocate_rect`/`allocate_space`、`ui.interact` 命中区、`ScrollArea`/`Area`/
//! `Modal`/`Window`/`Popup`/`ComboBox` 容器机制。

pub use egui_taffy::taffy;
pub use egui_taffy::{TaffyContainerUi, Tui, TuiBuilderLogic, tui};

use egui_taffy::taffy::{
    AlignItems, Dimension, Display, FlexDirection, FlexWrap, JustifyContent, Style,
};

/// 一行:子元素横向排列,交叉轴拉伸。
///
/// **taffy 0.9.2 的盒尺寸语义**(实测,`docs/design/egui-taffy-migration-plan.md` P1 记录):
/// 弹性分配尺寸与 `padding` 是**相加**的(近似 content-box,且 `box_sizing` 字段对 flex
/// 分配不生效)。因此:**宽度/高度策略节点不要加 padding**;内边距一律放到叶子节点或
/// 专用容器上(见 `shell/layout.rs` 的表头/正文样式)。
pub fn row() -> Style {
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Stretch),
        ..Default::default()
    }
}

/// 一列:子元素纵向排列,交叉轴拉伸。
pub fn column() -> Style {
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        align_items: Some(AlignItems::Stretch),
        ..Default::default()
    }
}

/// 一行:子元素从右向左排列,交叉轴居中。
///
/// 语义等价于 egui 的 `right_to_left` 行(对话框动作行):第一个添加的
/// 子元素位于最右侧,添加顺序(即 Tab 焦点顺序)保持不变。
pub fn row_reverse() -> Style {
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::RowReverse,
        align_items: Some(AlignItems::Center),
        ..Default::default()
    }
}

/// [`row`] 加自动换行:窄容器里的多个短控件(复选框、过滤片)按行回绕。
///
/// 与 egui 的 `horizontal_wrapped` 等价:子元素保持自身内容宽,空间不足时
/// 换到下一行;交叉轴起点对齐,避免不同高度的控件被拉伸。
pub fn row_wrap(gap: f32) -> Style {
    let mut style = row_gap(gap);
    style.flex_wrap = FlexWrap::Wrap;
    style.align_items = Some(AlignItems::Start);
    style
}

/// 让节点在主轴上填满剩余空间、交叉轴随内容(`auto`),用于内容列。
pub fn fill_main(style: &mut Style) {
    style.flex_basis = Dimension::length(0.0);
    style.flex_grow = 1.0;
    style.flex_shrink = 1.0;
    style.min_size = taffy::Size {
        width: Dimension::length(0.0),
        height: Dimension::length(0.0),
    };
}

/// 主轴弹性的一列(基准 0、填满剩余空间),用于面板正文外层。
pub fn flexible_column() -> Style {
    let mut style = column();
    fill_main(&mut style);
    style
}

/// 水平/垂直都居中、主轴填满剩余空间的一列,用于空状态。
pub fn centered_column() -> Style {
    let mut style = flexible_column();
    style.align_items = Some(AlignItems::Center);
    style.justify_content = Some(JustifyContent::Center);
    style
}

/// [`row`] 加横向子元素间距,交叉轴居中(普通表单行)。
pub fn row_gap(gap: f32) -> Style {
    let mut style = row();
    style.align_items = Some(AlignItems::Center);
    style.gap = taffy::Size {
        width: taffy::LengthPercentage::length(gap),
        height: taffy::LengthPercentage::length(0.0),
    };
    style
}

/// [`row_reverse`] 加横向子元素间距,纵向 gap 为零。
pub fn row_reverse_gap(gap: f32) -> Style {
    let mut style = row_reverse();
    style.gap = taffy::Size {
        width: taffy::LengthPercentage::length(gap),
        height: taffy::LengthPercentage::length(0.0),
    };
    style
}

/// [`column`] 加竖向子元素间距,横向 gap 为零(对话框内容列)。
pub fn column_gap(gap: f32) -> Style {
    let mut style = column();
    style.gap = taffy::Size {
        width: taffy::LengthPercentage::length(0.0),
        height: taffy::LengthPercentage::length(gap),
    };
    style
}

/// 让节点铺满可用宽度,高度随内容(`auto`)。
///
/// 用于 `reserve_width` 打开的顶层树根:根样式默认 `auto` 宽会收缩到内容,
/// 右对齐的动作行就贴不到预留宽度的右缘。
pub fn fill_width(style: &mut Style) {
    style.size = taffy::Size {
        width: Dimension::percent(1.0),
        height: Dimension::auto(),
    };
}

/// 固定内容宽、不参与收缩的自动尺寸叶(短文本标签)。
///
/// 行内的文本标签默认 `flex_shrink: 1`,在窄行里会被挤到裁剪;标签宽度应由
/// 文本本身决定,因此显式禁止收缩。
pub fn label_leaf() -> Style {
    Style {
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// 让节点在主轴上填满剩余空间(`flex_grow = 1`)。
pub fn grow(style: &mut Style) {
    style.flex_grow = 1.0;
}

/// 固定主轴尺寸的叶节点样式(配合 [`row`]/[`column`] 使用)。
pub fn fixed(main: f32) -> Style {
    Style {
        flex_grow: 0.0,
        flex_shrink: 0.0,
        flex_basis: Dimension::length(main),
        ..Default::default()
    }
}

/// 固定主轴尺寸的节点(与 [`fixed`] 同义,按"主轴"语义命名)。
pub fn fixed_main(main: f32) -> Style {
    fixed(main)
}

/// 主轴弹性节点:基准 0、填满剩余空间。
pub fn flexible() -> Style {
    let mut style = Style {
        flex_basis: Dimension::length(0.0),
        flex_grow: 1.0,
        flex_shrink: 1.0,
        ..Default::default()
    };
    style.min_size = taffy::Size {
        width: Dimension::length(0.0),
        height: Dimension::length(0.0),
    };
    style
}

/// 主轴弹性 + 行布局:用于"填满可用空间的一行容器"(如中央三栏行)。
pub fn flexible_row() -> Style {
    let mut style = row();
    style.flex_basis = Dimension::length(0.0);
    style.flex_grow = 1.0;
    style.flex_shrink = 1.0;
    style.min_size = taffy::Size {
        width: Dimension::length(0.0),
        height: Dimension::length(0.0),
    };
    style
}

/// 让容器节点铺满其可用空间(`size: 100% × 100%`)。
pub fn fill(style: &mut Style) {
    style.size = taffy::Size {
        width: Dimension::percent(1.0),
        height: Dimension::percent(1.0),
    };
}
