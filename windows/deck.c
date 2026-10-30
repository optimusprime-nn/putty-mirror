/*
 * deck.c: the PuTTYDeck host window.
 *
 * This is one top-level window with a sidebar on the left, listing
 * the open sessions and the saved ones, and a content panel on the
 * right. Each session is a separate process running the same
 * executable, whose terminal window is a child of the content panel;
 * only the selected session's terminal is visible. See deck.h.
 */

#include <stdio.h>

#include "putty.h"
#include "deck.h"

#define DECK_PARENT_ENV L"PUTTYDECK_PARENT"
#define DECK_MAIN_CLASS L"PuTTYDeckMain"
#define DECK_CONTENT_CLASS L"PuTTYDeckContent"
#define DECK_APPNAME L"PuTTYDeck"

#define IDI_MAINICON 200               /* as in putty-rc.h */

#define COL_SIDEBAR  RGB(0x1E, 0x22, 0x27)
#define COL_TOOLBAR  RGB(0x2A, 0x2F, 0x36)
#define COL_HOVER    RGB(0x36, 0x3C, 0x45)
#define COL_SELECTED RGB(0x2F, 0x6F, 0xDE)
#define COL_TEXT     RGB(0xE8, 0xEA, 0xED)
#define COL_MUTED    RGB(0x9A, 0xA3, 0xAE)
#define COL_LIVE     RGB(0x4A, 0xC2, 0x6B)
#define COL_BELL     RGB(0xF2, 0xB1, 0x34)
#define COL_CONTENT  RGB(0x00, 0x00, 0x00)

enum {
    ID_NEW = 100, ID_COPY, ID_PASTE, ID_COPYALL, ID_PASTEALL,
    ID_OPENLIST, ID_SAVEDLIST,
    ID_MENU_DUPLICATE, ID_MENU_COPYALL, ID_MENU_PASTE, ID_MENU_CLOSE,
    ID_ACCEL_NEW, ID_ACCEL_NEXT, ID_ACCEL_PREV,
};

enum { TIMER_POLL = 1 };
#define WM_DECK_REFOCUS (WM_APP + 0x210)

HWND deck_parent = NULL;

typedef struct DeckSession {
    HWND hwnd;
    wchar_t title[256];
    bool bell;
} DeckSession;

static DeckSession *sessions = NULL;
static size_t nsessions = 0, sessionsize = 0;
static int active = -1;

static struct sesslist saved;
static bool saved_valid = false;

static HINSTANCE deck_inst;
static HWND main_hwnd, content_hwnd, open_list, saved_list;
static HWND btn_new, btn_copy, btn_paste, btn_copyall, btn_pasteall;
static WNDPROC open_list_proc;
static HFONT ui_font, ui_font_small;
static HBRUSH br_sidebar, br_toolbar, br_content;
static UINT dpi = 96;
static int sidebar_width = 250;        /* unscaled */
static bool dragging_splitter = false, in_sizemove = false;

static int S(int x)
{
    return MulDiv(x, dpi, 96);
}

/*
 * If the host dies without closing us, our terminal has nowhere to be
 * shown, so we'd linger invisibly holding a connection open. Whether
 * Windows destroys a child window owned by another process in that
 * case isn't something we can rely on, and a session sitting in a
 * modal dialog wouldn't notice even if it did.
 */
static DWORD WINAPI host_watchdog(void *param)
{
    WaitForSingleObject((HANDLE)param, INFINITE);
    ExitProcess(0);
    return 0;
}

/*
 * Called by a process started with PUTTYDECK_PARENT in its
 * environment. A stale or inherited value just means we're the host.
 */
bool deck_find_parent(void)
{
    wchar_t buf[32], cls[64];
    unsigned long long value;
    HWND hwnd;
    DWORD pid = 0;
    HANDLE host;

    if (!GetEnvironmentVariableW(DECK_PARENT_ENV, buf, lenof(buf)))
        return false;
    if (swscanf(buf, L"%llx", &value) != 1)
        return false;
    hwnd = (HWND)(ULONG_PTR)value;
    if (!IsWindow(hwnd) || !GetClassNameW(hwnd, cls, lenof(cls)) ||
        wcscmp(cls, DECK_CONTENT_CLASS))
        return false;

    GetWindowThreadProcessId(hwnd, &pid);
    host = OpenProcess(SYNCHRONIZE, false, pid);
    if (host) {
        HANDLE thread = CreateThread(NULL, 0, host_watchdog, host, 0, NULL);
        if (thread)
            CloseHandle(thread);
    }

    deck_parent = hwnd;
    return true;
}

