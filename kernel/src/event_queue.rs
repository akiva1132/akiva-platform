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
//!   - input.key     (kind=2): { code: u32, modifiers: u8, phase: u8 }
//!   - input.resize  (kind=3): { w: u32, h: u32, density_q8_8: u32 }
//!   - system.visibility (kind=4): { visible: u8 }
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
    Pointer { x: f32, y: f32, button: u8, phase: PointerPhase },
    Key { code: u32, modifiers: u8, phase: KeyPhase },
    Resize { width_px: u32, height_px: u32, density: f32 },
    Visibility { visible: bool },
}

const EVENT_POINTER: u32 = 1;
const EVENT_KEY: u32 = 2;
const EVENT_RESIZE: u32 = 3;
const EVENT_VISIBILITY: u32 = 4;

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
            }),
            EVENT_VISIBILITY if !bytes.is_empty() => Some(Event::Visibility {
                visible: bytes[0] != 0,
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
        // unconsumed one. Prevents backlog when the user drags fast.
        if let Event::Pointer { phase: PointerPhase::Move, .. } = ev {
            if let Some(Event::Pointer { phase: PointerPhase::Move, .. }) = self.inner.back() {
                self.inner.pop_back();
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
