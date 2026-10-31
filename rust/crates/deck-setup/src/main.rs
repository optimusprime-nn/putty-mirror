//! Installer and uninstaller for RustuTTY. The files to install are embedded,
//! so setup is a single program. It installs for the current user only, needing
//! no administrator rights, and copies itself in to serve as the uninstaller.
//!
//!   setup.exe                 install, asking first
//!   setup.exe /S              install silently
//!   uninstall.exe /U          remove, asking first
//!   uninstall.exe /U /S       remove silently
//!
//! Saved sessions and host keys are left alone: they belong to PuTTY, and are
//! shared with any ordinary copy of it.
#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use windows::core::{Interface, GUID, HSTRING};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::UI::Shell::PropertiesSystem::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use winreg::enums::*;
use winreg::RegKey;

const EXE: &[u8] = include_bytes!(env!("DECK_EXE"));
const LICENCE: &[u8] = include_bytes!("../../../LICENCE");
const TOOLS: [(&str, &[u8]); 5] = [
    ("plink.exe", include_bytes!(concat!(env!("DECK_TOOLS"), "/plink.exe"))),
    ("pscp.exe", include_bytes!(concat!(env!("DECK_TOOLS"), "/pscp.exe"))),
    ("psftp.exe", include_bytes!(concat!(env!("DECK_TOOLS"), "/psftp.exe"))),
    ("puttygen.exe", include_bytes!(concat!(env!("DECK_TOOLS"), "/puttygen.exe"))),
    ("pageant.exe", include_bytes!(concat!(env!("DECK_TOOLS"), "/pageant.exe"))),
];
const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\RustuTTY";
const APP_ID: &str = "RustuTTY";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// PKEY_AppUserModel_ID
const APP_ID_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

struct Setup {
    silent: bool,
}

impl Setup {
    fn ask(&self, text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
        unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("RustuTTY Setup"), style | MB_SETFOREGROUND) }
    }

    fn complain(&self, text: &str) {
        if !self.silent {
            self.ask(text, MB_OK | MB_ICONERROR);
        }
    }

    fn install(&self) -> i32 {
        let (Some(dir), Some(lnk)) = (install_dir(), shortcut_path()) else {
            self.complain("Could not find where to install.");
            return 1;
        };
        let (exe, licence, uninstaller) = (dir.join("rustutty.exe"), dir.join("LICENCE.txt"), dir.join("uninstall.exe"));
        if !self.silent {
            let text = format!(
                "Install RustuTTY {VERSION} for this user?\n\nFolder: {}\n\nYour saved PuTTY sessions are not changed.",
                dir.display()
            );
            if self.ask(&text, MB_YESNO | MB_ICONQUESTION) != IDYES {
                return 2;
            }
        }
        if std::fs::create_dir_all(&dir).is_err() {
            self.complain("Could not create the install folder.");
            return 1;
        }
        // A running copy can't be overwritten, and this fails before anything
        // has changed, so an upgrade is all or nothing.
        if std::fs::write(&exe, EXE).is_err() {
            self.complain("Could not write rustutty.exe.\n\nIf RustuTTY is running, close it and try again.");
            return 1;
        }
        let rest = (|| -> Result<(), String> {
            std::fs::write(&licence, LICENCE).map_err(|e| format!("licence: {e}"))?;
            for (name, data) in TOOLS {
                std::fs::write(dir.join(name), data).map_err(|e| format!("{name}: {e}"))?;
            }
            let me = std::env::current_exe().map_err(|e| format!("own path: {e}"))?;
            std::fs::copy(me, &uninstaller).map_err(|e| format!("uninstaller: {e}"))?;
            make_shortcut(&lnk, &exe, &dir).map_err(|e| format!("shortcut: {e}"))?;
            register(&dir, &exe, &uninstaller).map_err(|e| format!("registry: {e}"))
        })();
        if let Err(why) = rest {
            self.complain(&format!("Installation did not finish.\n\n{why}"));
            return 1;
        }
        if !self.silent && self.ask("RustuTTY is installed. Start it now?", MB_YESNO | MB_ICONINFORMATION) == IDYES {
            let _ = std::process::Command::new(&exe).current_dir(&dir).spawn();
        }
        0
    }

    fn uninstall(&self) -> i32 {
        let (Some(dir), Some(lnk)) = (install_dir(), shortcut_path()) else {
            self.complain("Could not find the install folder.");
            return 1;
        };
        if !self.silent && self.ask("Remove RustuTTY?\n\nYour saved PuTTY sessions are kept.", MB_YESNO | MB_ICONQUESTION) != IDYES {
            return 2;
        }
        if let Err(e) = std::fs::remove_file(dir.join("rustutty.exe")) {
            if e.kind() != std::io::ErrorKind::NotFound {
                self.complain("Could not remove rustutty.exe.\n\nIf RustuTTY is running, close it and try again.");
                return 1;
            }
        }
        let _ = std::fs::remove_file(dir.join("LICENCE.txt"));
        for (name, _) in TOOLS {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = std::fs::remove_file(lnk);
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey(UNINSTALL_KEY);
        if !self.silent {
            self.ask("RustuTTY has been removed.", MB_OK | MB_ICONINFORMATION);
        }
        // This has to be the very last thing done: the deletion only works
        // once this program has exited, and it waits a fixed time for that.
        delete_self_later(&dir);
        0
    }
}

