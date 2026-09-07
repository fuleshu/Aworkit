//! Installs one subclass after Tauri's menu subclass. Stable boxed item data
//! exposes MSAA names and is freed with the window, never during a menu callback.
use super::paint::{self, Font};
use std::{
    cell::{Cell, RefCell},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{CreateBitmap, DeleteObject, HBITMAP},
    UI::{
        Accessibility::{MSAA_MENU_SIG, MSAAMENUINFO},
        Controls::*,
        HiDpi::GetDpiForWindow,
        Shell::*,
        WindowsAndMessaging::*,
    },
};
const SUBCLASS: usize = 0x41574d46;

#[repr(C)]
pub(super) struct Entry {
    msaa: MSAAMENUINFO,
    pub name: Vec<u16>,
    pub label: String,
    menu: HMENU,
    position: u32,
    original_type: u32,
    original_data: usize,
    pub top: bool,
    pub submenu: bool,
    bitmap: Cell<HBITMAP>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        let bitmap = self.bitmap.get();
        if !bitmap.is_null() {
            unsafe {
                DeleteObject(bitmap);
            }
        }
    }
}

struct MenuFonts {
    // Boxes are required: MENUITEMINFO and MSAA retain each entry's address.
    entries: Vec<Box<Entry>>,
    font: RefCell<Font>,
    size: Cell<f64>,
    dark: Cell<bool>,
    dpi: Cell<u32>,
    draws: Cell<u32>,
}

impl MenuFonts {
    fn entry(&self, data: usize) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| (&***entry as *const Entry as usize) == data)
            .map(Box::as_ref)
    }
    fn refresh(&self, hwnd: HWND, size: f64, dark: bool) -> Result<(), String> {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let font = Font::new(size, dpi)?;
        self.font.replace(font);
        self.size.set(size);
        self.dark.set(dark);
        self.dpi.set(dpi);
        unsafe {
            // Windows uses bitmap height for its non-client menu bar even for
            // owner-drawn text; itemHeight alone only resizes popup menu rows.
            for entry in self.entries.iter().filter(|entry| entry.top) {
                let (width, _) = paint::measure(entry, self.font.borrow().0, dpi);
                let bitmap = CreateBitmap(
                    width as i32,
                    (size * dpi as f64 / 96.0).ceil() as i32 + 12 * dpi as i32 / 96,
                    1,
                    32,
                    std::ptr::null(),
                );
                if bitmap.is_null() {
                    return Err("cannot size Windows menu item".into());
                }
                let info = MENUITEMINFOW {
                    cbSize: size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_BITMAP | MIIM_FTYPE,
                    hbmpItem: bitmap,
                    fType: entry.original_type | MFT_OWNERDRAW,
                    ..Default::default()
                };
                if SetMenuItemInfoW(entry.menu, entry.position, 1, &info) == 0 {
                    DeleteObject(bitmap);
                    return Err("cannot resize Windows menu item".into());
                }
                let old = entry.bitmap.replace(bitmap);
                if !old.is_null() {
                    DeleteObject(old);
                }
            }
            let menu = GetMenu(hwnd);
            SetMenu(hwnd, null_mut());
            SetMenu(hwnd, menu);
            DrawMenuBar(hwnd);
        }
        Ok(())
    }
}

pub fn install(window: &tauri::WebviewWindow) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0;
    unsafe {
        let menu = GetMenu(hwnd);
        if menu.is_null() {
            return Ok(());
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut state = Box::new(MenuFonts {
            entries: Vec::new(),
            font: RefCell::new(Font::new(13.0, dpi)?),
            size: Cell::new(13.0),
            dark: Cell::new(false),
            dpi: Cell::new(dpi),
            draws: Cell::new(0),
        });
        collect(menu, true, &mut state.entries)?;
        let pointer = Box::into_raw(state);
        if SetWindowSubclass(hwnd, Some(subclass), SUBCLASS, pointer as usize) == 0 {
            drop(Box::from_raw(pointer));
            return Err("cannot install Windows menu typography".into());
        }
        for entry in &(*pointer).entries {
            let info = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_DATA,
                fType: entry.original_type | MFT_OWNERDRAW,
                dwItemData: (&**entry as *const Entry) as usize,
                ..Default::default()
            };
            if SetMenuItemInfoW(entry.menu, entry.position, 1, &info) == 0 {
                for original in &(*pointer).entries {
                    let info = MENUITEMINFOW {
                        cbSize: size_of::<MENUITEMINFOW>() as u32,
                        fMask: MIIM_FTYPE | MIIM_DATA,
                        fType: original.original_type,
                        dwItemData: original.original_data,
                        ..Default::default()
                    };
                    SetMenuItemInfoW(original.menu, original.position, 1, &info);
                }
                RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS);
                drop(Box::from_raw(pointer));
                return Err("cannot apply Windows menu typography".into());
            }
        }
        (*pointer).refresh(hwnd, 13.0, false)?;
    }
    Ok(())
}

