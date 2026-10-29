use std::cell::Cell;
use std::time::Duration;

use gpui::{PenDeviceId, PenEvent, PenPhase, PenSample, PenTip, Pixels, TouchId, point, px};
use objc2_app_kit::{
    NSEvent, NSEventButtonMask, NSEventSubtype, NSEventType, NSPointingDeviceType,
};
use objc2_foundation::NSUInteger;
use smallvec::SmallVec;

use crate::events::read_modifiers;

const CAPABILITY_TILT: NSUInteger = 0x0080 | 0x0100;
const CAPABILITY_TANGENTIAL_PRESSURE: NSUInteger = 0x0800;
const CAPABILITY_ROTATION: NSUInteger = 0x2000;
const MAX_TILT_DEGREES: f64 = 60.;

#[derive(Clone, Copy)]
struct Tool {
    tip: PenTip,
    device_id: Option<PenDeviceId>,
    capabilities: NSUInteger,
}

impl Tool {
    const UNKNOWN: Self = Self {
        tip: PenTip::Tip,
        device_id: None,
        capabilities: CAPABILITY_TILT,
    };
}

thread_local! {
    static TOOL: Cell<Option<Tool>> = const { Cell::new(None) };
    static SAVED_COALESCING: Cell<Option<bool>> = const { Cell::new(None) };
}

fn enter_proximity(tool: Tool) {
    TOOL.set(Some(tool));
    if SAVED_COALESCING.get().is_none() {
        SAVED_COALESCING.set(Some(NSEvent::isMouseCoalescingEnabled()));
        NSEvent::setMouseCoalescingEnabled(false);
    }
}

fn leave_proximity() {
    TOOL.set(None);
    if let Some(enabled) = SAVED_COALESCING.take() {
        NSEvent::setMouseCoalescingEnabled(enabled);
    }
}

/// Tracks the pen in proximity, which every window shares. Returns whether the pen is
/// entering, or `None` when `event` is not a proximity event.
pub(crate) fn proximity(event: &NSEvent) -> Option<bool> {
    if event.r#type() != NSEventType::TabletProximity {
        return None;
    }
    if event.isEnteringProximity() {
        enter_proximity(Tool {
            tip: if event.pointingDeviceType() == NSPointingDeviceType::Eraser {
                PenTip::Eraser
            } else {
                PenTip::Tip
            },
            device_id: match event.uniqueID() {
                0 => None,
                id => Some(PenDeviceId(id)),
            },
            capabilities: event.capabilityMask(),
        });
        Some(true)
    } else {
        leave_proximity();
        Some(false)
    }
}

/// Pen strokes and hover of one window, read from mouse events that carry tablet data.
#[derive(Default)]
pub(crate) struct PenInput {
    next_id: u64,
    stroke: Option<TouchId>,
    hover: Option<TouchId>,
    last_sample: Option<PenSample>,
    tool: Option<Tool>,
}

impl PenInput {
    fn issue_id(&mut self) -> TouchId {
        self.next_id += 1;
        TouchId(self.next_id)
    }

