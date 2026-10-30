/*
 * deck.h: glue between PuTTY's terminal window (window.c) and the
 * PuTTYDeck host window (deck.c).
 *
 * A single puttydeck.exe plays both roles. Started normally it is the
 * host, which owns the sidebar. Every session is a further run of the
 * same executable, found by PUTTYDECK_PARENT in its environment, whose
 * terminal is created as a child window of the host's content panel.
 * Using the environment means the sessions PuTTY spawns for itself
 * (New Session, Duplicate Session) join the same deck automatically.
 */

#ifndef PUTTY_WINDOWS_DECK_H
#define PUTTY_WINDOWS_DECK_H

#define WM_DECK_ATTACH (WM_APP + 0x200) /* wParam = terminal HWND */
#define WM_DECK_DETACH (WM_APP + 0x201) /* wParam = terminal HWND */
#define WM_DECK_TITLE  (WM_APP + 0x202) /* wParam = terminal HWND */
#define WM_DECK_BELL   (WM_APP + 0x203) /* wParam = terminal HWND */
#define WM_DECK_CMD    (WM_APP + 0x204) /* terminal to host: DECK_CMD_* */
#define WM_DECK_EXEC   (WM_APP + 0x205) /* host to terminal: DECK_EXEC_* */

enum {
    DECK_CMD_NEW,
    DECK_CMD_CLOSE,
    DECK_CMD_NEXT,
    DECK_CMD_PREV,
    DECK_CMD_SELECT,                   /* lParam = zero-based index */
};

enum {
    DECK_EXEC_COPY,
    DECK_EXEC_PASTE,
    DECK_EXEC_COPYALL,
    DECK_EXEC_DUPLICATE,
    DECK_EXEC_QUIT,
};

#ifdef PUTTYDECK
extern HWND deck_parent;               /* NULL when we're not embedded */
bool deck_find_parent(void);
int deck_host_main(HINSTANCE inst, int show);
#else
#define deck_parent ((HWND)NULL)
#endif

#endif /* PUTTY_WINDOWS_DECK_H */
