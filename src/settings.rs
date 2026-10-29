//! Persisted application settings (JSON in the platform config directory).
//!
//! The file is versioned (`schema_version`) and nested by domain. Legacy flat
//! files written before versioning are migrated on load; a file written by a
//! newer build is loaded best-effort and never overwritten for the session.

use crate::platform::fs_ops;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current on-disk settings schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Default LAN collaboration listen port (probed upward when taken).
pub(crate) const COLLAB_DEFAULT_PORT: u16 = 39950;

/// Serde default for [`Settings::schema_version`]: the version this build writes.
fn current_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// Which theme the UI follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePref {
    /// Follow the operating system's preference.
    #[default]
    System,
    Light,
    Dark,
}

/// UI language preference.
///
/// `System` resolves to the operating system locale (falling back to the
/// environment then Chinese); the explicit variants pin a shipped language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LanguagePref {
    /// Follow the operating system's locale.
    #[default]
    System,
    /// Simplified Chinese.
    ZhCn,
    /// English (US).
    EnUs,
}

/// How the editor soft-wraps long lines.
///
/// Persisted as a snake_case string (`"off"` | `"always"` | `"markup"`). The
/// legacy boolean field `wrap_lines` is still accepted on load through
/// [`LegacySettingsV0`] (`true` → [`WrapPref::Always`], `false` → [`WrapPref::Off`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapPref {
    /// Never wrap: every line is one row and scrolls horizontally.
    #[default]
    Off,
    /// Always soft-wrap to the viewport width.
    Always,
    /// Wrap only lines that are entirely markup mode.
    Markup,
}

impl WrapPref {
    /// Maps this persisted preference to the editor's wrap mode.
    pub fn to_mode(self) -> typ_edit_editor::WrapMode {
        match self {
            Self::Off => typ_edit_editor::WrapMode::Off,
            Self::Always => typ_edit_editor::WrapMode::Always,
            Self::Markup => typ_edit_editor::WrapMode::Markup,
        }
    }
}

impl<'de> Deserialize<'de> for WrapPref {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct WrapPrefVisitor;

        impl serde::de::Visitor<'_> for WrapPrefVisitor {
            type Value = WrapPref;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a wrap mode string or a legacy boolean")
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<WrapPref, E> {
                Ok(if value {
                    WrapPref::Always
                } else {
                    WrapPref::Off
                })
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<WrapPref, E> {
                match value {
                    "off" => Ok(WrapPref::Off),
                    "always" => Ok(WrapPref::Always),
                    "markup" => Ok(WrapPref::Markup),
                    other => Err(E::unknown_variant(other, &["off", "always", "markup"])),
                }
            }
        }

        deserializer.deserialize_any(WrapPrefVisitor)
    }
}

/// Preview cursor style preference.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CursorStylePref {
    #[default]
    Line,
    Ring,
    Mask,
}

impl CursorStylePref {
    /// Maps this persisted preference to the preview widget's cursor style.
    pub fn to_preview(self) -> typ_edit_preview::CursorStyle {
        match self {
            Self::Line => typ_edit_preview::CursorStyle::Line,
            Self::Ring => typ_edit_preview::CursorStyle::Ring,
            Self::Mask => typ_edit_preview::CursorStyle::Mask,
        }
    }
}

/// Default cap for the recent-projects MRU list.
pub const RECENT_PROJECTS_DEFAULT_LIMIT: usize = 10;
/// Lower bound accepted for [`WorkspaceSettings::recent_projects_limit`].
pub const RECENT_PROJECTS_LIMIT_MIN: usize = 1;
/// Upper bound accepted for [`WorkspaceSettings::recent_projects_limit`].
pub const RECENT_PROJECTS_LIMIT_MAX: usize = 30;

/// Clamps a persisted recent-projects limit into the accepted range.
pub fn clamp_recent_limit(limit: usize) -> usize {
    limit.clamp(RECENT_PROJECTS_LIMIT_MIN, RECENT_PROJECTS_LIMIT_MAX)
}

/// Test-only redirect for [`config_dir`]; the first set wins per test binary.
#[cfg(test)]
static CONFIG_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Test-only: point every config-directory access at `path`, so tests never
/// touch the real user config. Idempotent; later calls are ignored.
#[cfg(test)]
pub(crate) fn set_config_dir_override(path: PathBuf) {
    let _ = CONFIG_DIR_OVERRIDE.set(path);
}

/// The platform config directory (`%APPDATA%\typ-edit\typ-edit` on Windows);
/// `None` when it cannot be resolved. No directory is created here.
pub(crate) fn config_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = CONFIG_DIR_OVERRIDE.get() {
        return Some(dir.clone());
    }
    directories::ProjectDirs::from("dev", "typ-edit", "typ-edit")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// A path inside the app's config-directory `cache/` folder for a cached
/// artifact (for example the `@preview` registry index). `None` when the
/// platform config directory cannot be resolved; no directory is created here.
pub(crate) fn cache_file(name: &str) -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("cache").join(name))
}

/// Serde default for [`WorkspaceSettings::recent_projects_limit`] (the
/// struct-level `#[serde(default)]` would otherwise use `usize`'s zero).
fn default_recent_projects_limit() -> usize {
    RECENT_PROJECTS_DEFAULT_LIMIT
}

/// Serde default for [`RecoverySettings::crash_recovery_enabled`]: the feature
/// is on unless the config explicitly turns it off.
fn default_crash_recovery_enabled() -> bool {
    true
}

/// Debug frame-tree renderer options: the feature toggle plus which per-node
/// details the tree shows. Every display flag defaults on; the feature itself
/// defaults off, so the preview is unchanged until the user opts in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugFrameTreeOptions {
    /// Replace the whole preview area with the compiled frame tree.
    pub enabled: bool,
    /// Show a container node's number of children.
    pub show_children: bool,
    /// Show a text node's (truncated) content.
    pub show_text: bool,
    /// Show walk position plus node size, in points.
    pub show_geometry: bool,
    /// Show a node's fill/stroke/font styling.
    pub show_style: bool,
    /// Show a node's source file and byte range.
    pub show_span: bool,
    /// Show `Group` nodes. Hiding a group flattens its children into the parent
    /// level; the group's transform still applies to their highlight geometry.
    pub show_group_nodes: bool,
    /// Show `Text` nodes.
    pub show_text_nodes: bool,
    /// Show `Shape` nodes.
    pub show_shape_nodes: bool,
    /// Show `Image` nodes.
    pub show_image_nodes: bool,
    /// Show `Link` nodes.
    pub show_link_nodes: bool,
    /// Show `Tag` nodes.
    pub show_tag_nodes: bool,
    /// Inspector highlight color as straight (unmultiplied) RGBA; used for the
    /// hovered node's rectangle in the preview and band in the editor. Defaults
    /// to a translucent green so it reads as distinct from the user selection.
    pub highlight_color: [u8; 4],
}

impl Default for DebugFrameTreeOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            show_children: true,
            show_text: true,
            show_geometry: true,
            show_style: true,
            show_span: true,
            show_group_nodes: true,
            show_text_nodes: true,
            show_shape_nodes: true,
            show_image_nodes: true,
            show_link_nodes: true,
            show_tag_nodes: true,
            highlight_color: [80, 200, 120, 110],
        }
    }
}

/// Editor behavior: wrap/format/spacing and completion cap.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// How the editor soft-wraps long lines. Accepts the snake_case string form
    /// only; the legacy `wrap_lines` boolean alias lives on [`LegacySettingsV0`].
    pub wrap: WrapPref,
    /// Multiplier of the base font row height for the gap between consecutive
    /// logical lines. `1.0` is today's layout (no extra gap).
    pub line_spacing: f32,
    /// Multiplier of the base font row height for the gap between soft-wrap
    /// continuation rows of one logical line. `1.0` is today's layout.
    pub wrap_line_spacing: f32,
    /// Maximum number of completion candidates kept in the popup.
    pub completion_max: usize,
    /// Pretty-print the buffer with typstyle before every save (output normalised to LF).
    pub format_on_save: bool,
    /// Spell-check the prose in Typst buffers (English only for now; default off).
    pub spell_check_enabled: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            wrap: WrapPref::Off,
            line_spacing: 1.0,
            wrap_line_spacing: 1.0,
            completion_max: typ_edit_editor::COMPLETION_MAX_DEFAULT,
            format_on_save: true,
            spell_check_enabled: false,
        }
    }
}

/// UI and editor font families and sizes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FontSettings {
    /// UI font family name (platform default: Microsoft YaHei on Windows,
    /// PingFang SC on macOS, Noto Sans SC elsewhere).
    pub ui_font: String,
    pub ui_font_size: f32,
    /// Editor font family name (platform default: Menlo on macOS, Consolas on
    /// Windows, DejaVu Sans Mono elsewhere).
    pub editor_font: String,
    pub editor_font_size: f32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            ui_font: crate::ui_fonts::default_ui_font_name().to_owned(),
            ui_font_size: 14.0,
            editor_font: crate::ui_fonts::default_editor_font_name().to_owned(),
            editor_font_size: 14.0,
        }
    }
}

