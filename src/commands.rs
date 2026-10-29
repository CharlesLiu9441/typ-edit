//! The command registry: every invokable action, its metadata, and its
//! default key bindings.
//!
//! [`COMMANDS`] is the single source of truth for the menu labels, the shortcut
//! display, and the runtime [`crate::keybindings::KeyBindings`] table. A
//! [`CommandId`] resolves to its entry by id ([`CommandId::meta`]), so the
//! enum's declaration order and the table's order are independent.

use crate::app::dispatch::{
    CommandState, collab_can_disconnect, collab_can_join, collab_can_share, collab_can_stop,
    collab_can_users, editor_can_redo, editor_can_undo, editor_editable,
};
use std::sync::OnceLock;

/// One invokable action, roughly in menu order; not tied to [`COMMANDS`] indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum CommandId {
    // File menu.
    OpenFolder,
    OpenFile,
    NewProject,
    Save,
    SaveAs,
    Recompile,
    Exit,
    // Edit menu.
    Cut,
    Copy,
    Paste,
    SelectAll,
    Undo,
    Redo,
    Find,
    FormatDocument,
    // Tabs (no menu entry; dispatch-only actions).
    CloseTab,
    PreviousTab,
    NextTab,
    // View menu.
    ToggleTree,
    ToggleEditor,
    TogglePreview,
    FoldAtCaret,
    UnfoldAtCaret,
    FoldAll,
    UnfoldAll,
    ToggleBookmark,
    NextBookmark,
    PreviousBookmark,
    ClearBookmarks,
    // Preview actions.
    ResetView,
    ZoomIn,
    ZoomOut,
    // Application.
    OpenSettings,
    ShowShortcuts,
    ShowCommandPalette,
    // Tools menu.
    OpenTemplateGallery,
    // Search navigation (no menu entry; dispatch-only actions).
    NextSearchResult,
    PreviousSearchResult,
    // Find references (no menu entry; dispatch-only action).
    FindReferences,
    // Git actions (no menu entry; palette-only).
    GitRefresh,
    GitFileHistory,
    GitCopyHash,
    // Collaboration (menu-driven; no default key bindings).
    CollabShare,
    CollabJoin,
    CollabUsers,
    CollabStopServer,
    CollabDisconnect,
}

impl CommandId {
    /// This command's entry in [`COMMANDS`], found by `id` rather than by the
    /// enum's declaration order.
    pub(crate) fn meta(self) -> &'static CommandMeta {
        COMMANDS
            .iter()
            .find(|meta| meta.id == self)
            .expect("every CommandId has a COMMANDS entry")
    }

    /// The stable settings key for this command (snake_case), stored on the
    /// command's [`COMMANDS`] entry so the string exists in exactly one place.
    pub(crate) fn as_str(self) -> &'static str {
        self.meta().id_str
    }

    /// Parses a stable settings key back into a command by scanning [`COMMANDS`].
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        COMMANDS
            .iter()
            .find(|meta| meta.id_str == value)
            .map(|meta| meta.id)
    }
}

/// The platform's primary shortcut modifier symbol (`Ctrl` or `Cmd`).
pub(crate) fn command_symbol() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// Rewrites a stored shortcut label for display on this platform.
///
/// Settings and reference rows keep the canonical `Ctrl+` spelling so a config
/// moves between platforms unchanged; macOS displays the same chord as `Cmd+`.
pub(crate) fn localize_shortcut(text: &str) -> String {
    localize_shortcut_for(text, cfg!(target_os = "macos"))
}

/// Testable form of [`localize_shortcut`].
fn localize_shortcut_for(text: &str, macos: bool) -> String {
    if macos && text.contains("Ctrl+") {
        text.replace("Ctrl+", "Cmd+")
    } else {
        text.to_owned()
    }
}

/// One keyboard combination: the modifiers plus one logical key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct KeyCombo {
    /// The platform command modifier: `Ctrl` on Windows/Linux, `Cmd` on macOS.
    pub(crate) ctrl: bool,
    pub(crate) shift: bool,
    pub(crate) alt: bool,
    pub(crate) key: egui::Key,
}

impl KeyCombo {
    /// `Ctrl/Cmd+<key>` (the platform command key: Ctrl on Windows/Linux, Cmd
    /// on macOS).
    pub(crate) const fn ctrl(key: egui::Key) -> Self {
        Self {
            ctrl: true,
            shift: false,
            alt: false,
            key,
        }
    }

