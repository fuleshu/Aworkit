//! GDI font ownership and owner-draw geometry. Every selected object is restored
//! before its font/brush can be released, including reentrant menu repaints.
use super::windows::Entry;
use windows_sys::Win32::{
    Foundation::{RECT, SIZE},
    Graphics::Gdi::*,
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::*,
        HiDpi::SystemParametersInfoForDpi,
        WindowsAndMessaging::*,
    },
};

pub struct Font(pub HFONT);
impl Font {
    pub fn new(logical_size: f64, dpi: u32) -> Result<Self, String> {
        // Only query the user's menu face. Never write system-wide font metrics.
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        unsafe {
            if SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
                0,
                96,
            ) == 0
            {
                return Err("cannot read Windows menu font".into());
            }
            metrics.lfMenuFont.lfHeight = -(logical_size * dpi as f64 / 96.0).round() as i32;
            let font = CreateFontIndirectW(&metrics.lfMenuFont);
            if font.is_null() {
                return Err("cannot create scaled Windows menu font".into());
            }
            Ok(Self(font))
        }
    }
}
impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.0);
        }
    }
}

pub fn measure(entry: &Entry, font: HFONT, dpi: u32) -> (u32, u32) {
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let previous = SelectObject(dc, font);
        let mut size = SIZE::default();
        GetTextExtentPoint32W(
            dc,
            entry.name.as_ptr(),
            entry.name.len() as i32 - 1,
            &mut size,
        );
        SelectObject(dc, previous);
        DeleteDC(dc);
        let padding = (if entry.top { 16 } else { 48 }) * dpi / 96;
        (
            (size.cx.max(0) as u32) + padding,
            (size.cy.max(0) as u32) + 8 * dpi / 96,
        )
    }
}

pub fn draw(entry: &Entry, item: &DRAWITEMSTRUCT, font: HFONT, dpi: u32, dark: bool) {
    unsafe {
        let saved = SaveDC(item.hDC);
        SelectObject(item.hDC, font);
        SetBkMode(item.hDC, TRANSPARENT as i32);
        let selected = item.itemState & (ODS_SELECTED | ODS_HOTLIGHT) != 0;
        let disabled = item.itemState & (ODS_DISABLED | ODS_GRAYED) != 0;
        // Forced colors always use system brushes; otherwise match the app theme.
        let mut contrast = HIGHCONTRASTW {
            cbSize: size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            (&mut contrast as *mut HIGHCONTRASTW).cast(),
            0,
        );
        let dark = dark && contrast.dwFlags & HCF_HIGHCONTRASTON == 0;
        let background = if dark {
            if selected { 0x40332a } else { 0x1d1917 }
        } else {
            GetSysColor(if selected {
                COLOR_HIGHLIGHT
            } else if entry.top {
                COLOR_MENUBAR
            } else {
                COLOR_MENU
            })
        };
        let foreground = if dark {
            if disabled { 0xaaaaaa } else { 0xf5f3f1 }
        } else {
            GetSysColor(if disabled {
                COLOR_GRAYTEXT
            } else if selected {
                COLOR_HIGHLIGHTTEXT
            } else {
                COLOR_MENUTEXT
            })
        };
        let brush = CreateSolidBrush(background);
        FillRect(item.hDC, &item.rcItem, brush);
        DeleteObject(brush);
        SetTextColor(item.hDC, foreground);
        let mut rect = item.rcItem;
        let inset = (if entry.top { 8 } else { 24 }) * dpi as i32 / 96;
        rect.left += inset;
        rect.right -= inset;
        let flags = DT_SINGLELINE
            | DT_VCENTER
            | if item.itemState & ODS_NOACCEL != 0 {
                DT_HIDEPREFIX
            } else {
                0
            };
        let parts: Vec<&str> = entry.label.split('\t').collect();
        text(item.hDC, parts[0], rect, flags | DT_LEFT);
        if let Some(accelerator) = parts.get(1) {
            text(item.hDC, accelerator, rect, flags | DT_RIGHT | DT_NOPREFIX);
        }
        if !entry.top && item.itemState & ODS_CHECKED != 0 {
            let mut check = item.rcItem;
            check.right = rect.left;
            text(item.hDC, "✓", check, flags | DT_CENTER);
        }
        if !entry.top && entry.submenu {
            let mut arrow = item.rcItem;
            arrow.left = rect.right;
            text(item.hDC, "›", arrow, flags | DT_CENTER);
        }
        RestoreDC(item.hDC, saved);
    }
}

unsafe fn text(dc: HDC, value: &str, mut rect: RECT, flags: u32) {
    let wide: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        DrawTextW(dc, wide.as_ptr(), wide.len() as i32, &mut rect, flags);
    }
}
