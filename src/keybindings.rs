//! The runtime keybinding table: the registry's default bindings with the
//! user's [`Settings::shortcuts`] overrides applied.
//!
//! A command whose id appears in the settings has its bindings *replaced* by
//! the parsed override; an override that parses to nothing is ignored and the
//! defaults are kept. Unknown command ids are ignored.

use crate::commands::{COMMANDS, CommandId, KeyCombo};
use crate::settings::Settings;
use std::collections::BTreeMap;

/// Command id → ordered list of key combinations.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeyBindings {
    map: BTreeMap<CommandId, Vec<KeyCombo>>,
}

impl KeyBindings {
    /// Builds the table from the defaults plus the settings overrides.
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        let mut map: BTreeMap<CommandId, Vec<KeyCombo>> = COMMANDS
            .iter()
            .map(|meta| (meta.id, meta.default_bindings.to_vec()))
            .collect();

        for (id_key, combo_keys) in &settings.shortcuts {
            let Some(id) = CommandId::from_str(id_key) else {
                log::warn!("ignoring unknown command id in shortcuts: {id_key}");
                continue;
            };
            let mut combos = Vec::with_capacity(combo_keys.len());
            for combo_key in combo_keys {
                match KeyCombo::parse(combo_key) {
                    Some(combo) => combos.push(combo),
                    None => {
                        log::warn!(
                            "ignoring invalid shortcut {combo_key:?} for {}",
                            id.as_str()
                        );
                    }
                }
            }
            if combos.is_empty() && !combo_keys.is_empty() {
                log::warn!(
                    "all shortcuts for {} are invalid; keeping defaults",
                    id.as_str()
                );
                continue;
            }
            // A present (and valid) key replaces the command's bindings; an
            // explicitly empty list clears them.
            map.insert(id, combos);
        }

        Self { map }
    }

    /// Consumes this command's binding, if one matched. Tries every combo.
    ///
    /// Combos that are a native passthrough (see [`is_native_passthrough`]) are
    /// dropped before matching, so the native widget keeps its own clipboard
    /// behavior and the shortcut is not hijacked.
    pub(crate) fn consume(&self, ui: &mut egui::Ui, id: CommandId) -> bool {
        let Some(combos) = self.map.get(&id) else {
            return false;
        };
        let combos = consumable_combos(id, combos);
        if combos.is_empty() {
            return false;
        }
        let mut consumed = false;
        for combo in &combos {
            consumed |= ui.input_mut(|input| input.consume_key(combo.to_modifiers(), combo.key));
        }
        if consumed {
            return true;
        }
        // egui-winit converts a Ctrl+C/X/V press into `Copy`/`Cut`/`Paste` and
        // swallows the key press, so such overrides can only match the converted
        // form (or the unconverted release) rather than `consume_key`.
        if !combos.iter().any(|combo| {
            combo.ctrl && matches!(combo.key, egui::Key::C | egui::Key::X | egui::Key::V)
        }) {
            return false;
        }
        let (events, current) = ui.input(|input| (input.events.clone(), input.modifiers));
        let Some(index) = combos
            .iter()
            .find_map(|combo| shortcut_event_index(&events, combo, current))
        else {
            return false;
        };
        ui.input_mut(|input| {
            if index < input.events.len() {
                input.events.remove(index);
            }
        });
        true
    }

    /// The display label of a command's bindings, joined with `" / "`.
    /// `None` when the command has no (remaining) binding.
    pub(crate) fn label(&self, id: CommandId) -> Option<String> {
        let combos = self.map.get(&id)?;
        if combos.is_empty() {
            return None;
        }
        Some(
            combos
                .iter()
                .map(|combo| combo.label())
                .collect::<Vec<_>>()
                .join(" / "),
        )
    }

    /// Every combo bound to more than one command, for the settings page.
    pub(crate) fn conflicts(&self) -> Vec<(KeyCombo, Vec<CommandId>)> {
        let mut seen: BTreeMap<KeyCombo, Vec<CommandId>> = BTreeMap::new();
        for meta in COMMANDS {
            let Some(combos) = self.map.get(&meta.id) else {
                continue;
            };
            for combo in combos {
                let ids = seen.entry(*combo).or_default();
                if !ids.contains(&meta.id) {
                    ids.push(meta.id);
                }
            }
        }
        seen.into_iter().filter(|(_, ids)| ids.len() > 1).collect()
    }
}