    /// `Ctrl/Cmd+Shift+<key>`.
    pub(crate) const fn ctrl_shift(key: egui::Key) -> Self {
        Self {
            ctrl: true,
            shift: true,
            alt: false,
            key,
        }
    }

    /// The egui modifier set this combo matches, preserving the legacy mapping
    /// (`Ctrl`/`Cmd` => [`egui::Modifiers::COMMAND`]).
    pub(crate) fn to_modifiers(self) -> egui::Modifiers {
        let mut mods = egui::Modifiers::NONE;
        if self.ctrl {
            mods |= egui::Modifiers::COMMAND;
        }
        if self.shift {
            mods |= egui::Modifiers::SHIFT;
        }
        if self.alt {
            mods |= egui::Modifiers::ALT;
        }
        mods
    }

    /// Renders the platform's spelling (`"Ctrl+Shift+S"` on Windows/Linux,
    /// `"Cmd+Shift+S"` on macOS) using egui's canonical key name, with the
    /// bracket keys shown as the symbols users actually press. The long
    /// `OpenBracket` / `CloseBracket` names overflow menu rows and mean nothing
    /// to a reader; `[` and `]` stay unambiguous.
    pub(crate) fn label(self) -> String {
        self.label_with(command_symbol())
    }

    /// Renders with an explicit command-modifier symbol, so both platform
    /// spellings are unit-testable from one host.
    fn label_with(self, command: &str) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str(command);
            out.push('+');
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        out.push_str(key_label(self.key));
        out
    }

    /// Parses the format produced by [`Self::label`]: `"Ctrl+Shift+S"`,
    /// `"Cmd+Shift+S"`, `"ctrl + shift + s"` (modifiers are case-insensitive,
    /// whitespace is ignored). `Cmd`, `Command`, `Super`, and `Meta` are
    /// accepted as aliases for the platform command modifier, so a config
    /// written on macOS parses on Windows and vice versa. An unknown key is
    /// rejected.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut key: Option<egui::Key> = None;
        for part in value.split('+') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" | "super" | "meta" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                _ => {
                    if key.is_some() {
                        return None;
                    }
                    key = Some(match part {
                        "[" => egui::Key::OpenBracket,
                        "]" => egui::Key::CloseBracket,
                        _ => egui::Key::from_name(part)?,
                    });
                }
            }
        }
        key.map(|key| Self {
            ctrl,
            shift,
            alt,
            key,
        })
    }
}

/// Display name of one logical key. Only the two bracket variants differ from
/// egui's canonical debug name.
fn key_label(key: egui::Key) -> &'static str {
    match key {
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        _ => key.name(),
    }
}

/// Where a command appears in the shortcuts overview: the display row, its
/// order within a row that merges several commands, and the group/description
/// i18n keys. `group: None` marks a dispatch-only row that is not rendered.
pub(crate) struct ShortcutDisplay {
    pub(crate) row: u16,
    pub(crate) order: u8,
    pub(crate) group: Option<&'static str>,
    pub(crate) description: &'static str,
}

