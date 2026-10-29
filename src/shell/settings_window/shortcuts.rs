//! Shortcuts category: rebind any registry command, with conflict warnings.

use std::collections::BTreeMap;

use egui::RichText;
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::theme::Theme;

use crate::commands::{COMMANDS, CommandId, KeyCombo};
use crate::keybindings::KeyBindings;
use crate::settings::Settings;

use super::grid::settings_grid;
use super::{capturing_index, clear_capture, set_capturing};

/// What a captured key event asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureStep {
    /// Not part of the capture: a bare modifier, a key release/repeat, or a key
    /// without Ctrl/Alt held.
    Ignore,
    /// A new combo was pressed.
    Bind(KeyCombo),
    /// Backspace/Delete with Ctrl/Alt: the command becomes unbound.
    Clear,
    /// Escape: leave capture mode without changing anything.
    Cancel,
}

/// Whether `key` is a bare modifier key. egui exposes physical left/right
/// variants (`Super*` doubles for winit's `Meta*`).
fn is_modifier_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

/// Stores an override for `id` (`Some` binds a combo, `None` leaves it unbound).
pub(super) fn set_binding(settings: &mut Settings, id: CommandId, combo: Option<&KeyCombo>) {
    let value = match combo {
        Some(combo) => vec![combo.label()],
        None => Vec::new(),
    };
    settings.shortcuts.insert(id.as_str().to_owned(), value);
}

/// Drops `id`'s override, restoring its registry defaults.
pub(super) fn reset_binding(settings: &mut Settings, id: CommandId) {
    settings.shortcuts.remove(id.as_str());
}

/// Interprets a single key event for shortcut recording. Pure, so the capture
/// rules are locked by unit tests with synthetic events: only the logical `key`
/// is bound, bare modifiers are ignored, and a binding needs Ctrl/Alt.
fn interpret_capture_event(event: &egui::Event) -> CaptureStep {
    let egui::Event::Key {
        key,
        physical_key,
        pressed: true,
        repeat: false,
        modifiers,
    } = event
    else {
        return CaptureStep::Ignore;
    };
    if is_modifier_key(*key) || physical_key.is_some_and(is_modifier_key) {
        return CaptureStep::Ignore;
    }
    if *key == egui::Key::Escape {
        return CaptureStep::Cancel;
    }
    if !(modifiers.command || modifiers.alt) {
        return CaptureStep::Ignore;
    }
    if *key == egui::Key::Backspace || *key == egui::Key::Delete {
        return CaptureStep::Clear;
    }
    CaptureStep::Bind(KeyCombo {
        ctrl: modifiers.command,
        shift: modifiers.shift,
        alt: modifiers.alt,
        key: *key,
    })
}

/// Pass 2: a key *release* whose matching press was swallowed by egui-winit.
/// The integration converts a Ctrl+C/X/V press into `Copy`/`Cut`/`Paste` and
/// drops the original `Event::Key`, but it never converts releases — so the
/// release still carries the real key and modifiers.
fn interpret_release_event(event: &egui::Event) -> CaptureStep {
    let egui::Event::Key {
        key,
        pressed: false,
        repeat: false,
        modifiers,
        ..
    } = event
    else {
        return CaptureStep::Ignore;
    };
    if is_modifier_key(*key) || !(modifiers.command || modifiers.alt) {
        return CaptureStep::Ignore;
    }
    CaptureStep::Bind(KeyCombo {
        ctrl: modifiers.command,
        shift: modifiers.shift,
        alt: modifiers.alt,
        key: *key,
    })
}

/// Pass 3 fallback: the raw conversion events egui-winit emits *instead of* the
/// Ctrl+C/X/V press. Only `command || alt` is accepted, so Windows
/// Shift+Insert/Shift+Delete (also converted) are rejected.
fn interpret_conversion_event(event: &egui::Event, current: egui::Modifiers) -> CaptureStep {
    if !(current.command || current.alt) {
        return CaptureStep::Ignore;
    }
    let key = match event {
        egui::Event::Copy => egui::Key::C,
        egui::Event::Cut => egui::Key::X,
        egui::Event::Paste(_) => egui::Key::V,
        _ => return CaptureStep::Ignore,
    };
    CaptureStep::Bind(KeyCombo {
        ctrl: current.command,
        shift: current.shift,
        alt: current.alt,
        key,
    })
}