/// Preview viewport: cursor, selection and raster resolution.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewSettings {
    /// Keep the preview scrolled to the editor cursor's mapped position;
    /// toggled by the preview zoom bar's 跟随 button (default off).
    pub auto_scroll_preview: bool,
    /// 预览光标竖线宽度（px）。
    pub cursor_width: f32,
    /// 预览光标样式：竖线 / 圆环 / 遮罩。
    pub cursor_style: CursorStylePref,
    /// 预览光标是否闪烁。
    pub cursor_blink: bool,
    /// 闪烁间隔（一次显示/隐藏相位的时长，毫秒）。
    pub cursor_blink_interval: f32,
    /// 预览选区遮罩颜色（RGB）。
    pub selection_color: [u8; 3],
    /// 预览选区遮罩透明度（0.0–1.0）。
    pub selection_opacity: f32,
    /// Resolution of the whole-page fallback raster in the preview.
    pub preview_dpi: f32,
    /// Replay preview pages through the GPU canvas renderer (experimental;
    /// pages with unsupported content fall back to the raster pipeline).
    pub gpu_canvas_preview: bool,
    /// Longest side allowed for a GPU source image before CPU downsampling.
    #[serde(default = "default_gpu_image_max_side")]
    pub gpu_image_max_side: u32,
}

/// Lower bound of the experimental GPU image source-side setting.
pub const GPU_IMAGE_MAX_SIDE_MIN: u32 = 1024;
/// Upper bound of the experimental GPU image source-side setting.
pub const GPU_IMAGE_MAX_SIDE_MAX: u32 = 12800;
/// Default longest source-image side for the GPU canvas.
pub const GPU_IMAGE_MAX_SIDE_DEFAULT: u32 = 4096;

fn default_gpu_image_max_side() -> u32 {
    GPU_IMAGE_MAX_SIDE_DEFAULT
}

impl Default for PreviewSettings {
    fn default() -> Self {
        Self {
            auto_scroll_preview: false,
            cursor_width: 1.5,
            cursor_style: CursorStylePref::Line,
            cursor_blink: false,
            cursor_blink_interval: 500.0,
            selection_color: [96, 140, 240],
            selection_opacity: 0.25,
            preview_dpi: 144.0,
            gpu_canvas_preview: false,
            gpu_image_max_side: GPU_IMAGE_MAX_SIDE_DEFAULT,
        }
    }
}

/// Raster export resolution.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    /// Resolution used for PNG export (typst-cli's default is 144 dpi).
    pub png_dpi: f32,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self { png_dpi: 144.0 }
    }
}

/// Project-tree icon style.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct TreeSettings {
    /// Tree folder icons: `false` = outlined, `true` = filled.
    pub folder_icon_filled: bool,
    /// Tree folder icon color as RGBA; `None` = follow the theme accent color.
    pub folder_icon_color: Option<[u8; 4]>,
}

/// Theme and overlay styling.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// Preferred UI theme.
    #[serde(default)]
    pub theme: ThemePref,
    /// Opacity of the dimming backdrop behind the centered settings overlay
    /// (0.0 = no veil, 1.0 = opaque black).
    pub overlay_opacity: f32,
    /// Whole-interface zoom factor (1.0 = 100%). Independent of the UI font
    /// size: this scales controls, icons and layout together.
    pub ui_scale: f32,
    /// Remove gaps and corner radii around the workspace cards.
    pub compact_mode: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemePref::System,
            overlay_opacity: 0.6,
            ui_scale: 1.0,
            compact_mode: false,
        }
    }
}

/// Persisted window geometry: the pane collapse state and widths.
///
/// This is session geometry, not appearance; [`Settings::reset_appearance`]
/// deliberately leaves it untouched.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutSettings {
    /// Panel collapse state (tree, editor, preview).
    pub pane_collapsed: [bool; 3],
    /// Pane widths in points (tree, editor, preview).
    pub pane_widths: [f32; 3],
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            pane_collapsed: [false, false, false],
            pane_widths: [200.0, 560.0, 520.0],
        }
    }
}

/// Workspace: the paste-image directory and the recent-projects MRU list.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSettings {
    /// 粘贴图片保存目录，相对项目根（也可填绝对路径），默认 "image"。
    pub image_dir: String,
    /// Recently opened project roots, most-recent first. Updated only while
    /// [`Self::recent_projects_enabled`] is set; disabling the feature keeps
    /// whatever is already stored.
    #[serde(default)]
    pub recent_projects: Vec<PathBuf>,
    /// Whether adopting a workspace records it in [`Self::recent_projects`].
    /// Defaults to `false`: nothing is recorded until the user opts in.
    #[serde(default)]
    pub recent_projects_enabled: bool,
    /// Maximum number of entries kept in [`Self::recent_projects`], clamped to
    /// [`RECENT_PROJECTS_LIMIT_MIN`]..=[`RECENT_PROJECTS_LIMIT_MAX`] where used.
    #[serde(default = "default_recent_projects_limit")]
    pub recent_projects_limit: usize,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            image_dir: "image".to_owned(),
            recent_projects: Vec::new(),
            recent_projects_enabled: false,
            recent_projects_limit: RECENT_PROJECTS_DEFAULT_LIMIT,
        }
    }
}

/// Crash recovery.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoverySettings {
    /// Keep an autosave snapshot of unsaved tabs for crash recovery. Defaults
    /// to `true`; turning it off deletes the snapshot (unless it is unresolved).
    #[serde(default = "default_crash_recovery_enabled")]
    pub crash_recovery_enabled: bool,
}

impl Default for RecoverySettings {
    fn default() -> Self {
        Self {
            crash_recovery_enabled: true,
        }
    }
}

/// Git integration toggles.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct GitSettings {
    /// 版本管理：启用 Git 集成（状态徽标 / blame / 历史）。默认关闭。
    #[serde(default)]
    pub git_enabled: bool,
    /// 状态栏显示当前行内 blame（需先启用 Git 集成）。默认关闭。
    #[serde(default)]
    pub git_blame_inline: bool,
}

/// LAN collaboration settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CollabSettings {
    /// The user name prefilled into the create/join dialogs (still editable).
    #[serde(default)]
    pub username: String,
    /// The server's starting listen port; the next nine are probed when taken.
    #[serde(default = "default_collab_port")]
    pub port: u16,
    /// Maximum concurrent clients on a server this machine hosts (0 = unlimited).
    /// Further joins are refused with "session is full".
    #[serde(default = "default_collab_max_connections")]
    pub max_connections: usize,
    /// Drop a hosted connection that sends nothing for this many seconds, which
    /// bounds a stalled or malicious peer (0 disables the idle timeout).
    #[serde(default = "default_collab_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

impl Default for CollabSettings {
    fn default() -> Self {
        Self {
            username: String::new(),
            port: default_collab_port(),
            max_connections: default_collab_max_connections(),
            idle_timeout_secs: default_collab_idle_timeout_secs(),
        }
    }
}

/// Serde/default for [`CollabSettings::port`]: the plan's default 39950.
fn default_collab_port() -> u16 {
    COLLAB_DEFAULT_PORT
}

/// Serde/default for [`CollabSettings::max_connections`].
fn default_collab_max_connections() -> usize {
    16
}

/// Serde/default for [`CollabSettings::idle_timeout_secs`].
fn default_collab_idle_timeout_secs() -> u64 {
    30
}

/// Debug-only preview renderers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct DebugSettings {
    /// Debug frame-tree preview renderer (feature toggle + displayed info).
    pub frame_tree: DebugFrameTreeOptions,
}

/// The persisted application settings, nested by domain.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// On-disk schema version; a missing value defaults to the current version.
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    /// Per-command shortcut overrides. Keys are stable command ids
    /// (`"save_as"`); values are display strings parsed by
    /// [`crate::commands::KeyCombo::parse`] (e.g. `"Ctrl+Shift+S"`). A
    /// present key replaces that command's default bindings; omitted commands
    /// keep their defaults.
    #[serde(default)]
    pub shortcuts: BTreeMap<String, Vec<String>>,
    /// Interface language; `System` follows the OS locale.
    #[serde(default)]
    pub language: LanguagePref,
    pub editor: EditorSettings,
    pub fonts: FontSettings,
    pub preview: PreviewSettings,
    pub export: ExportSettings,
    pub tree: TreeSettings,
    pub appearance: AppearanceSettings,
    pub layout: LayoutSettings,
    pub workspace: WorkspaceSettings,
    pub recovery: RecoverySettings,
    pub git: GitSettings,
    pub collab: CollabSettings,
    pub debug: DebugSettings,
    /// Session-only downgrade guard: `false` after loading a file written by a
    /// newer schema version, so [`Settings::save_to_path`] becomes a no-op.
    #[serde(skip)]
    pub(crate) writable: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            shortcuts: BTreeMap::new(),
            language: LanguagePref::System,
            editor: EditorSettings::default(),
            fonts: FontSettings::default(),
            preview: PreviewSettings::default(),
            export: ExportSettings::default(),
            tree: TreeSettings::default(),
            appearance: AppearanceSettings::default(),
            layout: LayoutSettings::default(),
            workspace: WorkspaceSettings::default(),
            recovery: RecoverySettings::default(),
            git: GitSettings::default(),
            collab: CollabSettings::default(),
            debug: DebugSettings::default(),
            writable: true,
        }
    }
}