    pub(crate) fn event(&mut self, event: &NSEvent, window_height: Pixels) -> Option<PenEvent> {
        let event_type = event.r#type();
        match event_type {
            NSEventType::TabletPoint => {
                if self.stroke.is_none()
                    || self.last_sample.is_some_and(|sample| {
                        sample.timestamp == Duration::from_secs_f64(event.timestamp())
                    })
                {
                    return None;
                }
            }
            NSEventType::LeftMouseDown
            | NSEventType::LeftMouseDragged
            | NSEventType::LeftMouseUp
            | NSEventType::MouseMoved
                if event.subtype() == NSEventSubtype::TabletPoint => {}
            _ => return None,
        }

        let tool = TOOL.get().unwrap_or_else(|| {
            enter_proximity(Tool::UNKNOWN);
            Tool::UNKNOWN
        });
        self.tool = Some(tool);
        let (phase, id) = match event_type {
            NSEventType::LeftMouseDown => {
                let id = self.issue_id();
                self.stroke = Some(id);
                (PenPhase::Down, id)
            }
            NSEventType::LeftMouseDragged | NSEventType::TabletPoint => {
                let id = match self.stroke {
                    Some(id) => id,
                    None => {
                        let id = self.issue_id();
                        self.stroke = Some(id);
                        id
                    }
                };
                (PenPhase::Moved, id)
            }
            NSEventType::LeftMouseUp => {
                let id = match self.stroke.take() {
                    Some(id) => id,
                    None => self.issue_id(),
                };
                (PenPhase::Up, id)
            }
            _ => match self.hover {
                Some(id) => (PenPhase::Moved, id),
                None => {
                    let id = self.issue_id();
                    self.hover = Some(id);
                    (PenPhase::Entered, id)
                }
            },
        };
        let in_contact =
            matches!(phase, PenPhase::Down | PenPhase::Moved) && self.stroke == Some(id);

        let sample = sample(event, window_height, tool, in_contact);
        self.last_sample = Some(sample);
        let mut samples = SmallVec::new();
        samples.push(sample);
        Some(PenEvent {
            id,
            device_id: tool.device_id,
            phase,
            tip: tool.tip,
            barrel: event
                .buttonMask()
                .intersects(NSEventButtonMask::PenLowerSide | NSEventButtonMask::PenUpperSide),
            in_contact,
            modifiers: read_modifiers(event),
            samples,
            predicted: Default::default(),
        })
    }

    /// Ends hover when the pen leaves the window while still in proximity.
    pub(crate) fn leave_window(&mut self) -> Option<PenEvent> {
        if self.stroke.is_some() {
            return None;
        }
        let id = self.hover.take()?;
        Some(self.end(id, PenPhase::Exited))
    }

    /// Ends the stroke and hover when the pen leaves proximity.
    pub(crate) fn leave_proximity(&mut self) -> SmallVec<[PenEvent; 2]> {
        let mut events = SmallVec::new();
        if let Some(id) = self.stroke.take() {
            events.push(self.end(id, PenPhase::Cancelled));
            if self.hover.is_none() {
                events.push(self.end(id, PenPhase::Exited));
            }
        }
        if let Some(id) = self.hover.take() {
            events.push(self.end(id, PenPhase::Exited));
        }
        events
    }

    fn end(&self, id: TouchId, phase: PenPhase) -> PenEvent {
        let tool = self.tool.unwrap_or(Tool::UNKNOWN);
        PenEvent {
            id,
            device_id: tool.device_id,
            phase,
            tip: tool.tip,
            barrel: false,
            in_contact: false,
            modifiers: Default::default(),
            samples: self
                .last_sample
                .map(|sample| PenSample {
                    pressure: 0.,
                    ..sample
                })
                .into_iter()
                .collect(),
            predicted: Default::default(),
        }
    }
}

fn sample(event: &NSEvent, window_height: Pixels, tool: Tool, in_contact: bool) -> PenSample {
    let location = event.locationInWindow();
    let tilt = event.tilt();
    PenSample {
        position: point(px(location.x as f32), window_height - px(location.y as f32)),
        pressure: if in_contact {
            event.pressure().clamp(0., 1.)
        } else {
            0.
        },
        tilt: (tool.capabilities & CAPABILITY_TILT != 0).then(|| {
            point(
                (tilt.x * MAX_TILT_DEGREES) as f32,
                (-tilt.y * MAX_TILT_DEGREES) as f32,
            )
        }),
        rotation: (tool.capabilities & CAPABILITY_ROTATION != 0).then(|| event.rotation()),
        wheel: (tool.capabilities & CAPABILITY_TANGENTIAL_PRESSURE != 0)
            .then(|| event.tangentialPressure()),
        distance: None,
        timestamp: Duration::from_secs_f64(event.timestamp()),
        provisional: Default::default(),
        id: None,
    }
}