/// Interprets this frame's events for shortcut recording in three passes; the
/// first non-`Ignore` step wins, so a later pass never overrides an earlier one:
///
/// 1. a fresh key press (the normal path);
/// 2. a key release (winit swallows the converted Ctrl+C/X/V/Insert press but
///    leaves the release intact — and Ctrl+V is recordable with an empty
///    clipboard, which emits no conversion event at all);
/// 3. a raw `Copy`/`Cut`/`Paste` event (fallback when no release is present).
fn interpret_capture_events(events: &[egui::Event], current: egui::Modifiers) -> CaptureStep {
    events
        .iter()
        .map(interpret_capture_event)
        .find(|step| *step != CaptureStep::Ignore)
        .or_else(|| {
            events
                .iter()
                .map(interpret_release_event)
                .find(|step| *step != CaptureStep::Ignore)
        })
        .or_else(|| {
            events
                .iter()
                .map(|event| interpret_conversion_event(event, current))
                .find(|step| *step != CaptureStep::Ignore)
        })
        .unwrap_or(CaptureStep::Ignore)
}

/// Reads (and swallows) this frame's capture events while a capture is active.
fn capture_step(ui: &egui::Ui) -> CaptureStep {
    let (events, current) = ui.input(|input| (input.events.clone(), input.modifiers));
    let step = interpret_capture_events(&events, current);
    // Swallow every capture event: the settings modal must not see Escape (which
    // would close the window), and neither the key events nor the converted
    // Copy/Cut/Paste may leak to the editor while recording.
    ui.input_mut(|input| {
        input.events.retain(|event| {
            !matches!(
                event,
                egui::Event::Key { .. }
                    | egui::Event::Copy
                    | egui::Event::Cut
                    | egui::Event::Paste(_)
            )
        });
    });
    step
}

/// Commands grouped by registry category, in first-appearance order; each group
/// keeps the registry order.
fn command_groups() -> Vec<(&'static str, Vec<usize>)> {
    let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
    for (index, meta) in COMMANDS.iter().enumerate() {
        let category = meta.category.unwrap_or("command.category.general");
        match groups.iter_mut().find(|(name, _)| *name == category) {
            Some((_, members)) => members.push(index),
            None => groups.push((category, vec![index])),
        }
    }
    groups
}

/// Draws one keycap-style binding label.
fn keycap(ui: &mut egui::Ui, text: &str, overridden: bool) {
    let theme = Theme::from_ui(ui);
    let color = if overridden {
        theme.colors.accent
    } else {
        theme.colors.text
    };
    egui::Frame::new()
        .fill(theme.colors.band)
        .corner_radius(egui::CornerRadius::same(theme.radii.small))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(text).monospace().size(13.0).color(color))
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
}