/// One command's registry entry.
pub(crate) struct CommandMeta {
    pub(crate) id: CommandId,
    /// The stable settings key (snake_case); the single source of truth behind
    /// [`CommandId::as_str`] and [`CommandId::from_str`].
    pub(crate) id_str: &'static str,
    /// i18n key of the command's display label.
    pub(crate) label: &'static str,
    /// i18n key of the palette/group heading; `None` falls back to
    /// `command.category.general` (e.g. Recompile).
    pub(crate) category: Option<&'static str>,
    /// Position in the keyboard-dispatch order; a permutation of `0..len` read
    /// by [`dispatch_order`].
    pub(crate) dispatch_rank: u16,
    /// Default bindings, overridable through `Settings.shortcuts`.
    pub(crate) default_bindings: &'static [KeyCombo],
    /// Display-only label for the menu when a command has no runtime binding
    /// (editor-local keys such as Ctrl+X, which the editor consumes itself).
    pub(crate) static_keys: Option<&'static str>,
    /// Menu enablement predicate; `None` means always enabled.
    pub(crate) enabled: Option<fn(&CommandState<'_>) -> bool>,
    /// Membership in the shortcuts overview; `None` when not listed there.
    pub(crate) shortcut: Option<ShortcutDisplay>,
}

/// Every command, in menu order.
pub(crate) const COMMANDS: &[CommandMeta] = &[
    CommandMeta {
        id: CommandId::OpenFolder,
        id_str: "open_folder",
        dispatch_rank: 24,
        shortcut: None,
        label: "menu.file.open_folder",
        category: Some("command.category.file"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::OpenFile,
        id_str: "open_file",
        dispatch_rank: 25,
        shortcut: None,
        label: "menu.file.open_file",
        category: Some("command.category.file"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::NewProject,
        id_str: "new_project",
        dispatch_rank: 26,
        shortcut: None,
        label: "menu.file.new_project",
        category: Some("command.category.file"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::Save,
        id_str: "save",
        dispatch_rank: 2,
        shortcut: Some(ShortcutDisplay {
            row: 0,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.save",
        }),
        label: "menu.file.save",
        category: Some("command.category.file"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::S)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::SaveAs,
        id_str: "save_as",
        dispatch_rank: 1,
        shortcut: Some(ShortcutDisplay {
            row: 1,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.save_as",
        }),
        label: "menu.file.save_as",
        category: Some("command.category.file"),
        default_bindings: &[KeyCombo::ctrl_shift(egui::Key::S)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::Recompile,
        id_str: "recompile",
        dispatch_rank: 5,
        shortcut: Some(ShortcutDisplay {
            row: 23,
            order: 0,
            group: None,
            description: "shortcut.recompile",
        }),
        label: "menu.file.recompile",
        category: None,
        default_bindings: &[KeyCombo::ctrl(egui::Key::R)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::Exit,
        id_str: "exit",
        dispatch_rank: 27,
        shortcut: None,
        label: "menu.file.exit",
        category: Some("command.category.file"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::Cut,
        id_str: "cut",
        dispatch_rank: 28,
        shortcut: None,
        label: "menu.edit.cut",
        category: Some("command.category.edit"),
        default_bindings: &[],
        static_keys: Some("Ctrl+X"),
        enabled: Some(editor_editable),
    },
    CommandMeta {
        id: CommandId::Copy,
        id_str: "copy",
        dispatch_rank: 29,
        shortcut: None,
        label: "menu.edit.copy",
        category: Some("command.category.edit"),
        default_bindings: &[],
        static_keys: Some("Ctrl+C"),
        enabled: Some(editor_editable),
    },
    CommandMeta {
        id: CommandId::Paste,
        id_str: "paste",
        dispatch_rank: 30,
        shortcut: None,
        label: "menu.edit.paste",
        category: Some("command.category.edit"),
        default_bindings: &[],
        static_keys: Some("Ctrl+V"),
        enabled: Some(editor_editable),
    },
    CommandMeta {
        id: CommandId::SelectAll,
        id_str: "select_all",
        dispatch_rank: 31,
        shortcut: None,
        label: "menu.edit.select_all",
        category: Some("command.category.edit"),
        default_bindings: &[],
        static_keys: Some("Ctrl+A"),
        enabled: Some(editor_editable),
    },
    CommandMeta {
        id: CommandId::Undo,
        id_str: "undo",
        dispatch_rank: 32,
        shortcut: None,
        label: "menu.edit.undo",
        category: Some("command.category.edit"),
        default_bindings: &[],
        static_keys: None,
        enabled: Some(editor_can_undo),
    },
    CommandMeta {
        id: CommandId::Redo,
        id_str: "redo",
        dispatch_rank: 33,
        shortcut: None,
        label: "menu.edit.redo",
        category: Some("command.category.edit"),
        default_bindings: &[],
        static_keys: None,
        enabled: Some(editor_can_redo),
    },
    CommandMeta {
        id: CommandId::Find,
        id_str: "find",
        dispatch_rank: 4,
        shortcut: Some(ShortcutDisplay {
            row: 3,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.find",
        }),
        label: "menu.edit.find",
        category: Some("command.category.edit"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::F)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::FormatDocument,
        id_str: "format_document",
        dispatch_rank: 3,
        shortcut: Some(ShortcutDisplay {
            row: 2,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.format",
        }),
        label: "menu.edit.format",
        category: Some("command.category.edit"),
        default_bindings: &[KeyCombo::ctrl_shift(egui::Key::F)],
        static_keys: None,
        enabled: Some(editor_editable),
    },
    CommandMeta {
        id: CommandId::CloseTab,
        id_str: "close_tab",
        dispatch_rank: 6,
        shortcut: Some(ShortcutDisplay {
            row: 4,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.close_tab",
        }),
        label: "shortcut.close_tab",
        category: Some("command.category.tab"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::W)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::PreviousTab,
        id_str: "previous_tab",
        dispatch_rank: 7,
        shortcut: Some(ShortcutDisplay {
            row: 5,
            order: 1,
            group: Some("shortcut.group.general"),
            description: "shortcut.cycle_tab",
        }),
        label: "command.previous_tab",
        category: Some("command.category.tab"),
        default_bindings: &[KeyCombo::ctrl_shift(egui::Key::Tab)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::NextTab,
        id_str: "next_tab",
        dispatch_rank: 8,
        shortcut: Some(ShortcutDisplay {
            row: 5,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.cycle_tab",
        }),
        label: "command.next_tab",
        category: Some("command.category.tab"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::Tab)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ToggleTree,
        id_str: "toggle_tree",
        dispatch_rank: 12,
        shortcut: Some(ShortcutDisplay {
            row: 6,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.toggle_pane",
        }),
        label: "menu.view.tree",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::Num1)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ToggleEditor,
        id_str: "toggle_editor",
        dispatch_rank: 13,
        shortcut: Some(ShortcutDisplay {
            row: 6,
            order: 1,
            group: Some("shortcut.group.general"),
            description: "shortcut.toggle_pane",
        }),
        label: "menu.view.editor",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::Num2)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::TogglePreview,
        id_str: "toggle_preview",
        dispatch_rank: 14,
        shortcut: Some(ShortcutDisplay {
            row: 6,
            order: 2,
            group: Some("shortcut.group.general"),
            description: "shortcut.toggle_pane",
        }),
        label: "menu.view.preview",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::Num3)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::FoldAtCaret,
        id_str: "fold_at_caret",
        dispatch_rank: 17,
        shortcut: None,
        label: "menu.view.fold",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: true,
            shift: true,
            alt: false,
            key: egui::Key::OpenBracket,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::UnfoldAtCaret,
        id_str: "unfold_at_caret",
        dispatch_rank: 18,
        shortcut: None,
        label: "menu.view.unfold",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: true,
            shift: true,
            alt: false,
            key: egui::Key::CloseBracket,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::FoldAll,
        id_str: "fold_all",
        dispatch_rank: 15,
        shortcut: None,
        label: "menu.view.fold_all",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: true,
            shift: true,
            alt: true,
            key: egui::Key::OpenBracket,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::UnfoldAll,
        id_str: "unfold_all",
        dispatch_rank: 16,
        shortcut: None,
        label: "menu.view.unfold_all",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: true,
            shift: true,
            alt: true,
            key: egui::Key::CloseBracket,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ToggleBookmark,
        id_str: "toggle_bookmark",
        dispatch_rank: 19,
        shortcut: None,
        label: "menu.view.toggle_bookmark",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::F2)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::NextBookmark,
        id_str: "next_bookmark",
        dispatch_rank: 20,
        shortcut: None,
        label: "menu.view.next_bookmark",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: false,
            shift: false,
            alt: true,
            key: egui::Key::ArrowDown,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::PreviousBookmark,
        id_str: "previous_bookmark",
        dispatch_rank: 21,
        shortcut: None,
        label: "menu.view.previous_bookmark",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: false,
            shift: false,
            alt: true,
            key: egui::Key::ArrowUp,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ClearBookmarks,
        id_str: "clear_bookmarks",
        dispatch_rank: 22,
        shortcut: None,
        label: "menu.view.clear_bookmarks",
        category: Some("command.category.view"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ResetView,
        id_str: "reset_view",
        dispatch_rank: 9,
        shortcut: Some(ShortcutDisplay {
            row: 9,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.reset_view",
        }),
        label: "shortcut.reset_view",
        category: Some("command.category.preview"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::Num0)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ZoomIn,
        id_str: "zoom_in",
        dispatch_rank: 10,
        shortcut: Some(ShortcutDisplay {
            row: 20,
            order: 0,
            group: Some("shortcut.group.preview"),
            description: "shortcut.zoom",
        }),
        label: "command.zoom_in",
        category: Some("command.category.preview"),
        default_bindings: &[
            KeyCombo::ctrl(egui::Key::Equals),
            KeyCombo::ctrl(egui::Key::Plus),
        ],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ZoomOut,
        id_str: "zoom_out",
        dispatch_rank: 11,
        shortcut: Some(ShortcutDisplay {
            row: 20,
            order: 1,
            group: Some("shortcut.group.preview"),
            description: "shortcut.zoom",
        }),
        label: "command.zoom_out",
        category: Some("command.category.preview"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::Minus)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::OpenSettings,
        id_str: "open_settings",
        dispatch_rank: 23,
        shortcut: Some(ShortcutDisplay {
            row: 7,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "shortcut.open_settings",
        }),
        label: "menu.settings",
        category: Some("command.category.app"),
        default_bindings: &[KeyCombo::ctrl(egui::Key::Comma)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ShowShortcuts,
        id_str: "show_shortcuts",
        dispatch_rank: 34,
        shortcut: Some(ShortcutDisplay {
            row: 10,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "menu.help.shortcuts",
        }),
        label: "menu.help.shortcuts",
        category: Some("command.category.app"),
        default_bindings: &[KeyCombo {
            ctrl: false,
            shift: false,
            alt: false,
            key: egui::Key::F1,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::ShowCommandPalette,
        id_str: "show_command_palette",
        dispatch_rank: 0,
        shortcut: Some(ShortcutDisplay {
            row: 8,
            order: 0,
            group: Some("shortcut.group.general"),
            description: "menu.help.command_palette",
        }),
        label: "menu.help.command_palette",
        category: Some("command.category.general"),
        default_bindings: &[KeyCombo::ctrl_shift(egui::Key::P)],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::OpenTemplateGallery,
        id_str: "open_template_gallery",
        dispatch_rank: 35,
        shortcut: None,
        label: "menu.tools.templates",
        category: Some("command.category.tools"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::NextSearchResult,
        id_str: "next_search_result",
        dispatch_rank: 37,
        shortcut: None,
        label: "command.search_next",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: false,
            shift: false,
            alt: false,
            key: egui::Key::F4,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::PreviousSearchResult,
        id_str: "previous_search_result",
        dispatch_rank: 36,
        shortcut: None,
        label: "command.search_previous",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: false,
            shift: true,
            alt: false,
            key: egui::Key::F4,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::FindReferences,
        id_str: "find_references",
        dispatch_rank: 38,
        shortcut: None,
        label: "command.find_references",
        category: Some("command.category.view"),
        default_bindings: &[KeyCombo {
            ctrl: false,
            shift: true,
            alt: false,
            key: egui::Key::F12,
        }],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::GitRefresh,
        id_str: "git_refresh",
        dispatch_rank: 39,
        shortcut: None,
        label: "git.command.refresh",
        category: Some("command.category.git"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::GitFileHistory,
        id_str: "git_file_history",
        dispatch_rank: 40,
        shortcut: None,
        label: "git.command.file_history",
        category: Some("command.category.git"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::GitCopyHash,
        id_str: "git_copy_hash",
        dispatch_rank: 41,
        shortcut: None,
        label: "git.command.copy_hash",
        category: Some("command.category.git"),
        default_bindings: &[],
        static_keys: None,
        enabled: None,
    },
    CommandMeta {
        id: CommandId::CollabShare,
        id_str: "collab_share",
        dispatch_rank: 42,
        shortcut: None,
        label: "collab.command.share",
        category: Some("command.category.collab"),
        default_bindings: &[],
        static_keys: None,
        enabled: Some(collab_can_share),
    },
    CommandMeta {
        id: CommandId::CollabJoin,
        id_str: "collab_join",
        dispatch_rank: 43,
        shortcut: None,
        label: "collab.command.join",
        category: Some("command.category.collab"),
        default_bindings: &[],
        static_keys: None,
        enabled: Some(collab_can_join),
    },
    CommandMeta {
        id: CommandId::CollabUsers,
        id_str: "collab_users",
        dispatch_rank: 44,
        shortcut: None,
        label: "collab.command.users",
        category: Some("command.category.collab"),
        default_bindings: &[],
        static_keys: None,
        enabled: Some(collab_can_users),
    },
    CommandMeta {
        id: CommandId::CollabStopServer,
        id_str: "collab_stop_server",
        dispatch_rank: 45,
        shortcut: None,
        label: "collab.command.stop_server",
        category: Some("command.category.collab"),
        default_bindings: &[],
        static_keys: None,
        enabled: Some(collab_can_stop),
    },
    CommandMeta {
        id: CommandId::CollabDisconnect,
        id_str: "collab_disconnect",
        dispatch_rank: 46,
        shortcut: None,
        label: "collab.command.disconnect",
        category: Some("command.category.collab"),
        default_bindings: &[],
        static_keys: None,
        enabled: Some(collab_can_disconnect),
    },
];

/// The order in which [`handle_shortcuts`](super::menu) tries to consume a
/// keyboard event; every [`COMMANDS`] command appears exactly once, derived by
/// sorting the entries on [`CommandMeta::dispatch_rank`] (a permutation of
/// `0..COMMANDS.len()`).
///
/// [`CommandId::ShowCommandPalette`] stays first (its legacy position); the
/// legacy pre-registry order follows. Shift-qualified combos must precede their
/// unshifted counterparts because `consume_key` ignores an extra Shift, so
/// `Ctrl+Shift+S`/`Ctrl+Shift+Tab` (SaveAs/PreviousTab) would otherwise fire
/// Save/NextTab.
pub(crate) fn dispatch_order() -> &'static [CommandId] {
    static ORDER: OnceLock<Vec<CommandId>> = OnceLock::new();
    ORDER.get_or_init(|| {
        let mut ranked: Vec<(u16, CommandId)> = COMMANDS
            .iter()
            .map(|meta| (meta.dispatch_rank, meta.id))
            .collect();
        ranked.sort_by_key(|(rank, _)| *rank);
        for pair in ranked.windows(2) {
            assert_ne!(pair[0].0, pair[1].0, "duplicate dispatch_rank");
        }
        ranked.into_iter().map(|(_, id)| id).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_i18n::t;

    #[test]
    fn key_combo_label_parse_round_trips() {
        let combos = [
            KeyCombo::ctrl(egui::Key::S),
            KeyCombo::ctrl_shift(egui::Key::S),
            KeyCombo::ctrl(egui::Key::Num0),
            KeyCombo::ctrl(egui::Key::Comma),
            KeyCombo::ctrl(egui::Key::Equals),
            KeyCombo {
                ctrl: false,
                shift: true,
                alt: true,
                key: egui::Key::F,
            },
            KeyCombo::ctrl_shift(egui::Key::OpenBracket),
            KeyCombo {
                ctrl: true,
                shift: true,
                alt: true,
                key: egui::Key::CloseBracket,
            },
        ];
        for combo in combos {
            assert_eq!(
                KeyCombo::parse(&combo.label()),
                Some(combo),
                "round trip of {}",
                combo.label()
            );
        }
    }

    #[test]
    fn bracket_keys_use_symbol_labels() {
        let command = command_symbol();
        assert_eq!(
            KeyCombo::ctrl_shift(egui::Key::OpenBracket).label(),
            format!("{command}+Shift+[")
        );
        assert_eq!(
            KeyCombo {
                ctrl: true,
                shift: true,
                alt: true,
                key: egui::Key::CloseBracket,
            }
            .label(),
            format!("{command}+Shift+Alt+]")
        );
    }

    #[test]
    fn parse_is_case_insensitive_on_modifiers_and_ignores_whitespace() {
        assert_eq!(
            KeyCombo::parse("ctrl + shift + s"),
            Some(KeyCombo::ctrl_shift(egui::Key::S))
        );
    }

    #[test]
    fn parse_rejects_unknown_keys() {
        assert_eq!(KeyCombo::parse("Ctrl+NotAKey"), None);
        assert_eq!(KeyCombo::parse("Ctrl+"), None);
    }

    #[test]
    fn labels_use_the_requested_command_symbol() {
        assert_eq!(
            KeyCombo::ctrl_shift(egui::Key::S).label_with("Cmd"),
            "Cmd+Shift+S"
        );
        assert_eq!(KeyCombo::ctrl(egui::Key::S).label_with("Ctrl"), "Ctrl+S");
    }

    #[test]
    fn command_aliases_parse_as_the_command_modifier() {
        for text in ["Cmd+S", "Command+S", "Super+S", "Meta+S"] {
            assert_eq!(
                KeyCombo::parse(text),
                Some(KeyCombo::ctrl(egui::Key::S)),
                "{text}"
            );
        }
    }

    #[test]
    fn shortcut_text_localization_swaps_ctrl_for_cmd_only_on_macos() {
        assert_eq!(localize_shortcut_for("Ctrl+Shift+P", true), "Cmd+Shift+P");
        assert_eq!(localize_shortcut_for("Ctrl+Click", true), "Cmd+Click");
        assert_eq!(localize_shortcut_for("Ctrl+Shift+P", false), "Ctrl+Shift+P");
        assert_eq!(localize_shortcut_for("Shift+Tab", true), "Shift+Tab");
    }

    #[test]
    fn command_id_str_round_trips() {
        for meta in COMMANDS {
            assert_eq!(CommandId::from_str(meta.id.as_str()), Some(meta.id));
        }
    }

    #[test]
    fn every_command_id_resolves_to_its_own_meta() {
        for meta in COMMANDS {
            assert_eq!(meta.id.meta().id, meta.id, "{:?}", meta.id);
            assert_eq!(
                CommandId::from_str(meta.id.as_str()),
                Some(meta.id),
                "{:?}",
                meta.id
            );
        }
    }

    #[test]
    fn every_command_label_and_category_resolves() {
        for meta in COMMANDS {
            assert_ne!(t(meta.label), meta.label, "missing label for {:?}", meta.id);
            if let Some(category) = meta.category {
                assert_ne!(
                    t(category),
                    category,
                    "missing category {category:?} for {:?}",
                    meta.id
                );
            }
        }
    }

    #[test]
    fn dispatch_order_covers_every_command() {
        let order = dispatch_order();
        assert_eq!(order.len(), COMMANDS.len());
        let mut dispatched = order.to_vec();
        dispatched.sort();
        dispatched.dedup();
        assert_eq!(
            dispatched.len(),
            COMMANDS.len(),
            "duplicate command in dispatch order"
        );
        let mut registered: Vec<CommandId> = COMMANDS.iter().map(|meta| meta.id).collect();
        registered.sort();
        assert_eq!(dispatched, registered);
    }

    #[test]
    fn shift_qualified_commands_dispatch_first() {
        let index = |id: CommandId| {
            dispatch_order()
                .iter()
                .position(|candidate| *candidate == id)
                .unwrap_or_else(|| panic!("{id:?} missing from dispatch order"))
        };
        assert!(index(CommandId::SaveAs) < index(CommandId::Save));
        assert!(index(CommandId::PreviousTab) < index(CommandId::NextTab));
    }

    #[test]
    fn command_id_strings_are_unique() {
        let mut ids: Vec<&str> = COMMANDS.iter().map(|meta| meta.id_str).collect();
        ids.sort_unstable();
        let total = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate command id string");
    }

    #[test]
    fn dispatch_rank_is_a_permutation() {
        let mut ranks: Vec<u16> = COMMANDS.iter().map(|meta| meta.dispatch_rank).collect();
        ranks.sort_unstable();
        let expected: Vec<u16> = (0..COMMANDS.len() as u16).collect();
        assert_eq!(ranks, expected, "dispatch_rank must cover 0..len uniquely");
    }

    #[test]
    fn dispatch_order_matches_the_legacy_sequence() {
        use CommandId::*;
        let expected = [
            ShowCommandPalette,
            SaveAs,
            Save,
            FormatDocument,
            Find,
            Recompile,
            CloseTab,
            PreviousTab,
            NextTab,
            ResetView,
            ZoomIn,
            ZoomOut,
            ToggleTree,
            ToggleEditor,
            TogglePreview,
            FoldAll,
            UnfoldAll,
            FoldAtCaret,
            UnfoldAtCaret,
            ToggleBookmark,
            NextBookmark,
            PreviousBookmark,
            ClearBookmarks,
            OpenSettings,
            OpenFolder,
            OpenFile,
            NewProject,
            Exit,
            Cut,
            Copy,
            Paste,
            SelectAll,
            Undo,
            Redo,
            ShowShortcuts,
            OpenTemplateGallery,
            PreviousSearchResult,
            NextSearchResult,
            FindReferences,
            GitRefresh,
            GitFileHistory,
            GitCopyHash,
            CollabShare,
            CollabJoin,
            CollabUsers,
            CollabStopServer,
            CollabDisconnect,
        ];
        assert_eq!(dispatch_order(), expected.as_slice());
    }
}
