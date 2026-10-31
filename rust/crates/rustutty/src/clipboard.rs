//! The Windows clipboard, or a private one in test builds.

#[cfg(feature = "test-hooks")]
mod imp {
    use std::sync::Mutex;
    static TEXT: Mutex<String> = Mutex::new(String::new());
    pub fn get() -> Option<String> {
        Some(TEXT.lock().unwrap().clone()).filter(|s| !s.is_empty())
    }
    pub fn set(text: &str) {
        *TEXT.lock().unwrap() = text.to_string();
    }
    pub fn set_with_rtf(text: &str, _rtf: &str) {
        set(text);
    }
}

#[cfg(not(feature = "test-hooks"))]
mod imp {
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    /// Another program can hold the clipboard briefly, so opening is retried.
    fn open() -> bool {
        for _ in 0..50 {
            if unsafe { OpenClipboard(Some(HWND::default())) }.is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    pub fn get() -> Option<String> {
        if !open() {
            return None;
        }
        let mut text = None;
        unsafe {
            if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
                let g = HGLOBAL(h.0);
                let p = GlobalLock(g) as *const u16;
                if !p.is_null() {
                    // another program wrote this, so its end can't be taken on trust
                    let limit = GlobalSize(g) / 2;
                    let mut n = 0;
                    while n < limit && *p.add(n) != 0 {
                        n += 1;
                    }
                    text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                    let _ = GlobalUnlock(g);
                }
            }
            let _ = CloseClipboard();
        }
        text
    }

    /// Puts bytes on the open clipboard in a format.
    unsafe fn put(format: u32, bytes: *const u8, len: usize) {
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, len) {
            let p = GlobalLock(g) as *mut u8;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(bytes, p, len);
                let _ = GlobalUnlock(g);
                if SetClipboardData(format, Some(HANDLE(g.0))).is_err() {
                    let _ = GlobalFree(Some(g));
                }
            }
        }
    }

    pub fn set(text: &str) {
        set_with_rtf(text, "");
    }

    /// Sets the text, and the same again as rich text if it's given.
    pub fn set_with_rtf(text: &str, rtf: &str) {
        let wide: Vec<u16> = text.replace('\n', "\r\n").encode_utf16().chain([0]).collect();
        if !open() {
            return;
        }
        unsafe {
            let _ = EmptyClipboard();
            put(CF_UNICODETEXT.0 as u32, wide.as_ptr().cast(), wide.len() * 2);
            if !rtf.is_empty() {
                let format = RegisterClipboardFormatW(windows::core::w!("Rich Text Format"));
                let bytes: Vec<u8> = rtf.bytes().chain([0]).collect();
                put(format, bytes.as_ptr(), bytes.len());
            }
            let _ = CloseClipboard();
        }
    }
}

pub use imp::{get, set, set_with_rtf};