/// Shortcuts category: rebind, reset, and warn about conflicting combinations.
pub(super) fn shortcuts_ui(ui: &mut egui::Ui, settings: &mut Settings) -> bool {
    let mut changed = false;

    if let Some(index) = capturing_index(ui.ctx()) {
        match capture_step(ui) {
            CaptureStep::Cancel => clear_capture(ui.ctx()),
            CaptureStep::Bind(combo) => {
                if let Some(meta) = COMMANDS.get(index) {
                    set_binding(settings, meta.id, Some(&combo));
                    changed = true;
                }
                clear_capture(ui.ctx());
            }
            CaptureStep::Clear => {
                if let Some(meta) = COMMANDS.get(index) {
                    set_binding(settings, meta.id, None);
                    changed = true;
                }
                clear_capture(ui.ctx());
            }
            CaptureStep::Ignore => {}
        }
    }

    let capturing = capturing_index(ui.ctx());
    let bindings = KeyBindings::from_settings(settings);
    let conflicts = bindings.conflicts();
    let mut row_conflicts: BTreeMap<CommandId, Vec<String>> = BTreeMap::new();
    for (combo, ids) in &conflicts {
        for id in ids {
            row_conflicts.entry(*id).or_default().push(combo.label());
        }
    }

    if capturing.is_some() {
        ui.label(RichText::new(t("settings.shortcuts.capture_hint")).strong());
        ui.add_space(4.0);
    }
    if !conflicts.is_empty() {
        let combos = conflicts
            .iter()
            .map(|(combo, _)| combo.label())
            .collect::<Vec<_>>()
            .join(", ");
        let count = conflicts.len().to_string();
        ui.colored_label(
            Theme::from_ui(ui).colors.warn,
            t_with(
                "settings.shortcuts.conflicts",
                &[("count", &count), ("combos", &combos)],
            ),
        );
        ui.add_space(4.0);
    }

    settings_grid(ui, "typ_edit_settings_grid_shortcuts", |table| {
        for (category, members) in command_groups() {
            let theme = Theme::from_ui(table.egui_ui());
            table.row("", |ui| {
                ui.label(
                    RichText::new(t(category))
                        .strong()
                        .color(theme.colors.text_strong),
                );
            });
            for index in members {
                let meta = &COMMANDS[index];
                let overridden = settings.shortcuts.contains_key(meta.id.as_str());
                let recording = capturing == Some(index);
                let effective = bindings.label(meta.id);
                table.row(&t(meta.label), |ui| {
                    ui.horizontal(|ui| {
                        match &effective {
                            Some(label) => keycap(ui, label, overridden),
                            None => keycap(ui, &t("settings.shortcuts.unbound"), false),
                        };
                        let state = if overridden {
                            t("settings.shortcuts.custom")
                        } else {
                            t("settings.shortcuts.default")
                        };
                        let muted = Theme::from_ui(ui).colors.text_muted;
                        ui.label(RichText::new(state).color(muted));
                        if let Some(combos) = row_conflicts.get(&meta.id) {
                            ui.colored_label(
                                Theme::from_ui(ui).colors.warn_text,
                                t("settings.shortcuts.conflict"),
                            )
                            .on_hover_text(combos.join(", "));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if button(
                                ui,
                                &t("settings.shortcuts.reset"),
                                ButtonVariant::Quiet,
                                overridden,
                            )
                            .clicked()
                            {
                                reset_binding(settings, meta.id);
                                changed = true;
                            }
                            let record_variant = if recording {
                                ButtonVariant::Primary
                            } else {
                                ButtonVariant::Secondary
                            };
                            if button(ui, &t("settings.shortcuts.record"), record_variant, true)
                                .clicked()
                            {
                                set_capturing(ui.ctx(), Some(index));
                            }
                        });
                    });
                });
            }
        }
    });

    ui.add_space(6.0);
    if button(
        ui,
        &t("settings.shortcuts.reset_all"),
        ButtonVariant::Secondary,
        !settings.shortcuts.is_empty(),
    )
    .clicked()
        && !settings.shortcuts.is_empty()
    {
        settings.shortcuts.clear();
        changed = true;
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Key;

    #[test]
    fn set_binding_stores_an_override() {
        let mut settings = Settings::default();
        set_binding(
            &mut settings,
            CommandId::Save,
            Some(&KeyCombo::ctrl(Key::K)),
        );
        assert_eq!(
            settings.shortcuts.get("save").map(Vec::as_slice),
            Some(&[format!("{}+K", crate::commands::command_symbol())][..])
        );
        let bindings = KeyBindings::from_settings(&settings);
        let expected = format!("{}+K", crate::commands::command_symbol());
        assert_eq!(
            bindings.label(CommandId::Save).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn reset_binding_removes_the_override() {
        let mut settings = Settings::default();
        set_binding(
            &mut settings,
            CommandId::Save,
            Some(&KeyCombo::ctrl(Key::K)),
        );
        reset_binding(&mut settings, CommandId::Save);
        assert!(!settings.shortcuts.contains_key("save"));
        let bindings = KeyBindings::from_settings(&settings);
        let expected = format!("{}+S", crate::commands::command_symbol());
        assert_eq!(
            bindings.label(CommandId::Save).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn unbound_binding_round_trips_as_empty() {
        let mut settings = Settings::default();
        set_binding(&mut settings, CommandId::Save, None);
        assert_eq!(
            settings.shortcuts.get("save").map(Vec::as_slice),
            Some(&[][..])
        );
        let bindings = KeyBindings::from_settings(&settings);
        assert_eq!(bindings.label(CommandId::Save), None);
    }

    fn key_event(
        key: egui::Key,
        physical_key: Option<egui::Key>,
        modifiers: egui::Modifiers,
    ) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn ctrl_pressed_alone_is_ignored() {
        let event = key_event(
            Key::ControlLeft,
            Some(Key::ControlLeft),
            egui::Modifiers::COMMAND,
        );
        assert_eq!(interpret_capture_event(&event), CaptureStep::Ignore);
    }

    #[test]
    fn alt_pressed_alone_is_ignored() {
        let event = key_event(Key::AltLeft, Some(Key::AltLeft), egui::Modifiers::ALT);
        assert_eq!(interpret_capture_event(&event), CaptureStep::Ignore);
    }

    #[test]
    fn a_logical_modifier_without_physical_is_ignored() {
        let event = key_event(Key::ShiftRight, None, egui::Modifiers::SHIFT);
        assert_eq!(interpret_capture_event(&event), CaptureStep::Ignore);
    }

    #[test]
    fn ctrl_s_commits_ctrl_s() {
        let event = key_event(Key::S, Some(Key::S), egui::Modifiers::COMMAND);
        assert_eq!(
            interpret_capture_event(&event),
            CaptureStep::Bind(KeyCombo {
                ctrl: true,
                shift: false,
                alt: false,
                key: Key::S,
            })
        );
    }

    #[test]
    fn ctrl_shift_s_preserves_shift() {
        let event = key_event(
            Key::S,
            Some(Key::S),
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        );
        assert_eq!(
            interpret_capture_event(&event),
            CaptureStep::Bind(KeyCombo {
                ctrl: true,
                shift: true,
                alt: false,
                key: Key::S,
            })
        );
    }

    #[test]
    fn alt_arrow_up_commits_alt_arrow_up() {
        let event = key_event(Key::ArrowUp, Some(Key::ArrowUp), egui::Modifiers::ALT);
        assert_eq!(
            interpret_capture_event(&event),
            CaptureStep::Bind(KeyCombo {
                ctrl: false,
                shift: false,
                alt: true,
                key: Key::ArrowUp,
            })
        );
    }

    #[test]
    fn escape_cancels_and_ctrl_backspace_clears() {
        let escape = key_event(Key::Escape, Some(Key::Escape), egui::Modifiers::NONE);
        assert_eq!(interpret_capture_event(&escape), CaptureStep::Cancel);

        let backspace = key_event(
            Key::Backspace,
            Some(Key::Backspace),
            egui::Modifiers::COMMAND,
        );
        assert_eq!(interpret_capture_event(&backspace), CaptureStep::Clear);
    }

    #[test]
    fn a_plain_key_without_modifiers_is_ignored() {
        let event = key_event(Key::S, Some(Key::S), egui::Modifiers::NONE);
        assert_eq!(interpret_capture_event(&event), CaptureStep::Ignore);
    }

    fn release_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers,
        }
    }

    fn binds(ctrl: bool, shift: bool, alt: bool, key: egui::Key) -> CaptureStep {
        CaptureStep::Bind(KeyCombo {
            ctrl,
            shift,
            alt,
            key,
        })
    }

    #[test]
    fn released_ctrl_c_records_ctrl_c() {
        let events = [release_event(Key::C, egui::Modifiers::COMMAND)];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::COMMAND),
            binds(true, false, false, Key::C)
        );
    }

    #[test]
    fn released_ctrl_v_records_ctrl_v() {
        let events = [release_event(Key::V, egui::Modifiers::COMMAND)];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::COMMAND),
            binds(true, false, false, Key::V)
        );
    }

    #[test]
    fn released_ctrl_insert_records_ctrl_insert_not_ctrl_c() {
        let events = [release_event(Key::Insert, egui::Modifiers::COMMAND)];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::COMMAND),
            binds(true, false, false, Key::Insert)
        );
    }

    #[test]
    fn released_shift_insert_is_ignored() {
        let events = [release_event(Key::Insert, egui::Modifiers::SHIFT)];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::SHIFT),
            CaptureStep::Ignore
        );
    }

    #[test]
    fn paste_event_fallback_records_ctrl_v() {
        let events = [egui::Event::Paste("x".to_owned())];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::COMMAND),
            binds(true, false, false, Key::V)
        );
    }

    #[test]
    fn copy_event_fallback_records_ctrl_c() {
        let events = [egui::Event::Copy];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::COMMAND),
            binds(true, false, false, Key::C)
        );
    }

    #[test]
    fn shift_only_conversion_event_is_ignored() {
        let events = [egui::Event::Copy];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::SHIFT),
            CaptureStep::Ignore
        );
    }

    #[test]
    fn a_pressed_key_beats_a_converted_event() {
        // Pass 1 outranks pass 3, so a real press wins over a stale Copy event.
        let events = [
            egui::Event::Copy,
            key_event(Key::S, Some(Key::S), egui::Modifiers::COMMAND),
        ];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::COMMAND),
            binds(true, false, false, Key::S)
        );
    }

    #[test]
    fn a_release_beats_a_conversion_event() {
        // Pass 2 outranks pass 3: Ctrl+Insert stays Ctrl+Insert, never Ctrl+C.
        let events = [
            release_event(Key::Insert, egui::Modifiers::COMMAND),
            egui::Event::Copy,
        ];
        assert_eq!(
            interpret_capture_events(&events, egui::Modifiers::COMMAND),
            binds(true, false, false, Key::Insert)
        );
    }
}