fn known_folder(id: &GUID, flags: KNOWN_FOLDER_FLAG) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, flags, None).ok()?;
        let path = p.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(p.0 as *const _));
        path
    }
}

fn install_dir() -> Option<PathBuf> {
    Some(known_folder(&FOLDERID_UserProgramFiles, KF_FLAG_CREATE)?.join("RustuTTY"))
}

fn shortcut_path() -> Option<PathBuf> {
    Some(known_folder(&FOLDERID_Programs, KF_FLAG_DEFAULT)?.join("RustuTTY.lnk"))
}

fn make_shortcut(lnk: &Path, exe: &Path, dir: &Path) -> windows::core::Result<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        link.SetDescription(&HSTRING::from("Terminal sessions in one window"))?;
        // Without this, a pinned shortcut and the running program would get
        // separate taskbar buttons.
        let store: IPropertyStore = link.cast()?;
        store.SetValue(&APP_ID_KEY, &PROPVARIANT::from(APP_ID))?;
        store.Commit()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
    }
}

fn register(dir: &Path, exe: &Path, uninstaller: &Path) -> std::io::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey_with_flags(UNINSTALL_KEY, KEY_SET_VALUE)?;
    key.set_value("DisplayName", &"RustuTTY")?;
    key.set_value("DisplayVersion", &VERSION)?;
    key.set_value("DisplayIcon", &exe.display().to_string())?;
    key.set_value("InstallLocation", &dir.display().to_string())?;
    key.set_value("UninstallString", &format!("\"{}\" /U", uninstaller.display()))?;
    key.set_value("QuietUninstallString", &format!("\"{}\" /U /S", uninstaller.display()))?;
    key.set_value("NoModify", &1u32)?;
    key.set_value("NoRepair", &1u32)?;
    key.set_value("EstimatedSize", &((EXE.len() / 1024) as u32))?;
    Ok(())
}

/// A running program can't delete itself, so a short-lived hidden command
/// prompt removes uninstall.exe and the folder after this one exits. It names
/// exactly those two things, and rmdir without /s leaves the folder alone if
/// anything else has been put in it.
fn delete_self_later(dir: &Path) {
    let uninstaller = dir.join("uninstall.exe");
    let Some(root) = std::env::var_os("SystemRoot") else { return };
    let system = Path::new(&root).join("System32");
    let cmd = format!("/c ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"", uninstaller.display(), dir.display());
    let _ = std::process::Command::new(system.join("cmd.exe"))
        .raw_arg(cmd)
        .current_dir(&system)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

fn main() {
    // Only load DLLs from the system, whatever is in Downloads.
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_uppercase()).collect();
    let setup = Setup { silent: args.iter().any(|a| a == "/S") };
    let uninstall = args.iter().any(|a| a == "/U");
    if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
        std::process::exit(1);
    }
    let status = if uninstall { setup.uninstall() } else { setup.install() };
    std::process::exit(status);
}