/// Whether `combo` on `id` is the platform-native clipboard action.
///
/// Such combos must reach the focused native widget untouched: egui-winit turns
/// command+C/X/V into [`egui::Event::Copy`]/`Cut`/`Paste`, so consuming them for
/// our own command both steals the widget's clipboard behavior and, for Paste,
/// loops forever (consume → `RequestPaste` → eframe re-emits `Event::Paste` →
/// consume) until the modifier is released and the events finally reach the
/// editor. Command+C/X/V convert regardless of Shift/Alt, so those variants are
/// indistinguishable from native and pass through too; Ctrl+A is a plain key
/// event and only the exact combo is native.
fn is_native_passthrough(id: CommandId, combo: &KeyCombo) -> bool {
    match id {
        CommandId::Copy => combo.ctrl && combo.key == egui::Key::C,
        CommandId::Cut => combo.ctrl && combo.key == egui::Key::X,
        CommandId::Paste => combo.ctrl && combo.key == egui::Key::V,
        CommandId::SelectAll => {
            combo.ctrl && !combo.shift && !combo.alt && combo.key == egui::Key::A
        }
        _ => false,
    }
}

/// The combos of `id` that the app may consume, i.e. all but the native
/// passthrough ones (other commands may still bind Ctrl+C/X/V/A as a remap).
fn consumable_combos(id: CommandId, combos: &[KeyCombo]) -> Vec<KeyCombo> {
    combos
        .iter()
        .copied()
        .filter(|combo| !is_native_passthrough(id, combo))
        .collect()
}

