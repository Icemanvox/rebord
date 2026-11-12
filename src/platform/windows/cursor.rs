//! Le pointeur de Windows : position, forme visible, et image de cette forme.

use crate::cursor::{compose, crop, visible_box};
use crate::platform::{CursorImage, CursorState, rgba_png};
use std::ffi::c_void;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetMonitorInfoW,
    GetObjectW, HBITMAP, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ReleaseDC,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, HICON, ICONINFO,
};

pub fn state() -> Option<CursorState> {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut ci).ok()? };
    Some(CursorState {
        x: ci.ptScreenPos.x as f64,
        y: ci.ptScreenPos.y as f64,
        visible: ci.flags.0 & CURSOR_SHOWING.0 != 0 && !ci.hCursor.is_invalid(),
        shape: ci.hCursor.0 as usize as u64,
    })
}

/// Les pixels d'un bitmap en BGRA 32 bits, de haut en bas.
fn pixels(hbm: HBITMAP) -> Option<(usize, usize, Vec<u8>)> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(hbm.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void))
            == 0
        {
            return None;
        }
        let (w, h) = (bm.bmWidth.max(0) as usize, bm.bmHeight.max(0) as usize);
        if w == 0 || h == 0 || w > 512 || h > 1024 {
            return None;
        }
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; w * h * 4];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbm, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        (lines > 0).then_some((w, h, buf))
    }
}

/// L'échelle de l'écran sous le pointeur (DPI / 96), pour donner la taille en points.
fn scale_at(x: i32, y: i32) -> f64 {
    unsafe {
        let m = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_err() {
            return 1.0;
        }
        (dx as f64 / 96.0).max(0.5)
    }
}

pub fn image() -> Option<CursorImage> {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut ci).ok()? };
    if ci.hCursor.is_invalid() {
        return None;
    }
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(HICON(ci.hCursor.0), &mut ii).ok()? };
    let mask = pixels(ii.hbmMask);
    let color = if ii.hbmColor.is_invalid() { None } else { pixels(ii.hbmColor) };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
        }
    }
    let (mw, mh, mask) = mask?;
    let (w, h, rgba) = match &color {
        Some((cw, ch, c)) => (*cw, *ch, compose(*cw, *ch, Some(c), &mask)),
        None => (mw, mh / 2, compose(mw, mh / 2, None, &mask)),
    };
    let b = visible_box(w, h, &rgba)?;
    let cropped = crop(w, &rgba, b);
    let png = rgba_png(b.2 as u32, b.3 as u32, &cropped)?;
    let s = scale_at(ci.ptScreenPos.x, ci.ptScreenPos.y) as f32;
    Some(CursorImage {
        hot_x: (ii.xHotspot as f32 - b.0 as f32) / s,
        hot_y: (ii.yHotspot as f32 - b.1 as f32) / s,
        width: b.2 as f32 / s,
        height: b.3 as f32 / s,
        png,
    })
}