impl From<LegacySettingsV0> for Settings {
    fn from(legacy: LegacySettingsV0) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            shortcuts: legacy.shortcuts,
            language: legacy.language,
            editor: EditorSettings {
                wrap: legacy.wrap,
                line_spacing: legacy.line_spacing,
                wrap_line_spacing: legacy.wrap_line_spacing,
                completion_max: legacy.completion_max,
                format_on_save: legacy.format_on_save,
                spell_check_enabled: legacy.spell_check_enabled,
            },
            fonts: FontSettings {
                ui_font: legacy.ui_font,
                ui_font_size: legacy.ui_font_size,
                editor_font: legacy.editor_font,
                editor_font_size: legacy.editor_font_size,
            },
            preview: PreviewSettings {
                auto_scroll_preview: legacy.auto_scroll_preview,
                cursor_width: legacy.cursor_width,
                cursor_style: legacy.cursor_style,
                cursor_blink: legacy.cursor_blink,
                cursor_blink_interval: legacy.cursor_blink_interval,
                selection_color: legacy.selection_color,
                selection_opacity: legacy.selection_opacity,
                preview_dpi: legacy.preview_dpi,
                gpu_canvas_preview: legacy.gpu_canvas_preview,
                gpu_image_max_side: legacy.gpu_image_max_side,
            },
            export: ExportSettings {
                png_dpi: legacy.png_dpi,
            },
            tree: TreeSettings {
                folder_icon_filled: legacy.folder_icon_filled,
                folder_icon_color: legacy.folder_icon_color,
            },
            appearance: AppearanceSettings {
                theme: legacy.theme,
                overlay_opacity: legacy.overlay_opacity,
                ui_scale: 1.0,
                compact_mode: legacy.compact_mode,
            },
            layout: LayoutSettings {
                pane_collapsed: legacy.pane_collapsed,
                pane_widths: legacy.pane_widths,
            },
            workspace: WorkspaceSettings {
                image_dir: legacy.image_dir,
                recent_projects: legacy.recent_projects,
                recent_projects_enabled: legacy.recent_projects_enabled,
                recent_projects_limit: legacy.recent_projects_limit,
            },
            recovery: RecoverySettings {
                crash_recovery_enabled: legacy.crash_recovery_enabled,
            },
            git: GitSettings {
                git_enabled: legacy.git_enabled,
                git_blame_inline: legacy.git_blame_inline,
            },
            collab: CollabSettings::default(),
            debug: DebugSettings {
                frame_tree: legacy.debug_frame_tree,
            },
            writable: true,
        }
    }
}

/// The pre-versioning flat settings shape (all 36 fields), deserialized only as
/// the source of the `v0 → v1` migration. Mirroring the old field names is what
/// makes a legacy `config.json` load instead of silently resetting.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct LegacySettingsV0 {
    ui_font: String,
    ui_font_size: f32,
    editor_font: String,
    editor_font_size: f32,
    /// The legacy flat file used `wrap_lines` for the boolean form.
    #[serde(default, alias = "wrap_lines")]
    wrap: WrapPref,
    line_spacing: f32,
    wrap_line_spacing: f32,
    completion_max: usize,
    format_on_save: bool,
    spell_check_enabled: bool,
    auto_scroll_preview: bool,
    cursor_width: f32,
    cursor_style: CursorStylePref,
    cursor_blink: bool,
    cursor_blink_interval: f32,
    selection_color: [u8; 3],
    selection_opacity: f32,
    png_dpi: f32,
    preview_dpi: f32,
    gpu_canvas_preview: bool,
    #[serde(default = "default_gpu_image_max_side")]
    gpu_image_max_side: u32,
    pane_collapsed: [bool; 3],
    pane_widths: [f32; 3],
    folder_icon_filled: bool,
    folder_icon_color: Option<[u8; 4]>,
    #[serde(default)]
    theme: ThemePref,
    overlay_opacity: f32,
    #[serde(default)]
    compact_mode: bool,
    image_dir: String,
    #[serde(default)]
    language: LanguagePref,
    #[serde(default)]
    shortcuts: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    recent_projects: Vec<PathBuf>,
    #[serde(default)]
    recent_projects_enabled: bool,
    #[serde(default = "default_recent_projects_limit")]
    recent_projects_limit: usize,
    #[serde(default = "default_crash_recovery_enabled")]
    crash_recovery_enabled: bool,
    #[serde(default)]
    git_enabled: bool,
    #[serde(default)]
    git_blame_inline: bool,
    #[serde(default)]
    debug_frame_tree: DebugFrameTreeOptions,
}

impl Default for LegacySettingsV0 {
    fn default() -> Self {
        let editor = EditorSettings::default();
        let fonts = FontSettings::default();
        let preview = PreviewSettings::default();
        let export = ExportSettings::default();
        let tree = TreeSettings::default();
        let appearance = AppearanceSettings::default();
        let layout = LayoutSettings::default();
        let workspace = WorkspaceSettings::default();
        let recovery = RecoverySettings::default();
        let git = GitSettings::default();
        let debug = DebugSettings::default();
        Self {
            ui_font: fonts.ui_font,
            ui_font_size: fonts.ui_font_size,
            editor_font: fonts.editor_font,
            editor_font_size: fonts.editor_font_size,
            wrap: editor.wrap,
            line_spacing: editor.line_spacing,
            wrap_line_spacing: editor.wrap_line_spacing,
            completion_max: editor.completion_max,
            format_on_save: editor.format_on_save,
            spell_check_enabled: editor.spell_check_enabled,
            auto_scroll_preview: preview.auto_scroll_preview,
            cursor_width: preview.cursor_width,
            cursor_style: preview.cursor_style,
            cursor_blink: preview.cursor_blink,
            cursor_blink_interval: preview.cursor_blink_interval,
            selection_color: preview.selection_color,
            selection_opacity: preview.selection_opacity,
            png_dpi: export.png_dpi,
            preview_dpi: preview.preview_dpi,
            gpu_canvas_preview: preview.gpu_canvas_preview,
            gpu_image_max_side: preview.gpu_image_max_side,
            pane_collapsed: layout.pane_collapsed,
            pane_widths: layout.pane_widths,
            folder_icon_filled: tree.folder_icon_filled,
            folder_icon_color: tree.folder_icon_color,
            theme: appearance.theme,
            overlay_opacity: appearance.overlay_opacity,
            compact_mode: appearance.compact_mode,
            image_dir: workspace.image_dir,
            language: LanguagePref::System,
            shortcuts: BTreeMap::new(),
            recent_projects: workspace.recent_projects,
            recent_projects_enabled: workspace.recent_projects_enabled,
            recent_projects_limit: workspace.recent_projects_limit,
            crash_recovery_enabled: recovery.crash_recovery_enabled,
            git_enabled: git.git_enabled,
            git_blame_inline: git.git_blame_inline,
            debug_frame_tree: debug.frame_tree,
        }
    }
}

/// Outcome of [`Settings::load`]: the settings plus an optional one-shot notice
/// about how the file was handled (the UI surfaces it as a toast in a later step).
pub struct SettingsLoad {
    pub settings: Settings,
    pub notice: Option<SettingsNotice>,
}

/// A non-fatal problem encountered while loading settings.
#[derive(Debug)]
pub enum SettingsNotice {
    /// The file was not valid settings; it was quarantined (when possible) and
    /// defaults are in force. `quarantined` is the moved-to path, if the rename
    /// succeeded.
    Corrupt {
        quarantined: Option<PathBuf>,
        error: String,
    },
    /// The file was written by a newer schema version. It loaded best-effort and
    /// will not be overwritten this session.
    FutureVersion { found: u32 },
    /// The file exists but could not be read; defaults are in force.
    Unreadable { error: String },
}

/// The v1 nested-domain keys; their presence proves a file is already v1 even
/// without a `schema_version`.
const V1_DOMAIN_KEYS: [&str; 12] = [
    "editor",
    "fonts",
    "preview",
    "export",
    "tree",
    "appearance",
    "layout",
    "workspace",
    "recovery",
    "git",
    "collab",
    "debug",
];

/// Whether `value` is a v1 object: it carries at least one v1 domain block.
fn looks_like_v1(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| V1_DOMAIN_KEYS.iter().any(|key| object.contains_key(*key)))
}

