mod pointer;
mod wintab;

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use gpui::{PenDeviceId, PenEvent, PenPhase, PenSample, PenSource, PenTip, TouchId};
use gpui_util::ResultExt;
use smallvec::SmallVec;
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, WPARAM},
    Graphics::Gdi::ClientToScreen,
    UI::{
        Controls::{
            FEEDBACK_PEN_BARRELVISUALIZATION, FEEDBACK_PEN_DOUBLETAP, FEEDBACK_PEN_PRESSANDHOLD,
            FEEDBACK_PEN_RIGHTTAP, FEEDBACK_PEN_TAP, SetWindowFeedbackSetting,
        },
        WindowsAndMessaging::{WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERUP},
    },
};
use windows::core::BOOL;

use crate::current_modifiers;
use pointer::PointerSource;
use wintab::WintabSource;

pub(crate) const WM_TABLET_QUERYSYSTEMGESTURESTATUS: u32 = 0x02CC;

const TABLET_DISABLE_PRESSANDHOLD: u32 = 0x0000_0001;
const TABLET_DISABLE_PENTAPFEEDBACK: u32 = 0x0000_0008;
const TABLET_DISABLE_PENBARRELFEEDBACK: u32 = 0x0000_0010;
const TABLET_DISABLE_FLICKS: u32 = 0x0001_0000;

pub(crate) const DISABLED_SYSTEM_GESTURES: isize = (TABLET_DISABLE_PRESSANDHOLD
    | TABLET_DISABLE_PENTAPFEEDBACK
    | TABLET_DISABLE_PENBARRELFEEDBACK
    | TABLET_DISABLE_FLICKS) as isize;

pub(crate) use ::wintab::sys::{WT_DEFBASE, WT_MAX};

#[derive(Default)]
pub(crate) struct PenSettings {
    requested: Cell<PenSource>,
    wintab: OnceCell<Option<::wintab::Wintab>>,
}

impl PenSettings {
    pub(crate) fn set_requested(&self, source: PenSource) {
        self.requested.set(source);
    }

    pub(crate) fn active(&self) -> PenSource {
        match self.requested.get() {
            PenSource::WindowsInk => PenSource::WindowsInk,
            PenSource::Auto | PenSource::Wintab if self.wintab().is_some() => PenSource::Wintab,
            _ => PenSource::WindowsInk,
        }
    }

    fn wintab(&self) -> Option<&::wintab::Wintab> {
        self.wintab
            .get_or_init(|| match ::wintab::Wintab::load() {
                Ok(wintab) => Some(wintab),
                Err(error) => {
                    log::info!("Wintab unavailable, using Windows Ink: {error}");
                    None
                }
            })
            .as_ref()
    }
}

pub(crate) enum PointerAction {
    Dispatch(PenEvent),
    Consume,
    Default,
}

struct Frame {
    phase: PenPhase,
    in_contact: bool,
    tip: PenTip,
    barrel: bool,
    device_id: Option<PenDeviceId>,
    samples: SmallVec<[PenSample; 8]>,
}

pub(crate) struct PenInput {
    settings: Rc<PenSettings>,
    state: RefCell<PenState>,
}

#[derive(Default)]
struct PenState {
    pointer: PointerSource,
    wintab: Option<WintabSource>,
    next_id: u64,
    stroke: Option<TouchId>,
    hover: Option<TouchId>,
    last_sample: Option<PenSample>,
    stroke_claimed: bool,
    pointer_consuming: bool,
    mouse_consuming: bool,
}

impl PenState {
    fn issue_id(&mut self) -> TouchId {
        self.next_id += 1;
        TouchId(self.next_id)
    }

    fn event(&mut self, frame: Frame) -> PenEvent {
        let id = match frame.phase {
            PenPhase::Down => {
                let id = self.issue_id();
                self.stroke = Some(id);
                id
            }
            _ if frame.in_contact || self.stroke.is_some() => match self.stroke {
                Some(id) => id,
                None => {
                    let id = self.issue_id();
                    self.stroke = Some(id);
                    id
                }
            },
            PenPhase::Entered => {
                let id = self.issue_id();
                self.hover = Some(id);
                id
            }
            _ => match self.hover {
                Some(id) => id,
                None => {
                    let id = self.issue_id();
                    self.hover = Some(id);
                    id
                }
            },
        };
        match frame.phase {
            PenPhase::Up | PenPhase::Cancelled => self.stroke = None,
            PenPhase::Exited => {
                self.stroke = None;
                self.hover = None;
            }
            _ => {}
        }

        let mut samples = frame.samples;
        if samples.is_empty() {
            samples.extend(self.last_sample);
        }
        self.last_sample = samples.last().copied();

        PenEvent {
            id,
            device_id: frame.device_id,
            phase: frame.phase,
            tip: frame.tip,
            barrel: frame.barrel,
            in_contact: frame.in_contact,
            modifiers: current_modifiers(),
            samples,
            predicted: Default::default(),
        }
    }
}