static void spawn_session(const wchar_t *args)
{
    wchar_t exe[MAX_PATH * 2], *cmd;
    size_t cmdlen;
    STARTUPINFOW si;
    PROCESS_INFORMATION pi;

    if (!GetModuleFileNameW(NULL, exe, lenof(exe)))
        return;

    cmdlen = wcslen(exe) + wcslen(args) + 4;
    cmd = snewn(cmdlen, wchar_t);
    swprintf(cmd, cmdlen, L"\"%ls\" %ls", exe, args);

    memset(&si, 0, sizeof(si));
    si.cb = sizeof(si);
    /* The session's config box has to be able to come to the front */
    AllowSetForegroundWindow(ASFW_ANY);
    if (CreateProcessW(exe, cmd, NULL, NULL, false, 0, NULL, NULL, &si, &pi)) {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    } else {
        MessageBoxW(main_hwnd, L"Unable to start a new session.",
                    DECK_APPNAME, MB_OK | MB_ICONERROR);
    }
    sfree(cmd);
}

static void spawn_saved(const char *name)
{
    wchar_t *wname = dup_mb_to_wc(DEFAULT_CODEPAGE, name);
    size_t i, o = 0, len = wcslen(wname);
    wchar_t *args = snewn(2 * len + 16, wchar_t);

    wcscpy(args, L"-load \"");
    o = wcslen(args);
    for (i = 0; i < len; i++) {
        if (wname[i] == L'"' || wname[i] == L'\\')
            args[o++] = L'\\';
        args[o++] = wname[i];
    }
    args[o++] = L'"';
    args[o] = 0;
    spawn_session(args);
    sfree(args);
    sfree(wname);
}

static void refocus(void)
{
    if (active >= 0)
        SetFocus(sessions[active].hwnd);
    else
        SetFocus(main_hwnd);
}

