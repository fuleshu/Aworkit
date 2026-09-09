//! GDI capture in physical desktop coordinates. Windows must remain visible and
//! unobscured: the returned pixels are what is currently displayed, not a hidden
//! window render. Handles are re-enumerated before capture; no input is sent.
use super::Capture;
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    io::Cursor,
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::*,
        },
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
    core::BOOL,
};

struct Target {
    id: String,
    title: String,
    kind: &'static str,
    rect: RECT,
}
impl Target {
    fn value(&self) -> Value {
        json!({"target":self.id,"title":self.title,"kind":self.kind,"x":self.rect.left,"y":self.rect.top,"width":self.rect.right-self.rect.left,"height":self.rect.bottom-self.rect.top})
    }
}

/// Restore the caller's DPI awareness even if enumeration/capture fails.
struct Dpi(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);
impl Dpi {
    fn physical() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for Dpi {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}

unsafe extern "system" fn monitor(
    handle: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let targets = unsafe { &mut *(data as *mut Vec<Target>) };
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(handle, &mut info as *mut _ as *mut MONITORINFO) } != 0 {
        let end = info
            .szDevice
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(info.szDevice.len());
        let device = String::from_utf16_lossy(&info.szDevice[..end]);
        targets.push(Target {
            id: format!("monitor:{device}"),
            title: format!(
                "{device}{}",
                if info.monitorInfo.dwFlags & 1 != 0 {
                    " (primary)"
                } else {
                    ""
                }
            ),
            kind: "monitor",
            rect: info.monitorInfo.rcMonitor,
        });
    }
    1
}
unsafe extern "system" fn window(handle: HWND, data: LPARAM) -> BOOL {
    if unsafe { IsWindowVisible(handle) } == 0 || unsafe { IsIconic(handle) } != 0 {
        return 1;
    }
    let mut cloaked = 0_u32;
    if unsafe {
        DwmGetWindowAttribute(
            handle,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    } >= 0
        && cloaked != 0
    {
        return 1;
    }
    let mut title = [0u16; 256];
    let n = unsafe { GetWindowTextW(handle, title.as_mut_ptr(), title.len() as i32) };
    if n <= 0 {
        return 1;
    }
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(handle, &mut pid);
    }
    if unsafe { GetWindowRect(handle, &mut rect) } == 0
        || rect.right <= rect.left
        || rect.bottom <= rect.top
    {
        return 1;
    }
    // DWM excludes the invisible resize border, which otherwise leaks pixels
    // from neighboring apps into an unobscured selected-window capture.
    let mut visible: RECT = unsafe { std::mem::zeroed() };
    if unsafe {
        DwmGetWindowAttribute(
            handle,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut visible as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    } >= 0
        && visible.right > visible.left
        && visible.bottom > visible.top
    {
        rect = visible;
    }
    let targets = unsafe { &mut *(data as *mut Vec<Target>) };
    targets.push(Target {
        id: format!("window:{pid}:{}", handle as usize),
        title: String::from_utf16_lossy(&title[..n as usize]),
        kind: "window",
        rect,
    });
    (targets.len() < 256) as BOOL
}
fn targets() -> Result<Vec<Target>, String> {
    let mut targets = Vec::<Target>::new();
    if unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(monitor),
            &mut targets as *mut _ as LPARAM,
        )
    } == 0
    {
        return Err("Cannot enumerate screenshot monitors".into());
    }
    unsafe {
        EnumWindows(Some(window), &mut targets as *mut _ as LPARAM);
    }
    Ok(targets)
}
pub(crate) fn list() -> Result<Value, String> {
    let _dpi = Dpi::physical();
    let targets = targets()?;
    Ok(
        json!({"targets":targets.iter().map(Target::value).collect::<Vec<_>>(),"truncated":targets.len() >= 256,"captureMode":"visible_desktop","note":"Keep the selected target visible and unobscured. Coordinates are physical desktop pixels."}),
    )
}

/// Own every GDI resource before the next fallible operation.
struct Surface {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}
pub(crate) fn capture(id: &str) -> Result<Capture, String> {
    let _dpi = Dpi::physical();
    let target = targets()?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or("Screenshot target is no longer available; list targets again")?;
    let mut rect = target.rect;
    // Clip partially offscreen windows rather than returning uninitialized pixels.
    unsafe {
        rect.left = rect.left.max(GetSystemMetrics(SM_XVIRTUALSCREEN));
        rect.top = rect.top.max(GetSystemMetrics(SM_YVIRTUALSCREEN));
        rect.right = rect
            .right
            .min(GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN));
        rect.bottom = rect
            .bottom
            .min(GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN));
    }
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    if width <= 0
        || height <= 0
        || width > 8000
        || height > 8000
        || i64::from(width) * i64::from(height) * 4 > 256 * 1024 * 1024
    {
        return Err("Screenshot target is offscreen or exceeds image dimensions".into());
    }
    let mut surface = Surface {
        screen: unsafe { GetDC(null_mut()) },
        memory: null_mut(),
        bitmap: null_mut(),
        previous: null_mut(),
    };
    if surface.screen.is_null() {
        return Err("Desktop capture is unavailable in this Windows session".into());
    }
    surface.memory = unsafe { CreateCompatibleDC(surface.screen) };
    if surface.memory.is_null() {
        return Err("Cannot create screenshot surface".into());
    }
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { std::mem::zeroed() }
    };
    let mut pixels: *mut c_void = null_mut();
    surface.bitmap = unsafe {
        CreateDIBSection(
            surface.screen,
            &info,
            DIB_RGB_COLORS,
            &mut pixels,
            null_mut(),
            0,
        )
    };
    if surface.bitmap.is_null() || pixels.is_null() {
        return Err("Cannot allocate screenshot bitmap".into());
    }
    surface.previous = unsafe { SelectObject(surface.memory, surface.bitmap) };
    if surface.previous.is_null() {
        return Err("Cannot select screenshot bitmap".into());
    }
    if unsafe {
        BitBlt(
            surface.memory,
            0,
            0,
            width,
            height,
            surface.screen,
            rect.left,
            rect.top,
            SRCCOPY | CAPTUREBLT,
        )
    } == 0
    {
        return Err("Windows could not capture this target".into());
    }
    if unsafe { GdiFlush() } == 0 {
        return Err("Windows could not finish screenshot capture".into());
    }
    let mut rgba = unsafe {
        std::slice::from_raw_parts(pixels.cast::<u8>(), width as usize * height as usize * 4)
    }
    .to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    let bitmap = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or("Invalid screenshot size")?;
    let mut encoded = Cursor::new(Vec::new());
    bitmap
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut source = target.value();
    source["captureMode"] = json!("visible_desktop");
    source["x"] = json!(rect.left);
    source["y"] = json!(rect.top);
    source["width"] = json!(width);
    source["height"] = json!(height);
    Ok(Capture {
        bytes: encoded.into_inner(),
        source,
    })
}
