//! Touch / synthesized primary-mouse twin deduplication for Android `NativeActivity`.
//!
//! Some Android `winit`/`NativeActivity` versions emit both a `Touch` event and a synthesized
//! `PointerButton { button: Primary }` at the exact same coordinates in the same frame, or
//! omit primary pointer button events for single-finger taps. `egui` already translates primary
//! touches into pointer events, so duplicate synthesized mouse events in the same frame can
//! cause double-clicks on menus, grid thumbnails, or filmstrip items.

/// Strip redundant synthesized `PointerButton` events when a `Touch` event is already present
/// in the same `RawInput` batch.
pub fn dedupe_touch_mouse_twins(raw: &mut egui::RawInput) {
    let has_touch = raw
        .events
        .iter()
        .any(|e| matches!(e, egui::Event::Touch { .. }));
    if !has_touch {
        return;
    }

    // Collect touch positions in this batch so we only drop duplicate synthesized mouse events
    // that sit right on top of an active touch point.
    let touch_positions: Vec<egui::Pos2> = raw
        .events
        .iter()
        .filter_map(|e| match e {
            egui::Event::Touch { pos, .. } => Some(*pos),
            _ => None,
        })
        .collect();

    raw.events.retain(|e| match e {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            ..
        } => !touch_positions.iter().any(|tp| (*tp - *pos).length_sq() < 4.0),
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_duplicate_mouse_click_at_same_touch_position() {
        let pos = egui::pos2(240.0, 180.0);
        let mut raw = egui::RawInput {
            events: vec![
                egui::Event::Touch {
                    device_id: egui::TouchDeviceId(0),
                    id: egui::TouchId(1),
                    phase: egui::TouchPhase::Start,
                    pos,
                    force: Some(1.0),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };

        dedupe_touch_mouse_twins(&mut raw);
        assert_eq!(raw.events.len(), 1);
        assert!(matches!(raw.events[0], egui::Event::Touch { .. }));
    }
}