static void fit_session(int i)
{
    RECT r;
    GetClientRect(content_hwnd, &r);
    /* Async, so that a session stuck in a modal loop can't hold us up */
    SetWindowPos(sessions[i].hwnd, NULL, 0, 0, r.right, r.bottom,
                 SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
}

static void update_caption(void)
{
    wchar_t buf[300];

    if (active >= 0) {
        swprintf(buf, lenof(buf), L"%ls - " DECK_APPNAME,
                 sessions[active].title);
        SetWindowTextW(main_hwnd, buf);
    } else {
        SetWindowTextW(main_hwnd, DECK_APPNAME);
    }
}

static void update_toolbar(void)
{
    BOOL any = active >= 0;
    EnableWindow(btn_copy, any);
    EnableWindow(btn_paste, any);
    EnableWindow(btn_copyall, any);
    EnableWindow(btn_pasteall, nsessions > 1);
}

static void select_session(int i)
{
    int prev = active;

    if (i < 0 || i >= (int)nsessions)
        return;

    active = i;
    sessions[i].bell = false;
    fit_session(i);
    ShowWindowAsync(sessions[i].hwnd, SW_SHOW);
    if (prev >= 0 && prev != i && prev < (int)nsessions)
        ShowWindowAsync(sessions[prev].hwnd, SW_HIDE);

    SendMessageW(open_list, LB_SETCURSEL, i, 0);
    InvalidateRect(open_list, NULL, false);
    update_caption();
    update_toolbar();
    PostMessageW(main_hwnd, WM_DECK_REFOCUS, 0, 0);
}

static int find_session(HWND hwnd)
{
    for (size_t i = 0; i < nsessions; i++)
        if (sessions[i].hwnd == hwnd)
            return (int)i;
    return -1;
}

static bool refresh_title(int i)
{
    wchar_t buf[lenof(sessions[0].title)];
    DWORD_PTR res;

    if (!SendMessageTimeoutW(sessions[i].hwnd, WM_GETTEXT, lenof(buf),
                             (LPARAM)buf, SMTO_ABORTIFHUNG, 100, &res))
        return false;
    if (!wcscmp(buf, sessions[i].title))
        return false;
    wcscpy(sessions[i].title, buf);
    return true;
}

static void redraw_row(int i)
{
    RECT r;
    if (SendMessageW(open_list, LB_GETITEMRECT, i, (LPARAM)&r) != LB_ERR)
        InvalidateRect(open_list, &r, false);
}

static void attach_session(HWND hwnd)
{
    if (!IsWindow(hwnd) || find_session(hwnd) >= 0)
        return;

    sgrowarray(sessions, sessionsize, nsessions);
    memset(&sessions[nsessions], 0, sizeof(*sessions));
    sessions[nsessions].hwnd = hwnd;
    nsessions++;
    refresh_title((int)nsessions - 1);
    SendMessageW(open_list, LB_ADDSTRING, 0, (LPARAM)L"");
    select_session((int)nsessions - 1);
}

/*
 * Drop any sessions whose window has gone. Checking the windows, not
 * trusting the detach message alone, catches a session that crashed.
 */
static void prune_sessions(void)
{
    bool removed_active = false;
    HWND keep = active >= 0 ? sessions[active].hwnd : NULL;

    for (int i = (int)nsessions - 1; i >= 0; i--) {
        if (IsWindow(sessions[i].hwnd))
            continue;
        if (i == active)
            removed_active = true;
        memmove(&sessions[i], &sessions[i + 1],
                (nsessions - i - 1) * sizeof(*sessions));
        nsessions--;
        SendMessageW(open_list, LB_DELETESTRING, i, 0);
        if (removed_active && active >= i)
            active = i;
    }

    if (removed_active) {
        int next = active < (int)nsessions ? active : (int)nsessions - 1;
        active = -1;
        if (next >= 0)
            select_session(next);
    } else {
        active = keep ? find_session(keep) : -1;
        if (active >= 0)
            SendMessageW(open_list, LB_SETCURSEL, active, 0);
    }

    InvalidateRect(open_list, NULL, false);
    InvalidateRect(content_hwnd, NULL, true);
    update_caption();
    update_toolbar();
}

static void refresh_saved(void)
{
    struct sesslist fresh;
    bool same;

    get_sesslist(&fresh, true);
    same = saved_valid && fresh.nsessions == saved.nsessions;
    for (int i = 0; same && i < fresh.nsessions; i++)
        same = !strcmp(fresh.sessions[i], saved.sessions[i]);
    if (same) {
        get_sesslist(&fresh, false);
        return;
    }

    if (saved_valid)
        get_sesslist(&saved, false);
    saved = fresh;
    saved_valid = true;

    SendMessageW(saved_list, LB_RESETCONTENT, 0, 0);
    /* Entry 0 is always "Default Settings", which isn't a destination */
    for (int i = 1; i < saved.nsessions; i++)
        SendMessageW(saved_list, LB_ADDSTRING, 0, (LPARAM)L"");
}

static size_t clipboard_length(void)
{
    size_t len = 0;

    if (!IsClipboardFormatAvailable(CF_UNICODETEXT) ||
        !OpenClipboard(main_hwnd))
        return 0;
    HANDLE h = GetClipboardData(CF_UNICODETEXT);
    if (h) {
        const wchar_t *text = GlobalLock(h);
        if (text) {
            len = wcslen(text);
            GlobalUnlock(h);
        }
    }
    CloseClipboard();
    return len;
}

static void exec_active(int what)
{
    if (active >= 0)
        PostMessageW(sessions[active].hwnd, WM_DECK_EXEC, what, 0);
    refocus();
}

static void paste_to_all(void)
{
    wchar_t msg[200];
    size_t len = clipboard_length();

    if (!len) {
        MessageBoxW(main_hwnd, L"The clipboard has no text to paste.",
                    DECK_APPNAME, MB_OK | MB_ICONINFORMATION);
    } else {
        swprintf(msg, lenof(msg),
                 L"Paste %zu characters into all %zu open sessions?",
                 len, nsessions);
        if (MessageBoxW(main_hwnd, msg, DECK_APPNAME,
                        MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2) == IDOK)
            for (size_t i = 0; i < nsessions; i++)
                PostMessageW(sessions[i].hwnd, WM_DECK_EXEC,
                             DECK_EXEC_PASTE, 0);
    }
    refocus();
}

static void close_session(int i)
{
    if (i >= 0 && i < (int)nsessions)
        PostMessageW(sessions[i].hwnd, WM_CLOSE, 0, 0);
}

static void step_session(int delta)
{
    if (nsessions)
        select_session((active + delta + (int)nsessions) % (int)nsessions);
}

static void handle_cmd(WPARAM cmd, LPARAM arg)
{
    switch (cmd) {
      case DECK_CMD_NEW:
        spawn_session(L"");
        break;
      case DECK_CMD_CLOSE:
        close_session(active);
        break;
      case DECK_CMD_NEXT:
        step_session(+1);
        break;
      case DECK_CMD_PREV:
        step_session(-1);
        break;
      case DECK_CMD_SELECT:
        select_session((int)arg);
        break;
    }
}

/* Messages from terminals, which are addressed to the content panel */
static bool handle_session_message(UINT msg, WPARAM wp, LPARAM lp)
{
    int i;

    switch (msg) {
      case WM_DECK_ATTACH:
        attach_session((HWND)wp);
        return true;
      case WM_DECK_DETACH:
        /* The window goes away just after this is posted */
        SetTimer(main_hwnd, TIMER_POLL, 100, NULL);
        return true;
      case WM_DECK_TITLE:
        i = find_session((HWND)wp);
        if (i >= 0 && refresh_title(i)) {
            redraw_row(i);
            if (i == active)
                update_caption();
        }
        return true;
      case WM_DECK_BELL:
        i = find_session((HWND)wp);
        if (i >= 0 && i != active) {
            sessions[i].bell = true;
            redraw_row(i);
        }
        return true;
      case WM_DECK_CMD:
        handle_cmd(wp, lp);
        return true;
    }
    return false;
}

static void session_menu(int i, POINT pt)
{
    HMENU m = CreatePopupMenu();
    int cmd;

    AppendMenuW(m, MF_STRING, ID_MENU_DUPLICATE, L"&Duplicate session");
    AppendMenuW(m, MF_STRING, ID_MENU_COPYALL, L"&Copy all output");
    AppendMenuW(m, MF_STRING, ID_MENU_PASTE, L"&Paste");
    AppendMenuW(m, MF_SEPARATOR, 0, NULL);
    AppendMenuW(m, MF_STRING, ID_MENU_CLOSE, L"C&lose session");
    cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON,
                         pt.x, pt.y, 0, main_hwnd, NULL);
    DestroyMenu(m);

    switch (cmd) {
      case ID_MENU_DUPLICATE:
        PostMessageW(sessions[i].hwnd, WM_DECK_EXEC, DECK_EXEC_DUPLICATE, 0);
        break;
      case ID_MENU_COPYALL:
        PostMessageW(sessions[i].hwnd, WM_DECK_EXEC, DECK_EXEC_COPYALL, 0);
        break;
      case ID_MENU_PASTE:
        PostMessageW(sessions[i].hwnd, WM_DECK_EXEC, DECK_EXEC_PASTE, 0);
        break;
      case ID_MENU_CLOSE:
        close_session(i);
        break;
    }
}

