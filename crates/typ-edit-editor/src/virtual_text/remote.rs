//! Remote-participant cursor painting for the virtualized text path.
//!
//! Remote cursors arrive as byte offsets (validated and clamped by the host);
//! this module converts them to the editor's char space, paints a translucent
//! selection fill behind each range and a colored caret line with the
//! participant's name, reusing the local caret's geometry helpers. Only the
//! active file's carets are passed in.

use egui::{Align2, Color32, FontId, Rect};

use typ_edit_buffer::TextBuffer;

use super::caret::{paint_caret_rect, paint_galley_selection};
use super::hit::{LineSlot, caret_screen_rect, line_char_len};
use crate::RemoteCaret;

/// The stable 8-color palette remote cursors index into.
pub(super) const REMOTE_PALETTE: [Color32; 8] = [
    Color32::from_rgb(0xE0, 0x6C, 0x75),
    Color32::from_rgb(0x98, 0xC3, 0x79),
    Color32::from_rgb(0x61, 0xAF, 0xEF),
    Color32::from_rgb(0xE5, 0xC0, 0x7B),
    Color32::from_rgb(0xC6, 0x78, 0xDD),
    Color32::from_rgb(0x56, 0xB6, 0xC2),
    Color32::from_rgb(0xD1, 0x9A, 0x66),
    Color32::from_rgb(0xBE, 0x50, 0x46),
];

/// The color for a palette index, wrapping into the palette.
pub(super) fn remote_color(color_index: u8) -> Color32 {
    REMOTE_PALETTE[color_index as usize % REMOTE_PALETTE.len()]
}

/// Paints every remote cursor given for the active file.
pub(super) fn paint_remote(
    ui: &egui::Ui,
    clip: Rect,
    slots: &[LineSlot],
    buffer: &TextBuffer,
    carets: &[RemoteCaret],
) {
    if carets.is_empty() {
        return;
    }
    let painter = ui.painter().with_clip_rect(clip);
    for remote in carets {
        let color = remote_color(remote.color_index);
        let fill = color.gamma_multiply(0.24);
        for (start, end) in &remote.selections {
            if end <= start {
                continue;
            }
            let sel = buffer.byte_to_char(*start)..buffer.byte_to_char(*end);
            for slot in slots {
                let line_start = buffer.line_start(slot.line);
                let line_end = line_start + line_char_len(buffer, slot.line);
                let lo = sel.start.max(line_start);
                let hi = sel.end.min(line_end);
                if lo >= hi {
                    continue;
                }
                let Some(local) = slot.window_local((lo - line_start)..(hi - line_start)) else {
                    continue;
                };
                let _ = paint_galley_selection(
                    &painter,
                    clip,
                    &slot.galley,
                    slot.screen_origin(),
                    local,
                    fill,
                    2.0,
                );
            }
        }
        for byte in &remote.carets {
            let ch = buffer.byte_to_char(*byte);
            if let Some(rect) = caret_screen_rect(buffer, slots, ch) {
                paint_caret_rect(&painter, clip, rect, color);
            }
        }
        if let Some(first) = remote.carets.first() {
            let ch = buffer.byte_to_char(*first);
            if let Some(rect) = caret_screen_rect(buffer, slots, ch) {
                painter.text(
                    rect.left_top() - egui::vec2(0.0, 1.0),
                    Align2::LEFT_BOTTOM,
                    &remote.user,
                    FontId::proportional(10.0),
                    color,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_color_wraps_the_palette() {
        assert_eq!(remote_color(0), REMOTE_PALETTE[0]);
        assert_eq!(remote_color(7), REMOTE_PALETTE[7]);
        assert_eq!(remote_color(8), REMOTE_PALETTE[0]);
        assert_eq!(remote_color(255), REMOTE_PALETTE[255 % 8]);
    }
}
