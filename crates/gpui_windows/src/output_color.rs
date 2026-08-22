//! Per-monitor output color handling for the DirectX renderer.
//!
//! Mirrors Chromium's approach on Windows (`ui/display/win/screen_win.cc`,
//! `ui/display/win/color_profile_reader.cc`):
//!
//! - When the window's monitor has advanced color enabled (HDR or Auto Color
//!   Management), present scene-referred scRGB through an fp16 swap chain and
//!   let the OS map it to the panel.
//! - Otherwise present display-referred 8-bit values, converting with a 3x3
//!   matrix derived from the monitor ICC profile's primaries. Like Chromium
//!   (`ICCProfile::GetPrimariesOnlyColorSpace`), the profile's TRC is ignored
//!   and an sRGB transfer is assumed: real-world profiles frequently ship
//!   broken curves, per-channel curves cause banding in 8-bit output, and the
//!   calibration part of a profile is already applied by the OS via the GPU
//!   gamma ramp (vcgt).

use windows::{
    Win32::{
        Devices::Display::{
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
            DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
            DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
            QueryDisplayConfig,
        },
        Foundation::HWND,
        Graphics::Gdi::{
            CreateDCW, DeleteDC, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
            MONITORINFOEXW, MonitorFromWindow,
        },
        UI::ColorSystem::GetICMProfileW,
    },
    core::PCWSTR,
};

/// How the final present pass hands pixels to the compositor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OutputColorMode {
    /// Advanced color: fp16 swap chain tagged scRGB; the OS color-manages.
    ScRgb,
    /// SDR: 8-bit swap chain holding display-referred values. `matrix` maps
    /// linear sRGB to the monitor's gamut (rows; the .w lanes are unused).
    DisplayReferred { matrix: [[f32; 4]; 3] },
}

pub(crate) const IDENTITY_MATRIX: [[f32; 4]; 3] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

pub(crate) fn resolve_output_color_mode(hwnd: HWND) -> (HMONITOR, OutputColorMode) {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    (monitor, monitor_color_mode(monitor))
}

/// The output color mode for a monitor. `GPUI_COLOR_MODE=scrgb|srgb` overrides
/// the detection for debugging (`srgb` means display-referred with an
/// identity matrix, i.e. the pre-wide-gamut behavior).
pub(crate) fn monitor_color_mode(monitor: HMONITOR) -> OutputColorMode {
    match std::env::var("GPUI_COLOR_MODE").as_deref() {
        Ok("scrgb") => {
            return OutputColorMode::ScRgb;
        }
        Ok("srgb") => {
            return OutputColorMode::DisplayReferred {
                matrix: IDENTITY_MATRIX,
            };
        }
        _ => {}
    }
    let mut monitor_info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    monitor_info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe {
        GetMonitorInfoW(
            monitor,
            &mut monitor_info as *mut MONITORINFOEXW as *mut MONITORINFO,
        )
    };
    if !ok.as_bool() {
        log::warn!("GetMonitorInfoW failed; assuming an sRGB monitor");
        return OutputColorMode::DisplayReferred {
            matrix: IDENTITY_MATRIX,
        };
    }

    if advanced_color_enabled(&monitor_info.szDevice) {
        return OutputColorMode::ScRgb;
    }

    let matrix = monitor_icc_matrix(&monitor_info.szDevice).unwrap_or(IDENTITY_MATRIX);
    OutputColorMode::DisplayReferred { matrix }
}

/// Whether the monitor with the given GDI device name (e.g. `\\.\DISPLAY1`)
/// has advanced color (HDR or Auto Color Management) enabled.
fn advanced_color_enabled(gdi_device_name: &[u16; 32]) -> bool {
    unsafe {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            .is_err()
        {
            return false;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return false;
        }

        for path in &paths[..path_count as usize] {
            let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source_name.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source_name.header.adapterId = path.sourceInfo.adapterId;
            source_name.header.id = path.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source_name.header) != 0 {
                continue;
            }
            if source_name.viewGdiDeviceName != *gdi_device_name {
                continue;
            }

            let mut color_info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();
            color_info.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
            color_info.header.size =
                std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32;
            color_info.header.adapterId = path.targetInfo.adapterId;
            color_info.header.id = path.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut color_info.header) != 0 {
                continue;
            }
            // Bit 0: advancedColorSupported, bit 1: advancedColorEnabled.
            return color_info.Anonymous.value & 0x2 != 0;
        }
    }
    false
}