/*
 * The open-sessions list is an ordinary list box, plus the extras
 * people expect of a tab strip: a close cross, middle-click to close,
 * and a right-click menu.
 */
static LRESULT CALLBACK open_list_wndproc(HWND hwnd, UINT msg, WPARAM wp,
                                          LPARAM lp)
{
    POINT pt = { (short)LOWORD(lp), (short)HIWORD(lp) };
    LRESULT hit;
    int i;

    switch (msg) {
      case WM_LBUTTONDOWN:
      case WM_MBUTTONUP:
      case WM_RBUTTONUP:
        hit = SendMessageW(hwnd, LB_ITEMFROMPOINT, 0, lp);
        i = LOWORD(hit);
        if (HIWORD(hit) || i >= (int)nsessions)
            break;
        if (msg == WM_MBUTTONUP) {
            close_session(i);
            return 0;
        } else if (msg == WM_RBUTTONUP) {
            ClientToScreen(hwnd, &pt);
            session_menu(i, pt);
            return 0;
        } else {
            RECT r;
            GetClientRect(hwnd, &r);
            if (pt.x >= r.right - S(30)) {
                close_session(i);
                return 0;
            }
        }
        break;
    }
    return CallWindowProcW(open_list_proc, hwnd, msg, wp, lp);
}

static void fill(HDC dc, const RECT *r, COLORREF c)
{
    SetBkColor(dc, c);
    ExtTextOutW(dc, 0, 0, ETO_OPAQUE, r, NULL, 0, NULL);
}

static void draw_session_row(const DRAWITEMSTRUCT *d)
{
    int i = (int)d->itemID;
    bool sel = i == active;
    RECT r = d->rcItem, text;
    COLORREF dot;
    HBRUSH br;
    HGDIOBJ oldbrush, oldpen;
    int sz = S(8), cy = (r.top + r.bottom) / 2;

    if (i < 0 || i >= (int)nsessions) {
        fill(d->hDC, &r, COL_SIDEBAR);
        return;
    }

    fill(d->hDC, &r, sel ? COL_SELECTED : COL_SIDEBAR);
    SetBkMode(d->hDC, TRANSPARENT);
    SelectObject(d->hDC, ui_font);

    dot = sessions[i].bell ? COL_BELL : COL_LIVE;
    br = CreateSolidBrush(dot);
    oldbrush = SelectObject(d->hDC, br);
    oldpen = SelectObject(d->hDC, GetStockObject(NULL_PEN));
    Ellipse(d->hDC, r.left + S(10), cy - sz / 2,
            r.left + S(10) + sz, cy + sz / 2);
    SelectObject(d->hDC, oldbrush);
    SelectObject(d->hDC, oldpen);
    DeleteObject(br);

    text = r;
    text.left += S(26);
    text.right -= S(30);
    SetTextColor(d->hDC, COL_TEXT);
    DrawTextW(d->hDC, sessions[i].title[0] ? sessions[i].title : L"(session)",
              -1, &text, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS |
              DT_NOPREFIX);

    text = r;
    text.left = r.right - S(28);
    text.right = r.right - S(6);
    SetTextColor(d->hDC, sel ? COL_TEXT : COL_MUTED);
    DrawTextW(d->hDC, L"\x00D7", 1, &text,
              DT_SINGLELINE | DT_VCENTER | DT_CENTER);
}

