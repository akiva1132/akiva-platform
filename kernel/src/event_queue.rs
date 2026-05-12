//! Event queue.
//!
//! All input / system / IPC events land here before dispatch. The kernel
//! drains the queue once per frame and handles each event against the
//! subsystems that care. Events are decoded from a host-provided byte
//! buffer into typed variants; the rest of the kernel never touches raw
//! bytes.
//!
//! Wire format (little-endian, stable across kernel and HAL):
//!   - input.pointer (kind=1): { x: f32, y: f32, button: u8, phase: u8 }
//!     (Stone 13.4 extended: optional trailing `display_id: u32` —
//!     legacy 10-byte payload defaults to display 0.)
//!   - input.key     (kind=2): { code: u32, modifiers: u8, phase: u8 }
//!   - input.resize  (kind=3): { w: u32, h: u32, density_q8_8: u32 }
//!     (Stone 13.4 extended: optional trailing `display_id: u32` —
//!     legacy 12-byte payload defaults to display 0.)
//!   - system.visibility (kind=4): { visible: u8 }
//!   - input.text    (kind=5): { text_len: u16, text: utf8 }
//!     The committed printable text the user typed. Foundation gap fix
//!     — KeyEvent's `code` was deprecated `KeyboardEvent.keyCode`, which
//!     can't carry character data, so apps wanting typed input had no
//!     wire path. Single-char keydown (`e.key.length == 1` with no
//!     Ctrl/Meta) and `compositionend` (`e.data`) both produce one of
//!     these. Modifier-only keystrokes (Shift, arrows, F-keys) stay on
//!     the key channel and don't fire a text event.
//!   - input.wheel   (kind=6): { dx: f32, dy: f32, display_id: u32 }
//!     A one-shot scroll delta from the `wheel` DOM event (mouse wheel,
//!     trackpad two-finger swipe). Positive `dy` = "content should move
//!     up" (matches DOM convention). Distinct channel from `pointer`
//!     because wheels don't have Down/Move/Up phases, so the dispatcher
//!     can't conflate them with drags, and they need to route to a
//!     hit-test against the topmost `Scroll`/`LazyColumn` under the
//!     cursor instead of the activity's focused widget.
//!
//! See docs/runtime-web.md §4 for the full pipeline, including where this
//! queue sits between the worker's `akiva_on_event` and the kernel frame
//! loop.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down = 0,
    Move = 1,
    Up = 2,
    Cancel = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Down = 0,
    Repeat = 1,
    Up = 2,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Stone 13.4 — `display_id` identifies which display the event
    /// originated on. `0` is the primary; followers carry their
    /// allocated id. Legacy host code that doesn't include the id
    /// reads back as `0`.
    Pointer { x: f32, y: f32, button: u8, phase: PointerPhase, display_id: u32 },
    Key { code: u32, modifiers: u8, phase: KeyPhase },
    Resize { width_px: u32, height_px: u32, density: f32, display_id: u32 },
    Visibility { visible: bool },
    /// Foundation gap — the printable text a user typed. Distinct
    /// channel from `Key` because `Key.code` is the deprecated
    /// `KeyboardEvent.keyCode` (numeric, layout-dependent, can't carry
    /// a character) and apps that want text routinely also want the
    /// composition pipeline (`compositionend.data`) collapsed into
    /// the same callback. One event per chunk of committed text:
    /// typing "abc" produces three events; an IME committing "שלום"
    /// produces one event of length 4.
    TextInput { text: String },
    /// One-shot scroll delta from a `wheel` DOM event. `dx`/`dy` are
    /// in CSS pixels times the device-pixel ratio (matches pointer
    /// coordinate space). Positive `dy` = "content scrolls down" by
    /// DOM convention. `display_id` is the originating tab's display
    /// so the framework can route the event to the activity owning
    /// that display, same pattern as `Pointer`.
    Wheel { dx: f32, dy: f32, display_id: u32 },
}

const EVENT_POINTER: u32 = 1;
const EVENT_KEY: u32 = 2;
const EVENT_RESIZE: u32 = 3;
const EVENT_VISIBILITY: u32 = 4;
const EVENT_TEXT: u32 = 5;
const EVENT_WHEEL: u32 = 6;

impl Event {
    pub fn decode(kind: u32, bytes: &[u8]) -> Option<Self> {
        match kind {
            EVENT_POINTER if bytes.len() >= 10 => Some(Event::Pointer {
                x: f32::from_le_bytes(bytes[0..4].try_into().ok()?),
                y: f32::from_le_bytes(bytes[4..8].try_into().ok()?),
                button: bytes[8],
                phase: match bytes[9] {
                    0 => PointerPhase::Down,
                    1 => PointerPhase::Move,
                    2 => PointerPhase::Up,
                    _ => PointerPhase::Cancel,
                },
                display_id: if bytes.len() >= 14 {
                    u32::from_le_bytes(bytes[10..14].try_into().ok()?)
                } else {
                    0
                },
            }),
            EVENT_KEY if bytes.len() >= 6 => Some(Event::Key {
                code: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
                modifiers: bytes[4],
                phase: match bytes[5] {
                    0 => KeyPhase::Down,
                    1 => KeyPhase::Repeat,
                    _ => KeyPhase::Up,
                },
            }),
            EVENT_RESIZE if bytes.len() >= 12 => Some(Event::Resize {
                width_px: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
                height_px: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
                // q8.8 fixed-point — density is small and deterministic.
                density: u32::from_le_bytes(bytes[8..12].try_into().ok()?) as f32 / 256.0,
                display_id: if bytes.len() >= 16 {
                    u32::from_le_bytes(bytes[12..16].try_into().ok()?)
                } else {
                    0
                },
            }),
            EVENT_VISIBILITY if !bytes.is_empty() => Some(Event::Visibility {
                visible: bytes[0] != 0,
            }),
            EVENT_TEXT if bytes.len() >= 2 => {
                let text_len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
                if 2 + text_len > bytes.len() { return None; }
                let text = std::str::from_utf8(&bytes[2..2 + text_len]).ok()?.to_string();
                Some(Event::TextInput { text })
            }
            EVENT_WHEEL if bytes.len() >= 12 => Some(Event::Wheel {
                dx: f32::from_le_bytes(bytes[0..4].try_into().ok()?),
                dy: f32::from_le_bytes(bytes[4..8].try_into().ok()?),
                display_id: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            }),
            _ => None,
        }
    }
}