/// Index of the first event matching the converted form of `combo`.
///
/// egui-winit turns Ctrl+C/X/V presses into [`egui::Event::Copy`] /
/// [`egui::Event::Cut`] / [`egui::Event::Paste`]. Releases are not converted, so
/// a released V gives the only exact match when the clipboard is empty.
fn shortcut_event_index(
    events: &[egui::Event],
    combo: &KeyCombo,
    current: egui::Modifiers,
) -> Option<usize> {
    let mods_match = |modifiers: egui::Modifiers| {
        combo.ctrl == modifiers.command
            && combo.shift == modifiers.shift
            && combo.alt == modifiers.alt
    };
    events.iter().position(|event| match event {
        egui::Event::Copy => combo.ctrl && combo.key == egui::Key::C && mods_match(current),
        egui::Event::Cut => combo.ctrl && combo.key == egui::Key::X && mods_match(current),
        egui::Event::Paste(_) => combo.ctrl && combo.key == egui::Key::V && mods_match(current),
        egui::Event::Key {
            key,
            pressed: false,
            repeat: false,
            modifiers,
            ..
        } => combo.key == egui::Key::V && *key == combo.key && mods_match(*modifiers),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandId, KeyCombo};
    use crate::settings::Settings;
    use egui::Key;

    fn settings_with(overrides: &[(&str, &[&str])]) -> Settings {
        Settings {
            shortcuts: overrides
                .iter()
                .map(|(id, combos)| {
                    (
                        (*id).to_owned(),
                        combos.iter().map(|combo| (*combo).to_owned()).collect(),
                    )
                })
                .collect(),
            ..Settings::default()
        }
    }

    #[test]
    fn default_bindings_match_the_legacy_table() {
        let bindings = KeyBindings::from_settings(&Settings::default());
        use CommandId::*;
        let expected: &[(CommandId, &[KeyCombo])] = &[
            (Save, &[KeyCombo::ctrl(Key::S)]),
            (SaveAs, &[KeyCombo::ctrl_shift(Key::S)]),
            (FormatDocument, &[KeyCombo::ctrl_shift(Key::F)]),
            (Find, &[KeyCombo::ctrl(Key::F)]),
            (Recompile, &[KeyCombo::ctrl(Key::R)]),
            (CloseTab, &[KeyCombo::ctrl(Key::W)]),
            (PreviousTab, &[KeyCombo::ctrl_shift(Key::Tab)]),
            (NextTab, &[KeyCombo::ctrl(Key::Tab)]),
            (ResetView, &[KeyCombo::ctrl(Key::Num0)]),
            (
                ZoomIn,
                &[KeyCombo::ctrl(Key::Equals), KeyCombo::ctrl(Key::Plus)],
            ),
            (ZoomOut, &[KeyCombo::ctrl(Key::Minus)]),
            (ToggleTree, &[KeyCombo::ctrl(Key::Num1)]),
            (ToggleEditor, &[KeyCombo::ctrl(Key::Num2)]),
            (TogglePreview, &[KeyCombo::ctrl(Key::Num3)]),
            (
                FoldAtCaret,
                &[KeyCombo {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    key: Key::OpenBracket,
                }],
            ),
            (
                UnfoldAtCaret,
                &[KeyCombo {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    key: Key::CloseBracket,
                }],
            ),
            (
                FoldAll,
                &[KeyCombo {
                    ctrl: true,
                    shift: true,
                    alt: true,
                    key: Key::OpenBracket,
                }],
            ),
            (
                UnfoldAll,
                &[KeyCombo {
                    ctrl: true,
                    shift: true,
                    alt: true,
                    key: Key::CloseBracket,
                }],
            ),
            (ToggleBookmark, &[KeyCombo::ctrl(Key::F2)]),
            (
                NextBookmark,
                &[KeyCombo {
                    ctrl: false,
                    shift: false,
                    alt: true,
                    key: Key::ArrowDown,
                }],
            ),
            (
                PreviousBookmark,
                &[KeyCombo {
                    ctrl: false,
                    shift: false,
                    alt: true,
                    key: Key::ArrowUp,
                }],
            ),
            (OpenSettings, &[KeyCombo::ctrl(Key::Comma)]),
            (
                ShowShortcuts,
                &[KeyCombo {
                    ctrl: false,
                    shift: false,
                    alt: false,
                    key: Key::F1,
                }],
            ),
            (ShowCommandPalette, &[KeyCombo::ctrl_shift(Key::P)]),
            (
                NextSearchResult,
                &[KeyCombo {
                    ctrl: false,
                    shift: false,
                    alt: false,
                    key: Key::F4,
                }],
            ),
            (
                PreviousSearchResult,
                &[KeyCombo {
                    ctrl: false,
                    shift: true,
                    alt: false,
                    key: Key::F4,
                }],
            ),
            (
                FindReferences,
                &[KeyCombo {
                    ctrl: false,
                    shift: true,
                    alt: false,
                    key: Key::F12,
                }],
            ),
        ];
        for (id, combos) in expected {
            assert_eq!(
                bindings.map.get(id).map(Vec::as_slice),
                Some(*combos),
                "{id:?}"
            );
        }
        for id in [
            OpenFolder,
            OpenFile,
            Exit,
            Cut,
            Copy,
            Paste,
            SelectAll,
            Undo,
            Redo,
            ClearBookmarks,
            OpenTemplateGallery,
        ] {
            assert!(
                bindings.map.get(&id).is_some_and(Vec::is_empty),
                "{id:?} should have no default binding"
            );
        }
        assert_eq!(bindings.map.len(), COMMANDS.len());
    }

    #[test]
    fn settings_override_replaces_defaults() {
        let settings = settings_with(&[("save", &["Ctrl+Shift+K"])]);
        let bindings = KeyBindings::from_settings(&settings);
        assert_eq!(
            bindings.map.get(&CommandId::Save).map(Vec::as_slice),
            Some(&[KeyCombo::ctrl_shift(Key::K)][..])
        );
        // The old default is gone.
        assert!(
            !bindings
                .map
                .get(&CommandId::Save)
                .is_some_and(|combos| combos.contains(&KeyCombo::ctrl(Key::S)))
        );
    }

    #[test]
    fn empty_override_clears_the_binding() {
        let settings = settings_with(&[("save", &[])]);
        let bindings = KeyBindings::from_settings(&settings);
        assert!(
            bindings
                .map
                .get(&CommandId::Save)
                .is_some_and(Vec::is_empty)
        );
        assert_eq!(bindings.label(CommandId::Save), None);
    }

    #[test]
    fn invalid_override_keeps_the_defaults() {
        let settings = settings_with(&[("save", &["Ctrl+NotAKey"])]);
        let bindings = KeyBindings::from_settings(&settings);
        assert_eq!(
            bindings.map.get(&CommandId::Save).map(Vec::as_slice),
            Some(&[KeyCombo::ctrl(Key::S)][..])
        );
    }

    #[test]
    fn unknown_command_id_is_ignored() {
        let settings = settings_with(&[("no_such_command", &["Ctrl+Q"])]);
        let bindings = KeyBindings::from_settings(&settings);
        assert_eq!(bindings.map.len(), COMMANDS.len());
        assert!(CommandId::from_str("no_such_command").is_none());
    }

    #[test]
    fn label_joins_combos() {
        let bindings = KeyBindings::from_settings(&Settings::default());
        let command = crate::commands::command_symbol();
        let save = format!("{command}+S");
        let zoom = format!("{command}+Equals / {command}+Plus");
        assert_eq!(
            bindings.label(CommandId::Save).as_deref(),
            Some(save.as_str())
        );
        assert_eq!(
            bindings.label(CommandId::ZoomIn).as_deref(),
            Some(zoom.as_str())
        );
        assert_eq!(bindings.label(CommandId::Cut), None);
    }

    #[test]
    fn conflicts_are_detected() {
        let settings = settings_with(&[("save_as", &["Ctrl+S"])]);
        let bindings = KeyBindings::from_settings(&settings);
        assert_eq!(
            bindings.conflicts(),
            vec![(
                KeyCombo::ctrl(Key::S),
                vec![CommandId::Save, CommandId::SaveAs]
            )]
        );
    }

    #[test]
    fn default_table_has_no_conflicts() {
        let bindings = KeyBindings::from_settings(&Settings::default());
        assert!(bindings.conflicts().is_empty());
    }

    #[test]
    fn converted_copy_event_matches_ctrl_c() {
        let combo = KeyCombo::ctrl(Key::C);
        assert_eq!(
            shortcut_event_index(&[egui::Event::Copy], &combo, egui::Modifiers::COMMAND),
            Some(0)
        );
    }

    #[test]
    fn converted_cut_event_matches_ctrl_x() {
        let combo = KeyCombo::ctrl(Key::X);
        assert_eq!(
            shortcut_event_index(&[egui::Event::Cut], &combo, egui::Modifiers::COMMAND),
            Some(0)
        );
    }

    #[test]
    fn converted_paste_event_matches_ctrl_v() {
        let combo = KeyCombo::ctrl(Key::V);
        assert_eq!(
            shortcut_event_index(
                &[egui::Event::Paste("x".to_owned())],
                &combo,
                egui::Modifiers::COMMAND
            ),
            Some(0)
        );
    }

    #[test]
    fn released_v_matches_ctrl_v_without_a_paste_event() {
        let combo = KeyCombo::ctrl(Key::V);
        let events = [egui::Event::Key {
            key: Key::V,
            physical_key: Some(Key::V),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }];
        assert_eq!(
            shortcut_event_index(&events, &combo, egui::Modifiers::COMMAND),
            Some(0)
        );
    }

    #[test]
    fn exact_modifiers_are_required() {
        let combo = KeyCombo::ctrl(Key::C);
        assert_eq!(
            shortcut_event_index(
                &[egui::Event::Copy],
                &combo,
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
            ),
            None
        );
        // A press is not a converted event.
        assert_eq!(
            shortcut_event_index(
                &[egui::Event::Key {
                    key: Key::C,
                    physical_key: Some(Key::C),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                }],
                &combo,
                egui::Modifiers::COMMAND
            ),
            None
        );
    }

    #[test]
    fn shift_modified_copy_requires_shift() {
        let combo = KeyCombo::ctrl_shift(Key::C);
        assert_eq!(
            shortcut_event_index(
                &[egui::Event::Copy],
                &combo,
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
            ),
            Some(0)
        );
        assert_eq!(
            shortcut_event_index(&[egui::Event::Copy], &combo, egui::Modifiers::COMMAND),
            None
        );
    }

    #[test]
    fn alt_v_does_not_match_a_paste_event() {
        let combo = KeyCombo {
            ctrl: false,
            shift: false,
            alt: true,
            key: Key::V,
        };
        assert_eq!(
            shortcut_event_index(
                &[egui::Event::Paste("x".to_owned())],
                &combo,
                egui::Modifiers::ALT
            ),
            None
        );
    }

    #[test]
    fn non_matching_combo_returns_none() {
        let combo = KeyCombo::ctrl(Key::S);
        assert_eq!(
            shortcut_event_index(&[egui::Event::Copy], &combo, egui::Modifiers::COMMAND),
            None
        );
    }

    #[test]
    fn native_clipboard_combos_pass_through() {
        use CommandId::*;
        assert!(is_native_passthrough(Copy, &KeyCombo::ctrl(Key::C)));
        assert!(is_native_passthrough(Copy, &KeyCombo::ctrl_shift(Key::C)));
        assert!(is_native_passthrough(Cut, &KeyCombo::ctrl(Key::X)));
        assert!(is_native_passthrough(Paste, &KeyCombo::ctrl(Key::V)));
        assert!(is_native_passthrough(SelectAll, &KeyCombo::ctrl(Key::A)));
        // Non-native combos remain consumable.
        assert!(!is_native_passthrough(Paste, &KeyCombo::ctrl(Key::E)));
        assert!(!is_native_passthrough(
            SelectAll,
            &KeyCombo::ctrl_shift(Key::A)
        ));
        // Only the *matching* command passes through; a remap onto the same
        // physical key must still be consumed.
        assert!(!is_native_passthrough(Find, &KeyCombo::ctrl(Key::C)));
    }

    #[test]
    fn mixed_combos_keep_the_custom_one() {
        let combos = [KeyCombo::ctrl(Key::V), KeyCombo::ctrl(Key::E)];
        assert_eq!(
            consumable_combos(CommandId::Paste, &combos),
            vec![KeyCombo::ctrl(Key::E)]
        );
        let all_native = [KeyCombo::ctrl(Key::V)];
        assert!(consumable_combos(CommandId::Paste, &all_native).is_empty());
    }
}