static void draw_saved_row(const DRAWITEMSTRUCT *d)
{
    RECT r = d->rcItem;
    int i = (int)d->itemID + 1;
    bool sel = (d->itemState & ODS_SELECTED) != 0;

    fill(d->hDC, &r, sel ? COL_HOVER : COL_SIDEBAR);
    if (!saved_valid || i < 1 || i >= saved.nsessions)
        return;

    wchar_t *name = dup_mb_to_wc(DEFAULT_CODEPAGE, saved.sessions[i]);
    SetBkMode(d->hDC, TRANSPARENT);
    SelectObject(d->hDC, ui_font);
    SetTextColor(d->hDC, COL_TEXT);
    r.left += S(26);
    r.right -= S(6);
    DrawTextW(d->hDC, name, -1, &r, DT_SINGLELINE | DT_VCENTER |
              DT_END_ELLIPSIS | DT_NOPREFIX);
    sfree(name);
}

static void draw_button(const DRAWITEMSTRUCT *d)
{
    RECT r = d->rcItem;
    wchar_t label[64];
    bool pressed = (d->itemState & ODS_SELECTED) != 0;
    bool disabled = (d->itemState & ODS_DISABLED) != 0;
    bool primary = d->CtlID == ID_NEW;
    COLORREF bg = primary ? COL_SELECTED : pressed ? COL_SIDEBAR : COL_HOVER;

    fill(d->hDC, &r, disabled ? COL_TOOLBAR : bg);
    GetWindowTextW(d->hwndItem, label, lenof(label));
    SetBkMode(d->hDC, TRANSPARENT);
    SelectObject(d->hDC, ui_font);
    SetTextColor(d->hDC, disabled ? COL_MUTED : COL_TEXT);
    DrawTextW(d->hDC, label, -1, &r,
              DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX);
}

static int toolbar_height(void) { return S(40); }

static void sidebar_labels(int *open_y, int *saved_y, int h)
{
    *open_y = S(52);
    *saved_y = h - (h - S(52)) * 38 / 100;
}

static void layout(bool drag)
{
    RECT r;
    int w, h, sw = S(sidebar_width), pad = S(8), lbl = S(24);
    int open_y, saved_y, x, bh = S(28), by;
    struct { HWND h; int w; } btns[] = {
        { btn_copy, 70 }, { btn_paste, 70 },
        { btn_copyall, 86 }, { btn_pasteall, 104 },
    };

    GetClientRect(main_hwnd, &r);
    w = r.right;
    h = r.bottom;
    if (w <= 0 || h <= 0)
        return;
    sidebar_labels(&open_y, &saved_y, h);

    MoveWindow(btn_new, pad, pad, sw - 2 * pad, S(36), true);
    MoveWindow(open_list, 0, open_y + lbl, sw, saved_y - open_y - lbl, true);
    MoveWindow(saved_list, 0, saved_y + lbl, sw, h - saved_y - lbl, true);

    by = (toolbar_height() - bh) / 2;
    x = sw + S(4) + pad;
    for (size_t i = 0; i < lenof(btns); i++) {
        MoveWindow(btns[i].h, x, by, S(btns[i].w), bh, true);
        x += S(btns[i].w) + S(6);
    }

    MoveWindow(content_hwnd, sw + S(4), toolbar_height(),
               w - sw - S(4), h - toolbar_height(), true);

    /* During a drag only the visible session keeps up, so the others'
     * servers aren't told about every intermediate size */
    if (drag) {
        if (active >= 0)
            fit_session(active);
    } else {
        for (size_t i = 0; i < nsessions; i++)
            fit_session((int)i);
    }
    InvalidateRect(main_hwnd, NULL, true);
}