/// A settings-migration or codec failure.
#[derive(Debug)]
enum MigrateError {
    /// No migration is defined for this step's source version.
    Unsupported(u32),
    /// (De)serialization of a migration step failed.
    Codec(String),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(version) => write!(formatter, "unsupported schema version {version}"),
            Self::Codec(message) => write!(formatter, "{message}"),
        }
    }
}

/// The result of parsing a settings text, before any file-level policy applies.
enum Parsed {
    /// A usable set of settings.
    Settings(Settings),
    /// A newer-version file, loaded best-effort (never overwritten).
    FutureVersion(Settings, u32),
    /// Unparseable; `quarantine` says whether the caller should move the file.
    Corrupt { error: String, quarantine: bool },
}

/// Steps a v0..current value up to [`SCHEMA_VERSION`], stamping the version.
fn migrate(mut value: Value, from: u32) -> Result<Value, MigrateError> {
    let mut version = from;
    while version < SCHEMA_VERSION {
        value = match version {
            0 => migrate_v0_to_v1(value)?,
            other => return Err(MigrateError::Unsupported(other)),
        };
        version += 1;
    }
    if let Value::Object(map) = &mut value {
        map.insert("schema_version".to_owned(), Value::from(SCHEMA_VERSION));
    }
    Ok(value)
}

/// Migrates the flat legacy shape into the nested v1 shape.
fn migrate_v0_to_v1(value: Value) -> Result<Value, MigrateError> {
    let legacy: LegacySettingsV0 =
        serde_json::from_value(value).map_err(|err| MigrateError::Codec(err.to_string()))?;
    let settings = Settings::from(legacy);
    serde_json::to_value(settings).map_err(|err| MigrateError::Codec(err.to_string()))
}

/// Parses settings text into [`Parsed`], applying the version/migration rules.
///
/// `from` is the file's declared version, or the detected version when absent:
/// a v1-domain block implies the current version, anything else is legacy v0.
fn parse_text(text: &str) -> Parsed {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            return Parsed::Corrupt {
                error: err.to_string(),
                quarantine: true,
            };
        }
    };
    let declared = value.get("schema_version").and_then(Value::as_u64);
    let from = declared.unwrap_or_else(|| {
        if looks_like_v1(&value) {
            u64::from(SCHEMA_VERSION)
        } else {
            0
        }
    });
    if from > u64::from(SCHEMA_VERSION) {
        return match serde_json::from_value::<Settings>(value) {
            Ok(mut settings) => {
                settings.schema_version = SCHEMA_VERSION;
                settings.writable = false;
                Parsed::FutureVersion(settings, u32::try_from(from).unwrap_or(u32::MAX))
            }
            Err(err) => Parsed::Corrupt {
                error: err.to_string(),
                quarantine: false,
            },
        };
    }
    match migrate(value, from as u32).and_then(|value| {
        serde_json::from_value::<Settings>(value)
            .map_err(|err| MigrateError::Codec(err.to_string()))
    }) {
        Ok(mut settings) => {
            settings.writable = true;
            Parsed::Settings(settings)
        }
        Err(err) => Parsed::Corrupt {
            error: err.to_string(),
            quarantine: true,
        },
    }
}

/// Moves an unreadable settings file aside, returning the new path on success.
fn quarantine_file(path: &Path) -> Option<PathBuf> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let target = path.with_file_name(format!("config.json.bad-{millis}"));
    match fs_ops::rename(path, &target) {
        Ok(()) => Some(target),
        Err(error) => {
            // The notice still reports the corruption, but without this log the
            // failed quarantine (and the file left in place) would be invisible.
            log::warn!(
                "cannot quarantine {} to {}: {error}",
                path.display(),
                target.display()
            );
            None
        }
    }
}

impl Settings {
    /// Resets appearance fields to their defaults.
    ///
    /// Only presentation settings are touched: UI/editor fonts and sizes, line
    /// spacing, wrap continuation spacing, preview cursor (width/style/blink),
    /// selection color and opacity, folder icon style and color, theme, and
    /// overlay opacity.
    ///
    /// Behavior and session state are left unchanged: wrap mode, completion
    /// cap, format-on-save, auto-scroll, PNG/preview DPI, GPU canvas, image
    /// directory, spell check, pane geometry, language, shortcuts, recent
    /// projects, crash recovery, git toggles, LAN collaboration settings, and
    /// the debug frame-tree options.
    #[cfg(test)]
    pub fn reset_appearance(&mut self) {
        let defaults = Self::default();
        self.fonts.ui_font = defaults.fonts.ui_font;
        self.fonts.ui_font_size = defaults.fonts.ui_font_size;
        self.fonts.editor_font = defaults.fonts.editor_font;
        self.fonts.editor_font_size = defaults.fonts.editor_font_size;
        self.editor.line_spacing = defaults.editor.line_spacing;
        self.editor.wrap_line_spacing = defaults.editor.wrap_line_spacing;
        self.preview.cursor_width = defaults.preview.cursor_width;
        self.preview.cursor_style = defaults.preview.cursor_style;
        self.preview.cursor_blink = defaults.preview.cursor_blink;
        self.preview.cursor_blink_interval = defaults.preview.cursor_blink_interval;
        self.preview.selection_color = defaults.preview.selection_color;
        self.preview.selection_opacity = defaults.preview.selection_opacity;
        self.tree.folder_icon_filled = defaults.tree.folder_icon_filled;
        self.tree.folder_icon_color = defaults.tree.folder_icon_color;
        self.appearance.theme = defaults.appearance.theme;
        self.appearance.overlay_opacity = defaults.appearance.overlay_opacity;
        self.appearance.ui_scale = defaults.appearance.ui_scale;
        self.appearance.compact_mode = defaults.appearance.compact_mode;
    }

    fn config_path() -> Option<PathBuf> {
        config_dir().map(|dir| dir.join("config.json"))
    }

    /// Loads the settings from the platform config path.
    pub fn load() -> SettingsLoad {
        match Self::config_path() {
            Some(path) => Self::load_from_path(&path),
            None => SettingsLoad {
                settings: Settings::default(),
                notice: None,
            },
        }
    }