impl PenInput {
    pub(crate) fn new(hwnd: HWND, settings: Rc<PenSettings>) -> Self {
        let disabled = BOOL(0);
        for feedback in [
            FEEDBACK_PEN_BARRELVISUALIZATION,
            FEEDBACK_PEN_TAP,
            FEEDBACK_PEN_DOUBLETAP,
            FEEDBACK_PEN_PRESSANDHOLD,
            FEEDBACK_PEN_RIGHTTAP,
        ] {
            let _ = unsafe {
                SetWindowFeedbackSetting(
                    hwnd,
                    feedback,
                    0,
                    size_of::<BOOL>() as u32,
                    Some(&disabled as *const BOOL as _),
                )
            };
        }
        let input = Self {
            settings,
            state: RefCell::default(),
        };
        input.apply_source(hwnd);
        input
    }

    pub(crate) fn apply_source(&self, hwnd: HWND) {
        let mut state = self.state.borrow_mut();
        let wanted = self.settings.active() == PenSource::Wintab;
        if wanted == state.wintab.is_some() {
            return;
        }
        state.wintab = if wanted {
            self.settings
                .wintab()
                .and_then(|wintab| WintabSource::open(wintab, hwnd).log_err())
        } else {
            None
        };
        state.stroke_claimed = false;
    }

    pub(crate) fn activate(&self, active: bool) {
        if let Some(wintab) = &self.state.borrow().wintab {
            wintab.activate(active);
        }
    }

    pub(crate) fn pointer(
        &self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        scale_factor: f32,
    ) -> Option<PointerAction> {
        let mut state = self.state.borrow_mut();
        if state.wintab.is_none() {
            let frame = state.pointer.read(hwnd, msg, wparam, scale_factor)?;
            return Some(PointerAction::Dispatch(state.event(frame)));
        }
        if !PointerSource::is_pen(wparam) {
            return None;
        }
        let consuming = match msg {
            WM_POINTERDOWN => {
                state.pointer_consuming = state.stroke_claimed;
                state.pointer_consuming
            }
            WM_POINTERUP | WM_POINTERCAPTURECHANGED => std::mem::take(&mut state.pointer_consuming),
            _ => state.pointer_consuming,
        };
        Some(if consuming {
            PointerAction::Consume
        } else {
            PointerAction::Default
        })
    }

    pub(crate) fn wintab_message(
        &self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        scale_factor: f32,
    ) -> Option<SmallVec<[PenEvent; 2]>> {
        let mut state = self.state.borrow_mut();
        let origin = client_origin(hwnd)?;
        let frames = state
            .wintab
            .as_mut()?
            .message(msg, wparam, lparam, origin, scale_factor)?;
        Some(frames.into_iter().map(|frame| state.event(frame)).collect())
    }

    // Call before handling a pointer or mouse message, or its stroke's claim is not known yet.
    pub(crate) fn poll_wintab(&self, hwnd: HWND, scale_factor: f32) -> SmallVec<[PenEvent; 2]> {
        let mut state = self.state.borrow_mut();
        let (Some(wintab), Some(origin)) = (state.wintab.as_mut(), client_origin(hwnd)) else {
            return SmallVec::new();
        };
        let frames = wintab.poll(origin, scale_factor);
        frames.into_iter().map(|frame| state.event(frame)).collect()
    }

    pub(crate) fn dispatched(&self, phase: PenPhase, in_contact: bool, claimed: bool) {
        let mut state = self.state.borrow_mut();
        if in_contact || phase == PenPhase::Down {
            state.stroke_claimed = claimed;
        } else if matches!(phase, PenPhase::Up | PenPhase::Cancelled | PenPhase::Exited) {
            state.stroke_claimed = false;
        }
    }

    pub(crate) fn consumes_mouse(&self, down: bool, up: bool) -> bool {
        let mut state = self.state.borrow_mut();
        if state.wintab.is_none() {
            return false;
        }
        if down {
            state.mouse_consuming = state.stroke_claimed;
        }
        let consuming = state.mouse_consuming;
        if up {
            state.mouse_consuming = false;
        }
        consuming
    }
}

fn client_origin(hwnd: HWND) -> Option<POINT> {
    let mut origin = POINT::default();
    unsafe { ClientToScreen(hwnd, &mut origin) }
        .as_bool()
        .then_some(origin)
}