static void make_fonts(void)
{
    if (ui_font)
        DeleteObject(ui_font);
    if (ui_font_small)
        DeleteObject(ui_font_small);
    ui_font = CreateFontW(-S(14), 0, 0, 0, FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET,
                          0, 0, CLEARTYPE_QUALITY, 0, L"Segoe UI");
    ui_font_small = CreateFontW(-S(11), 0, 0, 0, FW_SEMIBOLD, 0, 0, 0,
                                DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, 0,
                                L"Segoe UI");
    if (open_list) {
        SendMessageW(open_list, LB_SETITEMHEIGHT, 0, S(32));
        SendMessageW(saved_list, LB_SETITEMHEIGHT, 0, S(28));
    }
}

static void paint_main(HDC dc)
{
    RECT r, c;
    int open_y, saved_y, sw = S(sidebar_width);

    GetClientRect(main_hwnd, &r);
    sidebar_labels(&open_y, &saved_y, r.bottom);

    c = r;
    c.right = sw;
    fill(dc, &c, COL_SIDEBAR);
    c.left = sw;
    c.right = sw + S(4);
    fill(dc, &c, RGB(0x10, 0x12, 0x15));
    c.left = sw + S(4);
    c.right = r.right;
    c.bottom = toolbar_height();
    fill(dc, &c, COL_TOOLBAR);

    SetBkMode(dc, TRANSPARENT);
    SelectObject(dc, ui_font_small);
    SetTextColor(dc, COL_MUTED);
    c = r;
    c.left = S(12);
    c.right = sw;
    c.top = open_y;
    c.bottom = open_y + S(24);
    DrawTextW(dc, L"OPEN SESSIONS", -1, &c, DT_SINGLELINE | DT_VCENTER);
    c.top = saved_y;
    c.bottom = saved_y + S(24);
    DrawTextW(dc, L"SAVED SESSIONS", -1, &c, DT_SINGLELINE | DT_VCENTER);
}