    /// Loads and migrates settings from an explicit path (the testable seam).
    ///
    /// A missing file yields defaults with no notice; an unreadable file yields
    /// defaults plus [`SettingsNotice::Unreadable`]. Invalid JSON or a failed
    /// migration quarantines the file and yields defaults plus
    /// [`SettingsNotice::Corrupt`]. A newer-version file loads best-effort,
    /// preserves the original, and blocks further writes for the session.
    pub(crate) fn load_from_path(path: &Path) -> SettingsLoad {
        let text = match fs_ops::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return SettingsLoad {
                    settings: Settings::default(),
                    notice: None,
                };
            }
            Err(err) => {
                return SettingsLoad {
                    settings: Settings::default(),
                    notice: Some(SettingsNotice::Unreadable {
                        error: err.to_string(),
                    }),
                };
            }
        };
        match parse_text(&text) {
            Parsed::Settings(settings) => SettingsLoad {
                settings,
                notice: None,
            },
            Parsed::FutureVersion(settings, found) => SettingsLoad {
                settings,
                notice: Some(SettingsNotice::FutureVersion { found }),
            },
            Parsed::Corrupt { error, quarantine } => {
                let quarantined = if quarantine {
                    quarantine_file(path)
                } else {
                    None
                };
                SettingsLoad {
                    settings: Settings::default(),
                    notice: Some(SettingsNotice::Corrupt { quarantined, error }),
                }
            }
        }
    }

    /// Persists the settings to the platform config path.
    ///
    /// An unresolvable config directory is a no-op returning `Ok(())`; a write
    /// failure is returned so the caller can surface it (the frame-end persist
    /// logs it and sets its failure latch).
    pub fn save(&self) -> io::Result<()> {
        match Self::config_path() {
            Some(path) => self.save_to_path(&path),
            None => Ok(()),
        }
    }

    /// Whether this build may write the settings file. `false` after loading a
    /// file written by a newer schema version.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Writes the settings to an explicit path (the testable seam).
    ///
    /// A no-op returning `Ok(())` when the settings came from a newer-version
    /// file, so a downgrade never overwrites it.
    pub(crate) fn save_to_path(&self, path: &Path) -> io::Result<()> {
        if !self.writable {
            return Ok(());
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs_ops::write_atomic(path, json.as_bytes())
    }

    /// Parses settings JSON exactly as [`Self::load_from_path`] would (minus the
    /// file policy); the string-based test seam.
    #[cfg(test)]
    pub(crate) fn from_json_str(json: &str) -> Settings {
        match parse_text(json) {
            Parsed::Settings(settings) | Parsed::FutureVersion(settings, _) => settings,
            Parsed::Corrupt { .. } => Settings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn parse(json: &str) -> Settings {
        Settings::from_json_str(json)
    }

    /// Unique, freshly created scratch directory under the system temp dir.
    fn scratch_dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("typ-edit-settings-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn write_scratch(path: &Path, text: &str) {
        std::fs::write(path, text).expect("write scratch file");
    }

    #[test]
    fn legacy_true_wrap_lines_maps_to_always() {
        assert_eq!(
            parse(r#"{"wrap_lines": true}"#).editor.wrap,
            WrapPref::Always
        );
    }

    #[test]
    fn legacy_false_wrap_lines_maps_to_off() {
        assert_eq!(parse(r#"{"wrap_lines": false}"#).editor.wrap, WrapPref::Off);
    }

    #[test]
    fn new_wrap_string_markup_parses() {
        assert_eq!(parse(r#"{"wrap":"markup"}"#).editor.wrap, WrapPref::Markup);
    }

    #[test]
    fn missing_wrap_field_defaults_to_off() {
        assert_eq!(parse("{}").editor.wrap, WrapPref::Off);
    }

    #[test]
    fn wrap_serializes_as_snake_case_and_round_trips() {
        for (mode, name) in [
            (WrapPref::Off, "off"),
            (WrapPref::Always, "always"),
            (WrapPref::Markup, "markup"),
        ] {
            let json = serde_json::to_string(&mode).expect("serialize");
            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(
                serde_json::from_str::<WrapPref>(&json).expect("deserialize"),
                mode
            );
        }
    }

    #[test]
    fn settings_serialize_writes_new_key_only() {
        let mut settings = Settings::default();
        settings.editor.wrap = WrapPref::Markup;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(json.contains(r#""wrap":"markup""#), "new key: {json}");
        assert!(!json.contains("wrap_lines"), "legacy key leaked: {json}");
    }

    #[test]
    fn settings_round_trip_keeps_wrap() {
        let mut settings = Settings::default();
        settings.editor.wrap = WrapPref::Always;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert_eq!(parse(&json).editor.wrap, WrapPref::Always);
    }

    #[test]
    fn reset_appearance_keeps_wrap() {
        let mut settings = Settings::default();
        settings.editor.wrap = WrapPref::Markup;
        settings.reset_appearance();
        assert_eq!(
            settings.editor.wrap,
            WrapPref::Markup,
            "wrap mode is behavior, not appearance"
        );
    }

    #[test]
    fn appearance_defaults_have_unit_line_spacing() {
        let settings = Settings::default();
        assert_eq!(settings.editor.line_spacing, 1.0);
        assert_eq!(settings.editor.wrap_line_spacing, 1.0);
    }

    #[test]
    fn line_spacing_round_trips() {
        let mut settings = Settings::default();
        settings.editor.line_spacing = 1.75;
        settings.editor.wrap_line_spacing = 2.25;
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed = parse(&json);
        assert_eq!(parsed.editor.line_spacing, 1.75);
        assert_eq!(parsed.editor.wrap_line_spacing, 2.25);
    }

    #[test]
    fn missing_line_spacing_fields_default_to_one() {
        let settings = parse(r#"{"editor_font_size":15.0}"#);
        assert_eq!(settings.editor.line_spacing, 1.0);
        assert_eq!(settings.editor.wrap_line_spacing, 1.0);
    }

    #[test]
    fn reset_appearance_restores_unit_line_spacing() {
        let mut settings = Settings::default();
        settings.editor.line_spacing = 2.5;
        settings.editor.wrap_line_spacing = 0.75;
        settings.reset_appearance();
        assert_eq!(settings.editor.line_spacing, 1.0);
        assert_eq!(settings.editor.wrap_line_spacing, 1.0);
    }

    #[test]
    fn completion_max_defaults_to_the_editor_default() {
        assert_eq!(
            Settings::default().editor.completion_max,
            typ_edit_editor::COMPLETION_MAX_DEFAULT
        );
    }

    #[test]
    fn missing_completion_max_defaults_to_the_editor_default() {
        let settings = parse(r#"{"editor_font_size":15.0}"#);
        assert_eq!(
            settings.editor.completion_max,
            typ_edit_editor::COMPLETION_MAX_DEFAULT
        );
    }

    #[test]
    fn completion_max_round_trips() {
        let mut settings = Settings::default();
        settings.editor.completion_max = 200;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert_eq!(parse(&json).editor.completion_max, 200);
    }

    #[test]
    fn reset_appearance_keeps_completion_max() {
        let mut settings = Settings::default();
        settings.editor.completion_max = 500;
        settings.reset_appearance();
        assert_eq!(
            settings.editor.completion_max, 500,
            "completion cap is behavior, not appearance"
        );
    }

    #[test]
    fn to_mode_maps_every_variant() {
        assert_eq!(WrapPref::Off.to_mode(), typ_edit_editor::WrapMode::Off);
        assert_eq!(
            WrapPref::Always.to_mode(),
            typ_edit_editor::WrapMode::Always
        );
        assert_eq!(
            WrapPref::Markup.to_mode(),
            typ_edit_editor::WrapMode::Markup
        );
    }

    #[test]
    fn auto_scroll_preview_defaults_off_and_round_trips() {
        assert!(!Settings::default().preview.auto_scroll_preview);
        assert!(
            !parse(r#"{"format_on_save":false}"#)
                .preview
                .auto_scroll_preview
        );

        let mut settings = Settings::default();
        settings.preview.auto_scroll_preview = true;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(parse(&json).preview.auto_scroll_preview);

        settings.reset_appearance();
        assert!(
            settings.preview.auto_scroll_preview,
            "auto-scroll is behavior, not appearance"
        );
    }

    #[test]
    fn gpu_canvas_preview_defaults_off_and_round_trips() {
        assert!(!Settings::default().preview.gpu_canvas_preview);
        assert!(
            !parse(r#"{"format_on_save":false}"#)
                .preview
                .gpu_canvas_preview
        );

        let mut settings = Settings::default();
        settings.preview.gpu_canvas_preview = true;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(parse(&json).preview.gpu_canvas_preview);

        settings.reset_appearance();
        assert!(
            settings.preview.gpu_canvas_preview,
            "GPU canvas preview is behavior, not appearance"
        );
    }

    #[test]
    fn gpu_image_max_side_defaults_and_round_trips() {
        assert_eq!(
            Settings::default().preview.gpu_image_max_side,
            GPU_IMAGE_MAX_SIDE_DEFAULT
        );
        assert_eq!(
            parse(r#"{"format_on_save":false}"#)
                .preview
                .gpu_image_max_side,
            GPU_IMAGE_MAX_SIDE_DEFAULT
        );

        let mut settings = Settings::default();
        settings.preview.gpu_image_max_side = 8192;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert_eq!(parse(&json).preview.gpu_image_max_side, 8192);

        settings.reset_appearance();
        assert_eq!(settings.preview.gpu_image_max_side, 8192);
    }

    #[test]
    fn cursor_width_defaults_to_one_and_a_half_and_round_trips() {
        assert_eq!(Settings::default().preview.cursor_width, 1.5);
        assert_eq!(
            parse(r#"{"format_on_save":false}"#).preview.cursor_width,
            1.5
        );

        let mut settings = Settings::default();
        settings.preview.cursor_width = 3.25;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert_eq!(parse(&json).preview.cursor_width, 3.25);

        settings.reset_appearance();
        assert_eq!(settings.preview.cursor_width, 1.5);
    }

    #[test]
    fn cursor_style_blink_default_and_round_trip() {
        let defaults = Settings::default();
        assert_eq!(defaults.preview.cursor_style, CursorStylePref::Line);
        assert!(!defaults.preview.cursor_blink);
        assert_eq!(defaults.preview.cursor_blink_interval, 500.0);
        assert_eq!(
            parse(r#"{"format_on_save":false}"#).preview.cursor_style,
            CursorStylePref::Line
        );

        let mut settings = Settings::default();
        settings.preview.cursor_style = CursorStylePref::Mask;
        settings.preview.cursor_blink = true;
        settings.preview.cursor_blink_interval = 750.0;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(json.contains(r#""cursor_style":"mask""#), "json: {json}");
        let parsed = parse(&json);
        assert_eq!(parsed.preview.cursor_style, CursorStylePref::Mask);
        assert!(parsed.preview.cursor_blink);
        assert_eq!(parsed.preview.cursor_blink_interval, 750.0);

        settings.reset_appearance();
        assert_eq!(settings.preview.cursor_style, CursorStylePref::Line);
        assert!(!settings.preview.cursor_blink);
        assert_eq!(settings.preview.cursor_blink_interval, 500.0);
    }

    #[test]
    fn selection_color_opacity_default_and_round_trip() {
        let defaults = Settings::default();
        assert_eq!(defaults.preview.selection_color, [96, 140, 240]);
        assert_eq!(defaults.preview.selection_opacity, 0.25);

        let missing = parse(r#"{"format_on_save":false}"#);
        assert_eq!(missing.preview.selection_color, [96, 140, 240]);
        assert_eq!(missing.preview.selection_opacity, 0.25);

        let mut settings = Settings::default();
        settings.preview.selection_color = [10, 20, 30];
        settings.preview.selection_opacity = 0.6;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(
            json.contains(r#""selection_color":[10,20,30]"#),
            "json: {json}"
        );
        let parsed = parse(&json);
        assert_eq!(parsed.preview.selection_color, [10, 20, 30]);
        assert_eq!(parsed.preview.selection_opacity, 0.6);

        settings.reset_appearance();
        assert_eq!(settings.preview.selection_color, [96, 140, 240]);
        assert_eq!(settings.preview.selection_opacity, 0.25);
    }

    #[test]
    fn recent_projects_default_off_and_round_trip() {
        let defaults = Settings::default();
        assert!(defaults.workspace.recent_projects.is_empty());
        assert!(!defaults.workspace.recent_projects_enabled);
        assert_eq!(defaults.workspace.recent_projects_limit, 10);

        let mut settings = Settings::default();
        settings.workspace.recent_projects = vec![PathBuf::from("C:/projects/alpha")];
        settings.workspace.recent_projects_enabled = true;
        settings.workspace.recent_projects_limit = 25;
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed = parse(&json);
        assert_eq!(
            parsed.workspace.recent_projects,
            vec![PathBuf::from("C:/projects/alpha")]
        );
        assert!(parsed.workspace.recent_projects_enabled);
        assert_eq!(parsed.workspace.recent_projects_limit, 25);
    }

    #[test]
    fn missing_recent_fields_keep_defaults() {
        let settings = parse(r#"{"editor_font_size":15.0}"#);
        assert!(settings.workspace.recent_projects.is_empty());
        assert!(!settings.workspace.recent_projects_enabled);
        assert_eq!(settings.workspace.recent_projects_limit, 10);
    }

    #[test]
    fn reset_appearance_keeps_recent_projects() {
        let mut settings = Settings::default();
        settings.workspace.recent_projects = vec![PathBuf::from("C:/projects/alpha")];
        settings.workspace.recent_projects_enabled = true;
        settings.workspace.recent_projects_limit = 20;
        settings.reset_appearance();
        assert_eq!(
            settings.workspace.recent_projects,
            vec![PathBuf::from("C:/projects/alpha")]
        );
        assert!(settings.workspace.recent_projects_enabled);
        assert_eq!(settings.workspace.recent_projects_limit, 20);
    }

    #[test]
    fn clamp_recent_limit_bounds_to_one_and_thirty() {
        assert_eq!(clamp_recent_limit(0), 1);
        assert_eq!(clamp_recent_limit(1), 1);
        assert_eq!(clamp_recent_limit(10), 10);
        assert_eq!(clamp_recent_limit(30), 30);
        assert_eq!(clamp_recent_limit(100), 30);
    }

    #[test]
    fn crash_recovery_defaults_on() {
        assert!(Settings::default().recovery.crash_recovery_enabled);
        assert!(
            parse(r#"{"editor_font_size":15.0}"#)
                .recovery
                .crash_recovery_enabled
        );
    }

    #[test]
    fn crash_recovery_round_trips() {
        let mut settings = Settings::default();
        settings.recovery.crash_recovery_enabled = false;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(!parse(&json).recovery.crash_recovery_enabled);
    }

    #[test]
    fn reset_appearance_keeps_crash_recovery() {
        let mut settings = Settings::default();
        settings.recovery.crash_recovery_enabled = false;
        settings.reset_appearance();
        assert!(!settings.recovery.crash_recovery_enabled);
    }

    #[test]
    fn spell_check_defaults_off_and_round_trips() {
        assert!(!Settings::default().editor.spell_check_enabled);
        assert!(
            !parse(r#"{"format_on_save":false}"#)
                .editor
                .spell_check_enabled
        );

        let mut settings = Settings::default();
        settings.editor.spell_check_enabled = true;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(parse(&json).editor.spell_check_enabled);
    }

    #[test]
    fn reset_appearance_keeps_spell_check() {
        let mut settings = Settings::default();
        settings.editor.spell_check_enabled = true;
        settings.reset_appearance();
        assert!(
            settings.editor.spell_check_enabled,
            "spell check is a feature toggle, not an appearance default"
        );
    }

    #[test]
    fn git_defaults_off_and_round_trips() {
        let defaults = Settings::default();
        assert!(!defaults.git.git_enabled);
        assert!(!defaults.git.git_blame_inline);

        let missing = parse(r#"{"format_on_save":false}"#);
        assert!(!missing.git.git_enabled);
        assert!(!missing.git.git_blame_inline);

        let mut settings = Settings::default();
        settings.git.git_enabled = true;
        settings.git.git_blame_inline = true;
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed = parse(&json);
        assert!(parsed.git.git_enabled);
        assert!(parsed.git.git_blame_inline);
    }

    #[test]
    fn reset_appearance_keeps_git_toggles() {
        let mut settings = Settings::default();
        settings.git.git_enabled = true;
        settings.git.git_blame_inline = true;
        settings.reset_appearance();
        assert!(settings.git.git_enabled, "git is a feature toggle");
        assert!(
            settings.git.git_blame_inline,
            "inline blame is a feature toggle"
        );
    }

    #[test]
    fn debug_frame_tree_defaults_disabled_with_all_display_on() {
        let options = Settings::default().debug.frame_tree;
        assert!(!options.enabled, "the debug renderer is opt-in");
        assert!(options.show_children);
        assert!(options.show_text);
        assert!(options.show_geometry);
        assert!(options.show_style);
        assert!(options.show_span);
        assert!(options.show_group_nodes);
        assert!(options.show_text_nodes);
        assert!(options.show_shape_nodes);
        assert!(options.show_image_nodes);
        assert!(options.show_link_nodes);
        assert!(options.show_tag_nodes);
        assert_eq!(options.highlight_color, [80, 200, 120, 110]);

        let missing = parse(r#"{"format_on_save":false}"#);
        assert!(!missing.debug.frame_tree.enabled);
        assert!(missing.debug.frame_tree.show_style);
        assert_eq!(
            missing.debug.frame_tree.highlight_color,
            [80, 200, 120, 110]
        );
    }

    #[test]
    fn debug_frame_tree_round_trips() {
        let mut settings = Settings::default();
        settings.debug.frame_tree.enabled = true;
        settings.debug.frame_tree.show_text = false;
        settings.debug.frame_tree.show_shape_nodes = false;
        settings.debug.frame_tree.highlight_color = [1, 2, 3, 4];
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed = parse(&json);
        assert!(parsed.debug.frame_tree.enabled);
        assert!(!parsed.debug.frame_tree.show_text);
        assert!(!parsed.debug.frame_tree.show_shape_nodes);
        assert!(parsed.debug.frame_tree.show_children);
        assert!(parsed.debug.frame_tree.show_geometry);
        assert_eq!(parsed.debug.frame_tree.highlight_color, [1, 2, 3, 4]);
    }

    #[test]
    fn debug_frame_tree_partial_json_uses_defaults() {
        let options: DebugFrameTreeOptions =
            serde_json::from_str(r#"{"enabled":true}"#).expect("parse");
        assert!(options.enabled);
        assert!(options.show_children);
        assert!(options.show_text);
        assert!(options.show_geometry);
        assert!(options.show_style);
        assert!(options.show_span);
        assert!(options.show_group_nodes);
        assert!(options.show_text_nodes);
        assert!(options.show_shape_nodes);
        assert!(options.show_image_nodes);
        assert!(options.show_link_nodes);
        assert!(options.show_tag_nodes);
        assert_eq!(options.highlight_color, [80, 200, 120, 110]);

        let settings = parse(r#"{"debug_frame_tree":{"show_style":false}}"#);
        assert!(!settings.debug.frame_tree.enabled);
        assert!(!settings.debug.frame_tree.show_style);
        assert!(settings.debug.frame_tree.show_text);
        assert_eq!(
            settings.debug.frame_tree.highlight_color,
            [80, 200, 120, 110]
        );
    }

    #[test]
    fn reset_appearance_keeps_debug_frame_tree() {
        let mut settings = Settings::default();
        settings.debug.frame_tree.enabled = true;
        settings.debug.frame_tree.show_text = false;
        settings.reset_appearance();
        assert!(
            settings.debug.frame_tree.enabled,
            "the debug renderer is a feature toggle"
        );
        assert!(!settings.debug.frame_tree.show_text);
    }

    #[test]
    fn reset_appearance_keeps_collab() {
        let mut settings = Settings::default();
        settings.collab.username = "ann".to_owned();
        settings.collab.port = 40123;
        settings.reset_appearance();
        assert_eq!(settings.collab.username, "ann");
        assert_eq!(settings.collab.port, 40123);
    }

    #[test]
    fn collab_defaults_are_empty_user_and_default_port() {
        let collab = CollabSettings::default();
        assert!(collab.username.is_empty());
        assert_eq!(collab.port, COLLAB_DEFAULT_PORT);
    }

    #[test]
    fn collab_partial_json_fills_defaults() {
        let settings = parse(r#"{"collab":{"username":"ann"}}"#);
        assert_eq!(settings.collab.username, "ann");
        assert_eq!(settings.collab.port, COLLAB_DEFAULT_PORT);

        let empty = parse(r#"{"collab":{}}"#);
        assert!(empty.collab.username.is_empty());
        assert_eq!(empty.collab.port, COLLAB_DEFAULT_PORT);
    }

    #[test]
    fn collab_round_trips() {
        let mut settings = Settings::default();
        settings.collab.username = "bob".to_owned();
        settings.collab.port = 40000;
        let json = serde_json::to_string(&settings).expect("serialize");
        let back = parse(&json);
        assert_eq!(back.collab.username, "bob");
        assert_eq!(back.collab.port, 40000);
    }

    #[test]
    fn reset_appearance_restores_every_appearance_field() {
        let mut settings = Settings::default();
        settings.fonts.ui_font = "Non Default UI".to_owned();
        settings.fonts.ui_font_size = 21.0;
        settings.fonts.editor_font = "Non Default Mono".to_owned();
        settings.fonts.editor_font_size = 22.0;
        settings.editor.line_spacing = 2.5;
        settings.editor.wrap_line_spacing = 3.5;
        settings.preview.cursor_width = 4.0;
        settings.preview.cursor_style = CursorStylePref::Mask;
        settings.preview.cursor_blink = true;
        settings.preview.cursor_blink_interval = 900.0;
        settings.preview.selection_color = [1, 2, 3];
        settings.preview.selection_opacity = 0.9;
        settings.tree.folder_icon_filled = true;
        settings.tree.folder_icon_color = Some([1, 2, 3, 4]);
        settings.appearance.theme = ThemePref::Dark;
        settings.appearance.overlay_opacity = 0.9;
        settings.appearance.compact_mode = true;

        settings.reset_appearance();

        let defaults = Settings::default();
        assert_eq!(settings.fonts.ui_font, defaults.fonts.ui_font);
        assert_eq!(settings.fonts.ui_font_size, defaults.fonts.ui_font_size);
        assert_eq!(settings.fonts.editor_font, defaults.fonts.editor_font);
        assert_eq!(
            settings.fonts.editor_font_size,
            defaults.fonts.editor_font_size
        );
        assert_eq!(settings.editor.line_spacing, defaults.editor.line_spacing);
        assert_eq!(
            settings.editor.wrap_line_spacing,
            defaults.editor.wrap_line_spacing
        );
        assert_eq!(settings.preview.cursor_width, defaults.preview.cursor_width);
        assert_eq!(settings.preview.cursor_style, defaults.preview.cursor_style);
        assert_eq!(settings.preview.cursor_blink, defaults.preview.cursor_blink);
        assert_eq!(
            settings.preview.cursor_blink_interval,
            defaults.preview.cursor_blink_interval
        );
        assert_eq!(
            settings.preview.selection_color,
            defaults.preview.selection_color
        );
        assert_eq!(
            settings.preview.selection_opacity,
            defaults.preview.selection_opacity
        );
        assert_eq!(
            settings.tree.folder_icon_filled,
            defaults.tree.folder_icon_filled
        );
        assert_eq!(
            settings.tree.folder_icon_color,
            defaults.tree.folder_icon_color
        );
        assert_eq!(settings.appearance.theme, defaults.appearance.theme);
        assert_eq!(
            settings.appearance.overlay_opacity,
            defaults.appearance.overlay_opacity
        );
        assert_eq!(
            settings.appearance.compact_mode,
            defaults.appearance.compact_mode
        );
    }

    #[test]
    fn compact_mode_defaults_off_and_round_trips() {
        assert!(!Settings::default().appearance.compact_mode);
        let mut settings = Settings::default();
        settings.appearance.compact_mode = true;
        let json = serde_json::to_string(&settings).expect("serialize");
        assert!(parse(&json).appearance.compact_mode);
    }

    #[test]
    fn reset_appearance_keeps_every_behavior_field() {
        let mut settings = Settings::default();
        settings.editor.wrap = WrapPref::Markup;
        settings.editor.completion_max = 500;
        settings.editor.format_on_save = false;
        settings.preview.auto_scroll_preview = true;
        settings.export.png_dpi = 300.0;
        settings.preview.preview_dpi = 288.0;
        settings.preview.gpu_canvas_preview = true;
        settings.preview.gpu_image_max_side = 8192;
        settings.workspace.image_dir = "assets".to_owned();
        settings.editor.spell_check_enabled = true;
        settings.layout.pane_collapsed = [true, false, true];
        settings.layout.pane_widths = [111.0, 222.0, 333.0];
        settings.language = LanguagePref::EnUs;
        settings
            .shortcuts
            .insert("save_as".to_owned(), vec!["Ctrl+Shift+S".to_owned()]);
        settings.workspace.recent_projects = vec![PathBuf::from("C:/projects/alpha")];
        settings.workspace.recent_projects_enabled = true;
        settings.workspace.recent_projects_limit = 20;
        settings.recovery.crash_recovery_enabled = false;
        settings.git.git_enabled = true;
        settings.git.git_blame_inline = true;
        settings.debug.frame_tree.enabled = true;
        settings.debug.frame_tree.show_text = false;
        settings.debug.frame_tree.highlight_color = [7, 8, 9, 10];

        settings.reset_appearance();

        assert_eq!(settings.editor.wrap, WrapPref::Markup);
        assert_eq!(settings.editor.completion_max, 500);
        assert!(!settings.editor.format_on_save);
        assert!(settings.preview.auto_scroll_preview);
        assert_eq!(settings.export.png_dpi, 300.0);
        assert_eq!(settings.preview.preview_dpi, 288.0);
        assert!(settings.preview.gpu_canvas_preview);
        assert_eq!(settings.preview.gpu_image_max_side, 8192);
        assert_eq!(settings.workspace.image_dir, "assets");
        assert!(settings.editor.spell_check_enabled);
        assert_eq!(settings.layout.pane_collapsed, [true, false, true]);
        assert_eq!(settings.layout.pane_widths, [111.0, 222.0, 333.0]);
        assert_eq!(settings.language, LanguagePref::EnUs);
        assert_eq!(
            settings.shortcuts.get("save_as"),
            Some(&vec!["Ctrl+Shift+S".to_owned()])
        );
        assert_eq!(
            settings.workspace.recent_projects,
            vec![PathBuf::from("C:/projects/alpha")]
        );
        assert!(settings.workspace.recent_projects_enabled);
        assert_eq!(settings.workspace.recent_projects_limit, 20);
        assert!(!settings.recovery.crash_recovery_enabled);
        assert!(settings.git.git_enabled);
        assert!(settings.git.git_blame_inline);
        assert!(settings.debug.frame_tree.enabled);
        assert!(!settings.debug.frame_tree.show_text);
        assert_eq!(settings.debug.frame_tree.highlight_color, [7, 8, 9, 10]);
    }

    /// The historical legacy flat default document. `ui_font` and
    /// `editor_font` stay at their old values because stored settings are
    /// preserved; platform defaults only affect fresh settings (and missing
    /// fonts fall back at install time).
    const LEGACY_DEFAULT_FLAT_JSON: &str = r#"{
        "ui_font": "Noto Sans SC",
        "ui_font_size": 14.0,
        "editor_font": "JetBrains Maple Mono",
        "editor_font_size": 14.0,
        "wrap": "off",
        "line_spacing": 1.0,
        "wrap_line_spacing": 1.0,
        "completion_max": 50,
        "format_on_save": true,
        "spell_check_enabled": false,
        "auto_scroll_preview": false,
        "cursor_width": 1.5,
        "cursor_style": "line",
        "cursor_blink": false,
        "cursor_blink_interval": 500.0,
        "selection_color": [96, 140, 240],
        "selection_opacity": 0.25,
        "png_dpi": 144.0,
        "preview_dpi": 144.0,
        "gpu_canvas_preview": false,
        "pane_collapsed": [false, false, false],
        "pane_widths": [200.0, 560.0, 520.0],
        "folder_icon_filled": false,
        "folder_icon_color": null,
        "theme": "system",
        "overlay_opacity": 0.6,
        "image_dir": "image",
        "language": "system",
        "shortcuts": {},
        "recent_projects": [],
        "recent_projects_enabled": false,
        "recent_projects_limit": 10,
        "crash_recovery_enabled": true,
        "git_enabled": false,
        "git_blame_inline": false,
        "debug_frame_tree": {
            "enabled": false,
            "show_children": true,
            "show_text": true,
            "show_geometry": true,
            "show_style": true,
            "show_span": true
        }
    }"#;

    #[test]
    fn migrate_v0_flat_json_lands_in_substructures() {
        let settings = parse(
            r#"{
                "editor_font": "Mono X",
                "ui_font_size": 17.0,
                "wrap": "markup",
                "line_spacing": 1.75,
                "completion_max": 120,
                "format_on_save": false,
                "spell_check_enabled": true,
                "auto_scroll_preview": true,
                "cursor_width": 3.0,
                "cursor_style": "mask",
                "cursor_blink": true,
                "cursor_blink_interval": 800.0,
                "selection_color": [1, 2, 3],
                "selection_opacity": 0.5,
                "png_dpi": 300.0,
                "preview_dpi": 288.0,
                "gpu_canvas_preview": true,
                "pane_collapsed": [true, false, true],
                "pane_widths": [10.0, 20.0, 30.0],
                "folder_icon_filled": true,
                "folder_icon_color": [9, 8, 7, 6],
                "theme": "dark",
                "overlay_opacity": 0.9,
                "image_dir": "assets",
                "recent_projects": ["C:/projects/alpha"],
                "recent_projects_enabled": true,
                "recent_projects_limit": 25,
                "crash_recovery_enabled": false,
                "git_enabled": true,
                "git_blame_inline": true,
                "debug_frame_tree": {"enabled": true, "show_text": false}
            }"#,
        );

        assert_eq!(settings.schema_version, 1);
        assert_eq!(settings.fonts.editor_font, "Mono X");
        assert_eq!(
            settings.fonts.ui_font,
            crate::ui_fonts::default_ui_font_name()
        );
        assert_eq!(settings.fonts.ui_font_size, 17.0);
        assert_eq!(settings.editor.wrap, WrapPref::Markup);
        assert_eq!(settings.editor.line_spacing, 1.75);
        assert_eq!(settings.editor.completion_max, 120);
        assert!(!settings.editor.format_on_save);
        assert!(settings.editor.spell_check_enabled);
        assert!(settings.preview.auto_scroll_preview);
        assert_eq!(settings.preview.cursor_width, 3.0);
        assert_eq!(settings.preview.cursor_style, CursorStylePref::Mask);
        assert!(settings.preview.cursor_blink);
        assert_eq!(settings.preview.cursor_blink_interval, 800.0);
        assert_eq!(settings.preview.selection_color, [1, 2, 3]);
        assert_eq!(settings.preview.selection_opacity, 0.5);
        assert_eq!(settings.preview.preview_dpi, 288.0);
        assert!(settings.preview.gpu_canvas_preview);
        assert_eq!(settings.export.png_dpi, 300.0);
        assert!(settings.tree.folder_icon_filled);
        assert_eq!(settings.tree.folder_icon_color, Some([9, 8, 7, 6]));
        assert_eq!(settings.appearance.theme, ThemePref::Dark);
        assert_eq!(settings.appearance.overlay_opacity, 0.9);
        assert_eq!(settings.layout.pane_collapsed, [true, false, true]);
        assert_eq!(settings.layout.pane_widths, [10.0, 20.0, 30.0]);
        assert_eq!(settings.workspace.image_dir, "assets");
        assert_eq!(
            settings.workspace.recent_projects,
            vec![PathBuf::from("C:/projects/alpha")]
        );
        assert!(settings.workspace.recent_projects_enabled);
        assert_eq!(settings.workspace.recent_projects_limit, 25);
        assert!(!settings.recovery.crash_recovery_enabled);
        assert!(settings.git.git_enabled);
        assert!(settings.git.git_blame_inline);
        assert!(settings.debug.frame_tree.enabled);
        assert!(!settings.debug.frame_tree.show_text);
    }

    #[test]
    fn legacy_wrap_lines_bool_still_maps() {
        let dir = scratch_dir();
        let path = dir.join("config.json");

        write_scratch(&path, r#"{"wrap_lines": true}"#);
        assert_eq!(
            Settings::load_from_path(&path).settings.editor.wrap,
            WrapPref::Always
        );

        write_scratch(&path, r#"{"wrap_lines": false}"#);
        assert_eq!(
            Settings::load_from_path(&path).settings.editor.wrap,
            WrapPref::Off
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_json_is_quarantined_and_defaults() {
        let dir = scratch_dir();
        let path = dir.join("config.json");
        write_scratch(&path, "{not json");

        let load = Settings::load_from_path(&path);
        assert_eq!(
            serde_json::to_value(&load.settings).expect("serialize"),
            serde_json::to_value(Settings::default()).expect("serialize"),
            "corrupt settings fall back to defaults"
        );
        match load.notice {
            Some(SettingsNotice::Corrupt { quarantined, .. }) => {
                let quarantined = quarantined.expect("quarantine rename succeeds");
                assert_eq!(
                    std::fs::read(&quarantined).expect("read quarantined file"),
                    b"{not json"
                );
            }
            other => panic!("expected a Corrupt notice, got {other:?}"),
        }
        assert!(!path.exists(), "the original file was moved aside");
        let bad = std::fs::read_dir(&dir)
            .expect("read scratch dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("config.json.bad-")
            })
            .count();
        assert_eq!(bad, 1, "exactly one quarantined file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_round_trip_stamps_schema_version() {
        let dir = scratch_dir();
        let path = dir.join("config.json");

        let mut settings = Settings::default();
        settings.editor.wrap = WrapPref::Markup;
        settings.save_to_path(&path).expect("save");

        let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read"))
            .expect("written settings parse");
        assert_eq!(written.get("schema_version"), Some(&Value::from(1)));

        let load = Settings::load_from_path(&path);
        assert_eq!(load.settings.schema_version, 1);
        assert_eq!(load.settings.editor.wrap, WrapPref::Markup);
        assert!(load.notice.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_font_settings_use_platform_fonts() {
        let fonts = FontSettings::default();
        assert_eq!(fonts.ui_font, crate::ui_fonts::default_ui_font_name());
        assert_eq!(
            fonts.editor_font,
            crate::ui_fonts::default_editor_font_name()
        );
    }

    #[test]
    fn missing_subfields_default() {
        let settings = parse(r#"{"schema_version":1,"editor":{"wrap":"markup"}}"#);
        assert_eq!(settings.editor.wrap, WrapPref::Markup);
        assert_eq!(settings.editor.line_spacing, 1.0);
        assert_eq!(
            settings.editor.completion_max,
            typ_edit_editor::COMPLETION_MAX_DEFAULT
        );
        assert_eq!(settings.fonts.ui_font, FontSettings::default().ui_font);
        assert_eq!(
            settings.fonts.editor_font,
            FontSettings::default().editor_font
        );
        assert_eq!(
            settings.preview.cursor_width,
            PreviewSettings::default().cursor_width
        );
        assert_eq!(settings.appearance.overlay_opacity, 0.6);
        assert_eq!(
            settings.layout.pane_widths,
            LayoutSettings::default().pane_widths
        );
        assert!(settings.recovery.crash_recovery_enabled);
    }

    #[test]
    fn absent_version_nested_json_is_not_treated_as_legacy() {
        let settings = parse(r#"{"editor":{"wrap":"markup"}}"#);
        assert_eq!(
            settings.editor.wrap,
            WrapPref::Markup,
            "a nested editor block without schema_version is v1, not legacy"
        );
        assert_eq!(settings.schema_version, 1);
    }

    #[test]
    fn legacy_flat_json_is_not_silently_parsed_as_v1() {
        let settings = parse(r#"{"ui_font":"X"}"#);
        assert_eq!(
            settings.fonts.ui_font, "X",
            "a flat legacy field must be migrated, not dropped"
        );
    }

    #[test]
    fn future_version_loads_best_effort_and_preserves_file() {
        let dir = scratch_dir();
        let path = dir.join("config.json");
        let original = r#"{"schema_version":999,"editor":{"wrap":"markup","line_spacing":2.5}}"#;
        write_scratch(&path, original);

        let load = Settings::load_from_path(&path);
        assert_eq!(load.settings.editor.wrap, WrapPref::Markup);
        assert_eq!(load.settings.editor.line_spacing, 2.5);
        assert!(!load.settings.writable, "downgrade writes are blocked");
        assert!(!load.settings.is_writable(), "the public guard agrees");
        match load.notice {
            Some(SettingsNotice::FutureVersion { found }) => assert_eq!(found, 999),
            other => panic!("expected a FutureVersion notice, got {other:?}"),
        }

        load.settings.save_to_path(&path).expect("no-op save");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            original,
            "a future-version file is never overwritten"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bom_is_tolerated_by_load_from_path() {
        let dir = scratch_dir();
        let path = dir.join("config.json");
        write_scratch(
            &path,
            "\u{feff}{\"schema_version\":1,\"editor\":{\"wrap\":\"markup\"}}",
        );

        let load = Settings::load_from_path(&path);
        assert_eq!(load.settings.editor.wrap, WrapPref::Markup);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_default_json_preserves_stored_fonts() {
        let migrated = parse(LEGACY_DEFAULT_FLAT_JSON);
        let mut expected = Settings::default();
        // Existing configs keep their stored families; only fresh defaults are
        // platform-specific. Missing families are handled by the installer's
        // system fallback.
        expected.fonts.ui_font = "Noto Sans SC".to_owned();
        expected.fonts.editor_font = "JetBrains Maple Mono".to_owned();
        assert_eq!(
            serde_json::to_value(&migrated).expect("serialize migrated"),
            serde_json::to_value(&expected).expect("serialize default"),
            "legacy defaults keep the stored fonts and match all other defaults"
        );
    }
}
