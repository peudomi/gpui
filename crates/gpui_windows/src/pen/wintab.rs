use std::time::Duration;

use gpui::{PenDeviceId, PenPhase, PenSample, PenTip, Point, point, px};
use smallvec::SmallVec;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use wintab::{Axis, Context, ContextOptions, Message, Orientation, Wintab};

use super::Frame;

wintab::packet! {
    struct Sample {
        status, time, cursor, buttons, x, y, normal_pressure, tangent_pressure, orientation
    }
}

const TIP_BUTTON: u8 = 0;

#[derive(Default)]
struct Axes {
    pressure: Option<Axis>,
    wheel: Option<Axis>,
    azimuth: Option<Axis>,
    altitude: Option<Axis>,
    twist: Option<Axis>,
}

impl Axes {
    fn query(wintab: &Wintab) -> Self {
        let Some(device) = wintab.device(0) else {
            return Self::default();
        };
        let [azimuth, altitude, twist] =
            device.orientation.map_or([None; 3], |axes| axes.map(Some));
        Self {
            pressure: device.pressure,
            wheel: device.tangent_pressure,
            azimuth,
            altitude,
            twist,
        }
    }

    fn tilt(&self, orientation: Orientation) -> Option<Point<f32>> {
        let azimuth = self.azimuth?.degrees(orientation.azimuth)?.to_radians();
        let altitude = self.altitude?.degrees(orientation.altitude)?.to_radians();
        let horizontal = altitude.cos();
        Some(point(
            (azimuth.sin() * horizontal)
                .atan2(altitude.sin())
                .to_degrees() as f32,
            (-azimuth.cos() * horizontal)
                .atan2(altitude.sin())
                .to_degrees() as f32,
        ))
    }
}

pub(super) struct WintabSource {
    wintab: Wintab,
    context: Context<Sample>,
    axes: Axes,
    packets: Vec<Sample>,
    in_proximity: bool,
    in_contact: bool,
    cursor: Option<(u32, Option<PenDeviceId>)>,
}

impl WintabSource {
    pub fn open(wintab: &Wintab, hwnd: HWND) -> wintab::Result<Self> {
        let context = wintab.open::<Sample>(hwnd, &ContextOptions::new())?;
        Ok(Self {
            wintab: wintab.clone(),
            context,
            axes: Axes::query(wintab),
            packets: Vec::new(),
            in_proximity: false,
            in_contact: false,
            cursor: None,
        })
    }

    pub fn activate(&self, active: bool) {
        self.context.overlap(active).ok();
    }

    pub fn message(
        &mut self,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        origin: POINT,
        scale_factor: f32,
    ) -> Option<SmallVec<[Frame; 2]>> {
        Some(match self.context.message(msg, wparam, lparam)? {
            Message::Packet { .. } | Message::CursorChange { .. } => {
                self.poll(origin, scale_factor)
            }
            Message::Proximity {
                entering: false, ..
            } => {
                let mut frames = self.poll(origin, scale_factor);
                frames.extend(self.leave());
                frames
            }
            Message::Closed { .. } => self.leave(),
            Message::Update { .. } => {
                self.context.refresh();
                SmallVec::new()
            }
            Message::InfoChange { .. } => {
                self.context.refresh();
                self.axes = Axes::query(&self.wintab);
                SmallVec::new()
            }
            _ => SmallVec::new(),
        })
    }

    pub fn poll(&mut self, origin: POINT, scale_factor: f32) -> SmallVec<[Frame; 2]> {
        let mut packets = std::mem::take(&mut self.packets);
        packets.clear();
        self.context.drain(&mut packets);

        let mut frames: SmallVec<[Frame; 2]> = SmallVec::new();
        for packet in &packets {
            if packet.status.out_of_context() {
                frames.extend(self.leave());
                continue;
            }
            let in_contact = packet.buttons.is_pressed(TIP_BUTTON);
            let sample = self.sample(packet, in_contact, origin, scale_factor);
            let device_id = self.device_id(packet.cursor);
            let tip = if packet.status.inverted() {
                PenTip::Eraser
            } else {
                PenTip::Tip
            };
            let barrel = packet.buttons.0 & !(1 << TIP_BUTTON) != 0;
            let frame = |phase| Frame {
                phase,
                in_contact,
                tip,
                barrel,
                device_id,
                samples: smallvec::smallvec![sample],
            };

            if !self.in_proximity {
                self.in_proximity = true;
                frames.push(frame(PenPhase::Entered));
            }
            if in_contact != self.in_contact {
                self.in_contact = in_contact;
                frames.push(frame(if in_contact {
                    PenPhase::Down
                } else {
                    PenPhase::Up
                }));
                continue;
            }
            match frames.last_mut() {
                Some(last)
                    if last.phase == PenPhase::Moved
                        && last.tip == tip
                        && last.barrel == barrel =>
                {
                    last.samples.push(sample);
                }
                _ => frames.push(frame(PenPhase::Moved)),
            }
        }
        self.packets = packets;
        frames
    }

    fn leave(&mut self) -> SmallVec<[Frame; 2]> {
        let mut frames = SmallVec::new();
        if !self.in_proximity {
            return frames;
        }
        let frame = |phase| Frame {
            phase,
            in_contact: false,
            tip: PenTip::Tip,
            barrel: false,
            device_id: self.cursor.and_then(|(_, id)| id),
            samples: SmallVec::new(),
        };
        if self.in_contact {
            frames.push(frame(PenPhase::Cancelled));
        }
        frames.push(frame(PenPhase::Exited));
        self.in_proximity = false;
        self.in_contact = false;
        frames
    }

    fn device_id(&mut self, cursor: u32) -> Option<PenDeviceId> {
        match self.cursor {
            Some((index, id)) if index == cursor => id,
            _ => {
                let id = self
                    .wintab
                    .cursor(cursor)
                    .filter(|cursor| cursor.physical_id != 0)
                    .map(|cursor| {
                        PenDeviceId((cursor.kind as u64) << 32 | cursor.physical_id as u64)
                    });
                self.cursor = Some((cursor, id));
                id
            }
        }
    }

    fn sample(
        &self,
        packet: &Sample,
        in_contact: bool,
        origin: POINT,
        scale_factor: f32,
    ) -> PenSample {
        let (x, y) = self.context.to_screen(packet.x, packet.y);
        let scale_factor = scale_factor as f64;
        let orientation = packet.orientation;
        PenSample {
            position: point(
                px(((x - origin.x as f64) / scale_factor) as f32),
                px(((y - origin.y as f64) / scale_factor) as f32),
            ),
            pressure: match (in_contact, self.axes.pressure) {
                (false, _) => 0.,
                (true, Some(axis)) => axis.normalize(packet.normal_pressure as i32),
                (true, None) => 1.,
            },
            tilt: self.axes.tilt(orientation),
            rotation: self
                .axes
                .twist
                .and_then(|axis| axis.degrees(orientation.twist))
                .map(|degrees| degrees as f32),
            wheel: self
                .axes
                .wheel
                .map(|axis| axis.normalize(packet.tangent_pressure as i32)),
            distance: None,
            timestamp: Duration::from_millis(packet.time as u64),
            provisional: Default::default(),
            id: None,
        }
    }
}