pub struct EventQueue {
    inner: VecDeque<Event>,
}

impl EventQueue {
    pub const fn new() -> Self {
        Self { inner: VecDeque::new() }
    }

    pub fn push(&mut self, ev: Event) {
        // Coalesce pointer-moves: the latest pointer-move subsumes any prior
        // unconsumed one **on the same display**. Prevents backlog when the
        // user drags fast; per-display gating keeps a drag in tab A from
        // eating a move in tab B.
        if let Event::Pointer { phase: PointerPhase::Move, display_id: new_did, .. } = ev {
            if let Some(Event::Pointer {
                phase: PointerPhase::Move, display_id: prev_did, ..
            }) = self.inner.back() {
                if *prev_did == new_did {
                    self.inner.pop_back();
                }
            }
        }
        self.inner.push_back(ev);
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.inner.pop_front()
    }

    pub fn len(&self) -> usize { self.inner.len() }
    pub fn is_empty(&self) -> bool { self.inner.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_payload(s: &str) -> Vec<u8> {
        let bytes = s.as_bytes();
        let mut buf = Vec::with_capacity(2 + bytes.len());
        buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(bytes);
        buf
    }

    #[test]
    fn text_event_decodes_ascii() {
        let buf = text_payload("a");
        match Event::decode(EVENT_TEXT, &buf) {
            Some(Event::TextInput { text }) => assert_eq!(text, "a"),
            other => panic!("expected TextInput, got {other:?}"),
        }
    }

    #[test]
    fn text_event_decodes_unicode_multi_codepoint() {
        // emoji + Hebrew (per memory: i18n is post-MVP, but the wire
        // must carry arbitrary UTF-8 anyway because IME paste of any
        // language is legal). Length is in BYTES of UTF-8.
        let buf = text_payload("שלום👋");
        match Event::decode(EVENT_TEXT, &buf) {
            Some(Event::TextInput { text }) => assert_eq!(text, "שלום👋"),
            other => panic!("expected TextInput, got {other:?}"),
        }
    }

    #[test]
    fn text_event_rejects_truncated_payload() {
        // Header claims 10 bytes of text but only 3 follow.
        let mut buf = Vec::new();
        buf.extend_from_slice(&10u16.to_le_bytes());
        buf.extend_from_slice(b"abc");
        assert!(Event::decode(EVENT_TEXT, &buf).is_none());
    }

    #[test]
    fn text_event_rejects_invalid_utf8() {
        // text_len = 2, bytes 0xFF 0xFE — invalid UTF-8.
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&[0xFF, 0xFE]);
        assert!(Event::decode(EVENT_TEXT, &buf).is_none());
    }

    #[test]
    fn text_event_decodes_empty_text() {
        // Empty text is wire-legal even though `dispatch_text_input`
        // drops it framework-side. The decoder doesn't second-guess
        // the host.
        let buf = text_payload("");
        match Event::decode(EVENT_TEXT, &buf) {
            Some(Event::TextInput { text }) => assert!(text.is_empty()),
            other => panic!("expected TextInput, got {other:?}"),
        }
    }

    #[test]
    fn text_event_rejects_too_short_for_header() {
        // 1 byte — not even enough for the u16 length prefix.
        assert!(Event::decode(EVENT_TEXT, &[0x05]).is_none());
        assert!(Event::decode(EVENT_TEXT, &[]).is_none());
    }

    fn wheel_payload(dx: f32, dy: f32, display_id: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12);
        buf.extend_from_slice(&dx.to_le_bytes());
        buf.extend_from_slice(&dy.to_le_bytes());
        buf.extend_from_slice(&display_id.to_le_bytes());
        buf
    }

    #[test]
    fn wheel_event_decodes_full_payload() {
        let buf = wheel_payload(-2.5, 17.0, 3);
        match Event::decode(EVENT_WHEEL, &buf) {
            Some(Event::Wheel { dx, dy, display_id }) => {
                assert_eq!(dx, -2.5);
                assert_eq!(dy, 17.0);
                assert_eq!(display_id, 3);
            }
            other => panic!("expected Wheel, got {other:?}"),
        }
    }

    #[test]
    fn wheel_event_rejects_truncated_payload() {
        // 11 bytes — one short of the required 12.
        let mut buf = wheel_payload(1.0, 2.0, 0);
        buf.pop();
        assert!(Event::decode(EVENT_WHEEL, &buf).is_none());
        assert!(Event::decode(EVENT_WHEEL, &[]).is_none());
    }

    #[test]
    fn wheel_events_do_not_coalesce() {
        // Pointer-moves coalesce (only latest matters). Wheel deltas are
        // additive — losing one means losing scroll distance, which the
        // user notices. Two wheel events should land in the queue.
        let mut q = EventQueue::new();
        q.push(Event::Wheel { dx: 0.0, dy: 10.0, display_id: 0 });
        q.push(Event::Wheel { dx: 0.0, dy: 12.0, display_id: 0 });
        assert_eq!(q.len(), 2);
    }
}
