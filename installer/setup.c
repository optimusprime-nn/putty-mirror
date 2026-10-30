/*
 * setup.c: installer and uninstaller for PuTTYDeck.
 *
 * The files to install are embedded as resources, so setup is a single
 * program. It installs for the current user only, so it needs no
 * administrator rights, and it copies itself into the install folder
 * to serve as the uninstaller.
 *
 *   setup.exe                 install, asking first
 *   setup.exe /S              install silently
 *   uninstall.exe             remove, asking first (or setup.exe /U)
 *   uninstall.exe /U /S       remove silently
 *
 * Saved sessions and host keys are left alone. They belong to PuTTY
 * and are shared with any ordinary copy of it.
 */

#define COBJMACROS
#define INITGUID
#define UNICODE
#define _UNICODE

#include <windows.h>
#include <shellapi.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <objbase.h>
#include <knownfolders.h>
#include <propkey.h>
#include <stdbool.h>
#include <wchar.h>

#include "setup.h"

#define APPNAME L"PuTTYDeck"
#define APPID L"PuTTYDeck"             /* matches get_app_user_model_id */
#define UNINSTALL_KEY \
    L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\PuTTYDeck"

#define PATHLEN 1024

static bool silent = false;

static int ask(const wchar_t *msg, UINT type)
{
    return MessageBoxW(NULL, msg, APPNAME L" Setup", type | MB_SETFOREGROUND);
}

static void complain(const wchar_t *msg)
{
    if (!silent)
        ask(msg, MB_OK | MB_ICONERROR);
}

static bool install_dir(wchar_t *out)
{
    wchar_t *base;
    bool ok = false;

    if (SUCCEEDED(SHGetKnownFolderPath(&FOLDERID_UserProgramFiles,
                                       KF_FLAG_CREATE, NULL, &base))) {
        ok = _snwprintf_s(out, PATHLEN, _TRUNCATE, L"%ls\\" APPNAME, base) > 0;
        CoTaskMemFree(base);
    }
    return ok;
}

static bool shortcut_path(wchar_t *out)
{
    wchar_t *base;
    bool ok = false;

    if (SUCCEEDED(SHGetKnownFolderPath(&FOLDERID_Programs, 0, NULL, &base))) {
        ok = _snwprintf_s(out, PATHLEN, _TRUNCATE,
                          L"%ls\\" APPNAME L".lnk", base) > 0;
        CoTaskMemFree(base);
    }
    return ok;
}

static void join(wchar_t *out, const wchar_t *dir, const wchar_t *name)
{
    _snwprintf_s(out, PATHLEN, _TRUNCATE, L"%ls\\%ls", dir, name);
}

static bool extract(int id, const wchar_t *path)
{
    HRSRC res = FindResourceW(NULL, MAKEINTRESOURCEW(id), (LPCWSTR)RT_RCDATA);
    HGLOBAL data = res ? LoadResource(NULL, res) : NULL;
    const void *bytes = data ? LockResource(data) : NULL;
    DWORD size = res ? SizeofResource(NULL, res) : 0, written = 0;
    HANDLE f;
    bool ok;

    if (!bytes || !size)
        return false;

    f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                    FILE_ATTRIBUTE_NORMAL, NULL);
    if (f == INVALID_HANDLE_VALUE)
        return false;
    ok = WriteFile(f, bytes, size, &written, NULL) && written == size;
    CloseHandle(f);
    if (!ok)
        DeleteFileW(path);
    return ok;
}