static LRESULT CALLBACK content_wndproc(HWND hwnd, UINT msg, WPARAM wp,
                                        LPARAM lp)
{
    if (handle_session_message(msg, wp, lp))
        return 0;

    if (msg == WM_PAINT) {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(hwnd, &ps);
        RECT r;
        GetClientRect(hwnd, &r);
        fill(dc, &r, COL_CONTENT);
        if (!nsessions) {
            SetBkMode(dc, TRANSPARENT);
            SelectObject(dc, ui_font);
            SetTextColor(dc, COL_MUTED);
            DrawTextW(dc,
                      L"Click New session, or double-click a saved session.",
                      -1, &r, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
        }
        EndPaint(hwnd, &ps);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

static HWND make_button(const wchar_t *text, int id)
{
    return CreateWindowExW(0, L"BUTTON", text,
                           WS_CHILD | WS_VISIBLE | BS_OWNERDRAW,
                           0, 0, 10, 10, main_hwnd, (HMENU)(INT_PTR)id,
                           deck_inst, NULL);
}

static HWND make_list(int id, DWORD extra)
{
    return CreateWindowExW(0, L"LISTBOX", NULL,
                           WS_CHILD | WS_VISIBLE | WS_VSCROLL | LBS_NOTIFY |
                           LBS_OWNERDRAWFIXED | LBS_NOINTEGRALHEIGHT | extra,
                           0, 0, 10, 10, main_hwnd, (HMENU)(INT_PTR)id,
                           deck_inst, NULL);
}

static void create_children(void)
{
    btn_new = make_button(L"+  New session", ID_NEW);
    btn_copy = make_button(L"Copy", ID_COPY);
    btn_paste = make_button(L"Paste", ID_PASTE);
    btn_copyall = make_button(L"Copy all", ID_COPYALL);
    btn_pasteall = make_button(L"Paste to all", ID_PASTEALL);
    open_list = make_list(ID_OPENLIST, 0);
    saved_list = make_list(ID_SAVEDLIST, 0);
    open_list_proc = (WNDPROC)SetWindowLongPtrW(
        open_list, GWLP_WNDPROC, (LONG_PTR)open_list_wndproc);
    content_hwnd = CreateWindowExW(
        0, DECK_CONTENT_CLASS, NULL,
        WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
        0, 0, 10, 10, main_hwnd, NULL, deck_inst, NULL);
}

static bool in_splitter(LPARAM lp)
{
    int x = (short)LOWORD(lp);
    return x >= S(sidebar_width) && x < S(sidebar_width) + S(4);
}

static void on_command(int id, int code)
{
    switch (id) {
      case ID_NEW:
        spawn_session(L"");
        refocus();
        break;
      case ID_COPY:
        exec_active(DECK_EXEC_COPY);
        break;
      case ID_PASTE:
        exec_active(DECK_EXEC_PASTE);
        break;
      case ID_COPYALL:
        exec_active(DECK_EXEC_COPYALL);
        break;
      case ID_PASTEALL:
        paste_to_all();
        break;
      case ID_OPENLIST:
        if (code == LBN_SELCHANGE)
            select_session(
                (int)SendMessageW(open_list, LB_GETCURSEL, 0, 0));
        break;
      case ID_SAVEDLIST:
        if (code == LBN_DBLCLK) {
            int i = (int)SendMessageW(saved_list, LB_GETCURSEL, 0, 0);
            if (i >= 0 && i + 1 < saved.nsessions)
                spawn_saved(saved.sessions[i + 1]);
        }
        break;
      case ID_ACCEL_NEW:
        spawn_session(L"");
        break;
      case ID_ACCEL_NEXT:
        step_session(+1);
        break;
      case ID_ACCEL_PREV:
        step_session(-1);
        break;
    }
}

static void poll_sessions(void)
{
    bool gone = false;

    for (size_t i = 0; i < nsessions; i++) {
        if (!IsWindow(sessions[i].hwnd))
            gone = true;
        else if (refresh_title((int)i)) {
            redraw_row((int)i);
            if ((int)i == active)
                update_caption();
        }
    }
    if (gone)
        prune_sessions();
}

static bool confirm_close(void)
{
    wchar_t msg[100];

    if (!nsessions)
        return true;
    swprintf(msg, lenof(msg), L"Close %zu open session%ls?", nsessions,
             nsessions == 1 ? L"" : L"s");
    return MessageBoxW(main_hwnd, msg, DECK_APPNAME,
                       MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2) == IDOK;
}

static UINT window_dpi(HWND hwnd)
{
    typedef UINT (WINAPI *GetDpiForWindow_t)(HWND);
    GetDpiForWindow_t fn = (GetDpiForWindow_t)(void *)GetProcAddress(
        GetModuleHandleA("user32.dll"), "GetDpiForWindow");
    return fn ? fn(hwnd) : 96;
}

static LRESULT CALLBACK main_wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    switch (msg) {
      case WM_CREATE:
        main_hwnd = hwnd;
        dpi = window_dpi(hwnd);
        make_fonts();
        create_children();
        make_fonts();
        return 0;
      case WM_ERASEBKGND:
        return 1;
      case WM_PAINT: {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(hwnd, &ps);
        paint_main(dc);
        EndPaint(hwnd, &ps);
        return 0;
      }
      case WM_SIZE:
        if (wp != SIZE_MINIMIZED)
            layout(in_sizemove);
        return 0;
      case WM_ENTERSIZEMOVE:
        in_sizemove = true;
        return 0;
      case WM_EXITSIZEMOVE:
        in_sizemove = false;
        layout(false);
        return 0;
      case WM_GETMINMAXINFO:
        ((MINMAXINFO *)lp)->ptMinTrackSize.x = S(640);
        ((MINMAXINFO *)lp)->ptMinTrackSize.y = S(360);
        return 0;
      case WM_DPICHANGED: {
        const RECT *r = (const RECT *)lp;
        dpi = HIWORD(wp);
        make_fonts();
        SetWindowPos(hwnd, NULL, r->left, r->top, r->right - r->left,
                     r->bottom - r->top, SWP_NOZORDER | SWP_NOACTIVATE);
        return 0;
      }
      case WM_ACTIVATE:
        if (LOWORD(wp) != WA_INACTIVE) {
            refresh_saved();
            PostMessageW(hwnd, WM_DECK_REFOCUS, 0, 0);
        }
        return 0;
      case WM_SETFOCUS:
        if (active >= 0)
            SetFocus(sessions[active].hwnd);
        return 0;
      case WM_DECK_REFOCUS:
        if (GetActiveWindow() == hwnd)
            refocus();
        return 0;
      case WM_COMMAND:
        on_command(LOWORD(wp), HIWORD(wp));
        return 0;
      case WM_MEASUREITEM: {
        MEASUREITEMSTRUCT *m = (MEASUREITEMSTRUCT *)lp;
        if (m->CtlType == ODT_LISTBOX)
            m->itemHeight = S(m->CtlID == ID_OPENLIST ? 32 : 28);
        return true;
      }
      case WM_DRAWITEM: {
        const DRAWITEMSTRUCT *d = (const DRAWITEMSTRUCT *)lp;
        if (d->CtlID == ID_OPENLIST)
            draw_session_row(d);
        else if (d->CtlID == ID_SAVEDLIST)
            draw_saved_row(d);
        else
            draw_button(d);
        return true;
      }
      case WM_CTLCOLORLISTBOX:
        return (LRESULT)br_sidebar;
      case WM_SETCURSOR:
        if (LOWORD(lp) == HTCLIENT) {
            POINT pt;
            GetCursorPos(&pt);
            ScreenToClient(hwnd, &pt);
            if (in_splitter(MAKELPARAM(pt.x, pt.y))) {
                SetCursor(LoadCursor(NULL, IDC_SIZEWE));
                return true;
            }
        }
        break;
      case WM_LBUTTONDOWN:
        if (in_splitter(lp)) {
            dragging_splitter = true;
            SetCapture(hwnd);
        }
        return 0;
      case WM_MOUSEMOVE:
        if (dragging_splitter) {
            int x = MulDiv((short)LOWORD(lp), 96, dpi);
            if (x < 150) x = 150;
            if (x > 500) x = 500;
            if (x != sidebar_width) {
                sidebar_width = x;
                layout(true);
            }
        }
        return 0;
      case WM_LBUTTONUP:
        if (dragging_splitter) {
            dragging_splitter = false;
            ReleaseCapture();
            layout(false);
            refocus();
        }
        return 0;
      case WM_TIMER:
        if (wp == TIMER_POLL) {
            SetTimer(hwnd, TIMER_POLL, 1000, NULL);
            poll_sessions();
        }
        return 0;
      case WM_CLOSE:
        if (confirm_close()) {
            for (size_t i = 0; i < nsessions; i++)
                PostMessageW(sessions[i].hwnd, WM_DECK_EXEC,
                             DECK_EXEC_QUIT, 0);
            DestroyWindow(hwnd);
        }
        return 0;
      case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

/* The arguments we were started with, for forwarding to a session */
static const wchar_t *own_arguments(void)
{
    const wchar_t *p = GetCommandLineW();
    bool quoted = false;

    for (; *p && (quoted || (*p != L' ' && *p != L'\t')); p++)
        if (*p == L'"')
            quoted = !quoted;
    while (*p == L' ' || *p == L'\t')
        p++;
    return p;
}

int deck_host_main(HINSTANCE inst, int show)
{
    WNDCLASSW wc;
    MSG m;
    HACCEL accel;
    wchar_t envval[32];
    static ACCEL accels[] = {
        { FCONTROL | FSHIFT | FVIRTKEY, 'T', ID_ACCEL_NEW },
        { FCONTROL | FVIRTKEY, VK_TAB, ID_ACCEL_NEXT },
        { FCONTROL | FSHIFT | FVIRTKEY, VK_TAB, ID_ACCEL_PREV },
    };
    const wchar_t *args = own_arguments();

    deck_inst = inst;
    br_sidebar = CreateSolidBrush(COL_SIDEBAR);
    br_toolbar = CreateSolidBrush(COL_TOOLBAR);
    br_content = CreateSolidBrush(COL_CONTENT);

    memset(&wc, 0, sizeof(wc));
    wc.hInstance = inst;
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.hIcon = LoadIconW(inst, MAKEINTRESOURCEW(IDI_MAINICON));
    wc.lpfnWndProc = main_wndproc;
    wc.lpszClassName = DECK_MAIN_CLASS;
    RegisterClassW(&wc);

    wc.lpfnWndProc = content_wndproc;
    wc.lpszClassName = DECK_CONTENT_CLASS;
    wc.hbrBackground = br_content;
    RegisterClassW(&wc);

    if (!CreateWindowExW(0, DECK_MAIN_CLASS, DECK_APPNAME,
                         WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                         CW_USEDEFAULT, CW_USEDEFAULT, 1200, 760,
                         NULL, NULL, inst, NULL))
        return 1;

    swprintf(envval, lenof(envval), L"%llx",
             (unsigned long long)(ULONG_PTR)content_hwnd);
    SetEnvironmentVariableW(DECK_PARENT_ENV, envval);

    refresh_saved();
    update_toolbar();
    layout(false);
    ShowWindow(main_hwnd, show);
    UpdateWindow(main_hwnd);
    SetTimer(main_hwnd, TIMER_POLL, 1000, NULL);

    if (*args)
        spawn_session(args);

    accel = CreateAcceleratorTableW(accels, lenof(accels));
    while (GetMessageW(&m, NULL, 0, 0) > 0) {
        if (TranslateAcceleratorW(main_hwnd, accel, &m))
            continue;
        TranslateMessage(&m);
        DispatchMessageW(&m);
    }
    return (int)m.wParam;
}
