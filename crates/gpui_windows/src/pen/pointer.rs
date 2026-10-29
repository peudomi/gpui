use std::time::Duration;

use collections::HashMap;
use gpui::{PenPhase, PenSample, PenTip, point, px};
use smallvec::SmallVec;
use windows::Win32::{
    Foundation::{HANDLE, HWND, POINT, RECT, WPARAM},
    Graphics::Gdi::ClientToScreen,
    System::Performance::QueryPerformanceFrequency,
    UI::{
        Input::Pointer::{
            GetPointerDeviceRects, GetPointerPenInfoHistory, GetPointerType, POINTER_FLAG_CANCELED,
            POINTER_FLAG_INCONTACT, POINTER_PEN_INFO,
        },
        WindowsAndMessaging::*,
    },
};

use super::Frame;
use crate::HiLoWord;

const MAX_PIXEL_ERROR: f64 = 2.;

#[derive(Clone, Copy)]
struct HimetricToScreen {
    scale_x: f64,
    offset_x: f64,
    scale_y: f64,
    offset_y: f64,
}

impl HimetricToScreen {
    fn query(device: HANDLE) -> Option<Self> {
        let mut device_rect = RECT::default();
        let mut display_rect = RECT::default();
        unsafe { GetPointerDeviceRects(device, &mut device_rect, &mut display_rect) }.ok()?;
        let device_width = (device_rect.right - device_rect.left) as f64;
        let device_height = (device_rect.bottom - device_rect.top) as f64;
        if device_width <= 0. || device_height <= 0. {
            return None;
        }
        let scale_x = (display_rect.right - display_rect.left) as f64 / device_width;
        let scale_y = (display_rect.bottom - display_rect.top) as f64 / device_height;
        Some(Self {
            scale_x,
            offset_x: display_rect.left as f64 - scale_x * device_rect.left as f64,
            scale_y,
            offset_y: display_rect.top as f64 - scale_y * device_rect.top as f64,
        })
    }

    fn apply(&self, himetric: POINT) -> (f64, f64) {
        (
            self.scale_x * himetric.x as f64 + self.offset_x,
            self.scale_y * himetric.y as f64 + self.offset_y,
        )
    }
}

pub(super) struct PointerSource {
    devices: HashMap<isize, Option<HimetricToScreen>>,
    ticks_per_second: f64,
}

impl Default for PointerSource {
    fn default() -> Self {
        let mut frequency = 0;
        unsafe { QueryPerformanceFrequency(&mut frequency) }.ok();
        Self {
            devices: HashMap::default(),
            ticks_per_second: frequency as f64,
        }
    }
}

impl PointerSource {
    pub fn is_pen(wparam: WPARAM) -> bool {
        let mut pointer_type = POINTER_INPUT_TYPE::default();
        unsafe { GetPointerType(wparam.loword() as u32, &mut pointer_type) }.is_ok()
            && pointer_type == PT_PEN
    }

    pub fn read(
        &mut self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        scale_factor: f32,
    ) -> Option<Frame> {
        if !Self::is_pen(wparam) {
            return None;
        }
        let pointer_id = wparam.loword() as u32;

        let history = read_history(pointer_id);
        let Some(current) = history.last().copied() else {
            return (msg == WM_POINTERCAPTURECHANGED).then(|| Frame {
                phase: PenPhase::Cancelled,
                in_contact: false,
                tip: PenTip::Tip,
                barrel: false,
                device_id: None,
                samples: SmallVec::new(),
            });
        };
        let flags = current.pointerInfo.pointerFlags;
        let phase = match msg {
            _ if flags.contains(POINTER_FLAG_CANCELED) => PenPhase::Cancelled,
            WM_POINTERCAPTURECHANGED => PenPhase::Cancelled,
            WM_POINTERENTER => PenPhase::Entered,
            WM_POINTERLEAVE => PenPhase::Exited,
            WM_POINTERDOWN => PenPhase::Down,
            WM_POINTERUP => PenPhase::Up,
            _ => PenPhase::Moved,
        };
        let device = current.pointerInfo.sourceDevice;
        if phase == PenPhase::Entered {
            self.devices.remove(&(device.0 as isize));
        }
        let mapping = *self
            .devices
            .entry(device.0 as isize)
            .or_insert_with(|| HimetricToScreen::query(device));

        let mut origin = POINT::default();
        unsafe { ClientToScreen(hwnd, &mut origin) }.ok().ok()?;
        let samples = history
            .iter()
            .map(|info| self.sample(info, mapping, origin, scale_factor))
            .collect();

        Some(Frame {
            phase,
            in_contact: flags.contains(POINTER_FLAG_INCONTACT),
            tip: if current.penFlags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0 {
                PenTip::Eraser
            } else {
                PenTip::Tip
            },
            barrel: current.penFlags & PEN_FLAG_BARREL != 0,
            device_id: None,
            samples,
        })
    }

    fn sample(
        &self,
        info: &POINTER_PEN_INFO,
        mapping: Option<HimetricToScreen>,
        origin: POINT,
        scale_factor: f32,
    ) -> PenSample {
        let pixel = info.pointerInfo.ptPixelLocation;
        let (x, y) = mapping
            .map(|mapping| mapping.apply(info.pointerInfo.ptHimetricLocation))
            .filter(|(x, y)| {
                (x - pixel.x as f64).abs() <= MAX_PIXEL_ERROR
                    && (y - pixel.y as f64).abs() <= MAX_PIXEL_ERROR
            })
            .unwrap_or((pixel.x as f64, pixel.y as f64));
        let scale_factor = scale_factor as f64;
        let in_contact = info
            .pointerInfo
            .pointerFlags
            .contains(POINTER_FLAG_INCONTACT);

        PenSample {
            position: point(
                px(((x - origin.x as f64) / scale_factor) as f32),
                px(((y - origin.y as f64) / scale_factor) as f32),
            ),
            pressure: match (in_contact, info.penMask & PEN_MASK_PRESSURE != 0) {
                (false, _) => 0.,
                (true, true) => (info.pressure as f32 / 1024.).clamp(0., 1.),
                (true, false) => 1.,
            },
            tilt: (info.penMask & (PEN_MASK_TILT_X | PEN_MASK_TILT_Y) != 0)
                .then(|| point(info.tiltX as f32, info.tiltY as f32)),
            rotation: (info.penMask & PEN_MASK_ROTATION != 0).then_some(info.rotation as f32),
            wheel: None,
            distance: None,
            timestamp: self.timestamp(info),
            provisional: Default::default(),
            id: None,
        }
    }

    fn timestamp(&self, info: &POINTER_PEN_INFO) -> Duration {
        let count = info.pointerInfo.PerformanceCount;
        if count != 0 && self.ticks_per_second > 0. {
            Duration::from_secs_f64(count as f64 / self.ticks_per_second)
        } else {
            Duration::from_millis(info.pointerInfo.dwTime as u64)
        }
    }
}

fn read_history(pointer_id: u32) -> Vec<POINTER_PEN_INFO> {
    let mut count = 0;
    if unsafe { GetPointerPenInfoHistory(pointer_id, &mut count, None) }.is_err() || count == 0 {
        return Vec::new();
    }
    let mut history = vec![POINTER_PEN_INFO::default(); count as usize];
    if unsafe { GetPointerPenInfoHistory(pointer_id, &mut count, Some(history.as_mut_ptr())) }
        .is_err()
    {
        return Vec::new();
    }
    history.truncate(count as usize);
    history.reverse();
    history
}