static bool make_shortcut(const wchar_t *lnk, const wchar_t *exe,
                          const wchar_t *dir)
{
    IShellLinkW *link = NULL;
    IPersistFile *file = NULL;
    IPropertyStore *store = NULL;
    PROPVARIANT id;
    bool ok = false;

    if (FAILED(CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER,
                                &IID_IShellLinkW, (void **)&link)))
        return false;

    IShellLinkW_SetPath(link, exe);
    IShellLinkW_SetWorkingDirectory(link, dir);
    IShellLinkW_SetDescription(link, L"PuTTY sessions in one window");

    /* Without this, a pinned shortcut and the running program would
     * get separate taskbar buttons */
    if (SUCCEEDED(IShellLinkW_QueryInterface(link, &IID_IPropertyStore,
                                             (void **)&store))) {
        memset(&id, 0, sizeof(id));
        id.vt = VT_LPWSTR;
        id.pwszVal = (LPWSTR)APPID;
        IPropertyStore_SetValue(store, &PKEY_AppUserModel_ID, &id);
        IPropertyStore_Commit(store);
        IPropertyStore_Release(store);
    }

    if (SUCCEEDED(IShellLinkW_QueryInterface(link, &IID_IPersistFile,
                                             (void **)&file))) {
        ok = SUCCEEDED(IPersistFile_Save(file, lnk, TRUE));
        IPersistFile_Release(file);
    }
    IShellLinkW_Release(link);
    return ok;
}

static void set_string(HKEY key, const wchar_t *name, const wchar_t *value)
{
    RegSetValueExW(key, name, 0, REG_SZ, (const BYTE *)value,
                   (DWORD)((wcslen(value) + 1) * sizeof(wchar_t)));
}

static void set_dword(HKEY key, const wchar_t *name, DWORD value)
{
    RegSetValueExW(key, name, 0, REG_DWORD, (const BYTE *)&value,
                   sizeof(value));
}

static bool register_uninstall(const wchar_t *dir, const wchar_t *exe,
                               const wchar_t *uninstaller)
{
    HKEY key;
    wchar_t cmd[PATHLEN + 16];

    if (RegCreateKeyExW(HKEY_CURRENT_USER, UNINSTALL_KEY, 0, NULL, 0,
                        KEY_SET_VALUE, NULL, &key, NULL) != ERROR_SUCCESS)
        return false;

    set_string(key, L"DisplayName", APPNAME);
    set_string(key, L"DisplayVersion", DECK_VERSION);
    set_string(key, L"DisplayIcon", exe);
    set_string(key, L"InstallLocation", dir);
    _snwprintf_s(cmd, lenof_cmd, _TRUNCATE, L"\"%ls\" /U", uninstaller);
    set_string(key, L"UninstallString", cmd);
    _snwprintf_s(cmd, lenof_cmd, _TRUNCATE, L"\"%ls\" /U /S", uninstaller);
    set_string(key, L"QuietUninstallString", cmd);
    set_dword(key, L"NoModify", 1);
    set_dword(key, L"NoRepair", 1);
    set_dword(key, L"EstimatedSize", DECK_SIZE_KB);
    RegCloseKey(key);
    return true;
}

static int do_install(void)
{
    wchar_t dir[PATHLEN], exe[PATHLEN], licence[PATHLEN];
    wchar_t uninstaller[PATHLEN], lnk[PATHLEN], self[PATHLEN], msg[PATHLEN * 2];

    if (!install_dir(dir) || !shortcut_path(lnk)) {
        complain(L"Could not find where to install.");
        return 1;
    }
    join(exe, dir, L"puttydeck.exe");
    join(licence, dir, L"LICENCE.txt");
    join(uninstaller, dir, L"uninstall.exe");

    if (!silent) {
        _snwprintf_s(msg, lenof_msg, _TRUNCATE,
                     L"Install PuTTYDeck %ls for this user?\n\n"
                     L"Folder: %ls\n\n"
                     L"Your saved PuTTY sessions are not changed.",
                     DECK_VERSION, dir);
        if (ask(msg, MB_YESNO | MB_ICONQUESTION) != IDYES)
            return 2;
    }

    CreateDirectoryW(dir, NULL);
    if (GetFileAttributesW(dir) == INVALID_FILE_ATTRIBUTES) {
        complain(L"Could not create the install folder.");
        return 1;
    }

    /* A running copy can't be overwritten, and this fails before
     * anything has changed, so an upgrade is all or nothing */
    if (!extract(IDR_PUTTYDECK, exe)) {
        complain(L"Could not write puttydeck.exe.\n\n"
                 L"If PuTTYDeck is running, close it and try again.");
        return 1;
    }
    if (!extract(IDR_LICENCE, licence) ||
        !GetModuleFileNameW(NULL, self, PATHLEN) ||
        !CopyFileW(self, uninstaller, FALSE) ||
        !make_shortcut(lnk, exe, dir) ||
        !register_uninstall(dir, exe, uninstaller)) {
        complain(L"Installation did not finish.");
        return 1;
    }

    if (!silent && ask(L"PuTTYDeck is installed. Start it now?",
                       MB_YESNO | MB_ICONINFORMATION) == IDYES)
        ShellExecuteW(NULL, L"open", exe, NULL, dir, SW_SHOWNORMAL);
    return 0;
}