unsafe fn collect(menu: HMENU, top: bool, entries: &mut Vec<Box<Entry>>) -> Result<(), String> {
    unsafe {
        for position in 0..GetMenuItemCount(menu) {
            let mut buffer = vec![0u16; 1024];
            let mut info = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_STRING | MIIM_FTYPE | MIIM_DATA | MIIM_SUBMENU,
                dwTypeData: buffer.as_mut_ptr(),
                cch: buffer.len() as u32,
                ..Default::default()
            };
            if GetMenuItemInfoW(menu, position as u32, 1, &mut info) == 0 {
                return Err("cannot read native menu item".into());
            }
            if info.fType & MFT_SEPARATOR != 0 {
                continue;
            }
            let label = String::from_utf16_lossy(&buffer[..info.cch as usize]);
            let mut name: Vec<u16> = label
                .replace('&', "")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let entry = Box::new(Entry {
                msaa: MSAAMENUINFO {
                    dwMSAASignature: MSAA_MENU_SIG as u32,
                    cchWText: name.len() as u32 - 1,
                    pszWText: name.as_mut_ptr(),
                },
                name,
                label,
                menu,
                position: position as u32,
                original_type: info.fType,
                original_data: info.dwItemData,
                top,
                submenu: !info.hSubMenu.is_null(),
                bitmap: Cell::new(null_mut()),
            });
            entries.push(entry);
            if !info.hSubMenu.is_null() {
                collect(info.hSubMenu, false, entries)?;
            }
        }
        Ok(())
    }
}

pub fn project(window: &tauri::WebviewWindow, size: f64, dark: bool) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    window
        .run_on_main_thread(move || unsafe {
            let mut pointer = 0;
            if GetWindowSubclass(hwnd as HWND, Some(subclass), SUBCLASS, &mut pointer) != 0 {
                let state = &*(pointer as *const MenuFonts);
                if let Err(error) = state.refresh(hwnd as HWND, size, dark) {
                    eprintln!("Menu typography: {error}");
                }
            }
        })
        .map_err(|e| e.to_string())
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let state = &*(data as *const MenuFonts);
        match message {
            WM_MEASUREITEM if lparam != 0 => {
                let item = &mut *(lparam as *mut MEASUREITEMSTRUCT);
                if item.CtlType == ODT_MENU
                    && let Some(entry) = state.entry(item.itemData)
                {
                    (item.itemWidth, item.itemHeight) =
                        paint::measure(entry, state.font.borrow().0, state.dpi.get());
                    return 1;
                }
            }
            WM_DRAWITEM if lparam != 0 => {
                let item = &*(lparam as *const DRAWITEMSTRUCT);
                if item.CtlType == ODT_MENU
                    && let Some(entry) = state.entry(item.itemData)
                {
                    paint::draw(
                        entry,
                        item,
                        state.font.borrow().0,
                        state.dpi.get(),
                        state.dark.get(),
                    );
                    state.draws.set(state.draws.get().saturating_add(1));
                    return 1;
                }
            }
            WM_MENUCHAR => {
                let menu = lparam as HMENU;
                let key = char::from_u32((wparam & 0xffff) as u32)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let matches: Vec<_> = state
                    .entries
                    .iter()
                    .filter(|entry| {
                        entry.menu == menu
                            && mnemonic(&entry.label) == Some(key)
                            && GetMenuState(menu, entry.position, MF_BYPOSITION) & MFS_DISABLED == 0
                    })
                    .collect();
                if !matches.is_empty() {
                    let highlighted = matches.iter().position(|entry| {
                        GetMenuState(menu, entry.position, MF_BYPOSITION) & MFS_HILITE != 0
                    });
                    let next = highlighted.map_or(0, |index| (index + 1) % matches.len());
                    let action = if matches.len() == 1 {
                        MNC_EXECUTE
                    } else {
                        MNC_SELECT
                    };
                    return ((action << 16) | matches[next].position) as LRESULT;
                }
            }
            WM_DPICHANGED => {
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                let _ = state.refresh(hwnd, state.size.get(), state.dark.get());
                return result;
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS);
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                drop(Box::from_raw(data as *mut MenuFonts));
                return result;
            }
            _ => {}
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

fn mnemonic(label: &str) -> Option<char> {
    label
        .split_once('&')
        .and_then(|(_, rest)| rest.chars().next())
        .or_else(|| label.chars().next())
        .map(|c| c.to_ascii_lowercase())
}

#[cfg(debug_assertions)]
pub fn metrics(window: &tauri::WebviewWindow) -> Result<serde_json::Value, String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    let (sender, receiver) = std::sync::mpsc::channel();
    window.run_on_main_thread(move || unsafe {
        let mut pointer = 0;
        let result = if GetWindowSubclass(hwnd as HWND, Some(subclass), SUBCLASS, &mut pointer) != 0 {
            let state = &*(pointer as *const MenuFonts);
            let items: Vec<_> = state.entries.iter().filter(|e| e.top).map(|entry| {
                let mut rect = RECT::default();
                GetMenuItemRect(hwnd as HWND, entry.menu, entry.position, &mut rect);
                serde_json::json!({ "label": entry.label, "width": rect.right-rect.left, "height": rect.bottom-rect.top })
            }).collect();
            serde_json::json!({ "fontSize": state.size.get(), "dpi": state.dpi.get(), "draws": state.draws.get(), "items": items })
        } else { serde_json::Value::Null };
        let _ = sender.send(result);
    }).map_err(|e| e.to_string())?;
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())
}