/// Reads the monitor's ICC profile and derives the linear `sRGB -> monitor`
/// matrix from its primaries. `None` when there is no usable profile.
fn monitor_icc_matrix(gdi_device_name: &[u16; 32]) -> Option<[[f32; 4]; 3]> {
    let profile_path = monitor_icc_profile_path(gdi_device_name)?;
    let bytes = std::fs::read(&profile_path)
        .map_err(|err| log::warn!("Failed to read ICC profile {profile_path}: {err}"))
        .ok()?;
    let matrix = srgb_to_display_matrix_from_icc(&bytes);
    match &matrix {
        Some(matrix) => log::info!(
            "monitor ICC profile {profile_path}: sRGB->display rows {:?} {:?} {:?}",
            &matrix[0][..3],
            &matrix[1][..3],
            &matrix[2][..3],
        ),
        None => log::warn!("monitor ICC profile {profile_path} has no usable primaries"),
    }
    matrix
}

fn monitor_icc_profile_path(gdi_device_name: &[u16; 32]) -> Option<String> {
    unsafe {
        let hdc = CreateDCW(
            PCWSTR(gdi_device_name.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
        );
        if hdc.is_invalid() {
            return None;
        }
        let mut len = 0u32;
        GetICMProfileW(hdc, &mut len, None).ok();
        let path = if len > 0 {
            let mut buffer = vec![0u16; len as usize];
            let ok = GetICMProfileW(
                hdc,
                &mut len,
                Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            );
            if ok.as_bool() {
                let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
                Some(String::from_utf16_lossy(&buffer[..end]))
            } else {
                None
            }
        } else {
            None
        };
        DeleteDC(hdc).ok();
        path
    }
}

/// The sRGB primaries in the ICC profile connection space (XYZ, D50-adapted),
/// matching the rXYZ/gXYZ/bXYZ tags of a standard sRGB profile.
const SRGB_TO_XYZ_D50: [[f64; 3]; 3] = [
    [0.4360747, 0.3850649, 0.1430804],
    [0.2225045, 0.7168786, 0.0606169],
    [0.0139322, 0.0971045, 0.7141733],
];

/// Parses the rXYZ/gXYZ/bXYZ tags of an ICC profile and returns the
/// `linear sRGB -> linear display` matrix, as rows padded to float4.
fn srgb_to_display_matrix_from_icc(bytes: &[u8]) -> Option<[[f32; 4]; 3]> {
    let r = icc_xyz_tag(bytes, b"rXYZ")?;
    let g = icc_xyz_tag(bytes, b"gXYZ")?;
    let b = icc_xyz_tag(bytes, b"bXYZ")?;
    // Tag values are the columns of display RGB -> XYZ.
    let display_to_xyz = [
        [r[0], g[0], b[0]],
        [r[1], g[1], b[1]],
        [r[2], g[2], b[2]],
    ];
    let xyz_to_display = invert_3x3(&display_to_xyz)?;
    let m = multiply_3x3(&xyz_to_display, &SRGB_TO_XYZ_D50);
    Some([
        [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32, 0.0],
        [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32, 0.0],
        [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32, 0.0],
    ])
}

fn icc_xyz_tag(bytes: &[u8], signature: &[u8; 4]) -> Option<[f64; 3]> {
    let read_u32 = |offset: usize| -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    };
    if bytes.get(36..40)? != b"acsp" {
        return None;
    }
    let tag_count = read_u32(128)? as usize;
    for tag in 0..tag_count.min(1024) {
        let entry = 132 + tag * 12;
        if bytes.get(entry..entry + 4)? != signature {
            continue;
        }
        let offset = read_u32(entry + 4)? as usize;
        if bytes.get(offset..offset + 4)? != b"XYZ " {
            return None;
        }
        let component = |index: usize| -> Option<f64> {
            Some(read_u32(offset + 8 + index * 4)? as i32 as f64 / 65536.0)
        };
        return Some([component(0)?, component(1)?, component(2)?]);
    }
    None
}

fn invert_3x3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ])
}

fn multiply_3x3(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for row in 0..3 {
        for col in 0..3 {
            out[row][col] = (0..3).map(|k| a[row][k] * b[k][col]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_profile_produces_identity() {
        // A display whose primaries equal sRGB's must map through identity.
        let display_to_xyz = SRGB_TO_XYZ_D50;
        let inverse = invert_3x3(&display_to_xyz).unwrap();
        let m = multiply_3x3(&inverse, &SRGB_TO_XYZ_D50);
        for row in 0..3 {
            for col in 0..3 {
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!((m[row][col] - expected).abs() < 1e-6);
            }
        }
    }
}