/*
 * A running program can't delete itself, so a short-lived hidden
 * command prompt removes uninstall.exe and the folder once we've
 * exited. It names exactly those two things, and rmdir without /s
 * leaves the folder alone if anything else has been put in it.
 */
static void delete_self_later(const wchar_t *dir, const wchar_t *uninstaller)
{
    wchar_t cmd[PATHLEN * 3], system[MAX_PATH], shell[PATHLEN];
    STARTUPINFOW si;
    PROCESS_INFORMATION pi;

    if (!GetSystemDirectoryW(system, MAX_PATH))
        return;
    join(shell, system, L"cmd.exe");
    _snwprintf_s(cmd, PATHLEN * 3, _TRUNCATE,
                 L"cmd.exe /c ping -n 3 127.0.0.1 >nul & "
                 L"del /f /q \"%ls\" & rmdir \"%ls\"", uninstaller, dir);

    memset(&si, 0, sizeof(si));
    si.cb = sizeof(si);
    if (CreateProcessW(shell, cmd, NULL, NULL, FALSE, CREATE_NO_WINDOW, NULL,
                       system, &si, &pi)) {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
}

static int do_uninstall(void)
{
    wchar_t dir[PATHLEN], exe[PATHLEN], licence[PATHLEN];
    wchar_t uninstaller[PATHLEN], lnk[PATHLEN];

    if (!install_dir(dir) || !shortcut_path(lnk)) {
        complain(L"Could not find the install folder.");
        return 1;
    }
    join(exe, dir, L"puttydeck.exe");
    join(licence, dir, L"LICENCE.txt");
    join(uninstaller, dir, L"uninstall.exe");

    if (!silent &&
        ask(L"Remove PuTTYDeck?\n\nYour saved PuTTY sessions are kept.",
            MB_YESNO | MB_ICONQUESTION) != IDYES)
        return 2;

    if (!DeleteFileW(exe) && GetLastError() != ERROR_FILE_NOT_FOUND &&
        GetLastError() != ERROR_PATH_NOT_FOUND) {
        complain(L"Could not remove puttydeck.exe.\n\n"
                 L"If PuTTYDeck is running, close it and try again.");
        return 1;
    }
    DeleteFileW(licence);
    DeleteFileW(lnk);
    RegDeleteKeyW(HKEY_CURRENT_USER, UNINSTALL_KEY);

    if (!silent)
        ask(L"PuTTYDeck has been removed.", MB_OK | MB_ICONINFORMATION);

    /* This has to be the very last thing we do: the deletion only
     * works once we've exited, and it waits a fixed time for that */
    delete_self_later(dir, uninstaller);
    ExitProcess(0);
}

int WINAPI wWinMain(HINSTANCE inst, HINSTANCE prev, PWSTR cmdline, int show)
{
    int argc, status;
    bool uninstall = false;
    wchar_t **argv = CommandLineToArgvW(GetCommandLineW(), &argc);

    (void)inst; (void)prev; (void)cmdline; (void)show;

    /* Only load DLLs from the system, whatever is in Downloads */
    SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);

    for (int i = 1; argv && i < argc; i++) {
        if (!_wcsicmp(argv[i], L"/S"))
            silent = true;
        else if (!_wcsicmp(argv[i], L"/U"))
            uninstall = true;
    }

    if (FAILED(CoInitialize(NULL)))
        return 1;
    status = uninstall ? do_uninstall() : do_install();
    CoUninitialize();
    return status;
}
