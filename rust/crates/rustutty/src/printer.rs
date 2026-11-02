//! Sends what a host prints to a Windows printer, unchanged.

use crate::wide;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Graphics::Printing::*;

/// One print job, which ends when this is dropped.
pub struct Job(PRINTER_HANDLE);

impl Job {
    pub fn start(printer: &str) -> Option<Job> {
        unsafe {
            let mut h = PRINTER_HANDLE::default();
            let name = wide(printer);
            OpenPrinterW(PCWSTR(name.as_ptr()), &mut h, None).ok()?;
            let doc = DOC_INFO_1W {
                pDocName: PWSTR(w!("RUSTuTTY remote printer output").as_ptr() as *mut u16),
                pOutputFile: PWSTR::null(),
                pDatatype: PWSTR(w!("RAW").as_ptr() as *mut u16),
            };
            if StartDocPrinterW(h, 1, &doc) == 0 || !StartPagePrinter(h).as_bool() {
                let _ = ClosePrinter(h);
                return None;
            }
            Some(Job(h))
        }
    }

    pub fn write(&self, data: &[u8]) {
        let mut written = 0;
        unsafe {
            let _ = WritePrinter(self.0, data.as_ptr().cast(), data.len() as u32, &mut written);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = EndPagePrinter(self.0);
            let _ = EndDocPrinter(self.0);
            let _ = ClosePrinter(self.0);
        }
    }
}
