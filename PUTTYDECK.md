# PuTTYDeck

PuTTY 0.85 with all your sessions in one window. Open sessions are listed on
the left; click one to switch to it. It is a single `puttydeck.exe` with no
installer or runtime to add, and it uses your existing PuTTY saved sessions and
known host keys.

Each session is still a separate PuTTY process, embedded as a child window, so
one session crashing or hanging doesn't take the others with it.

## Using it

Run `puttydeck.exe`. **New session** opens the normal PuTTY configuration
box; double-clicking a saved session connects straight away. Arguments are
passed to the first session, for example `puttydeck.exe -ssh user@host`.

Right-click a session in the list for more, middle-click to close it, or use
the × on its row.

### Copy and paste

The clipboard is shared, so copying in one session and pasting in another just
works.

| Action | How |
| --- | --- |
| Copy | Select with the mouse: it's copied immediately |
| Copy | `Ctrl+C` while text is selected, `Ctrl+Shift+C`, or the Copy button |
| Paste | Right-click, `Ctrl+V`, `Ctrl+Shift+V`, or the Paste button |
| Copy a whole session's output | Copy all button |
| Paste into every session | Paste to all button (asks first) |

So that these don't get in the way of the terminal:

- `Ctrl+C` with nothing selected is still an interrupt. With text selected, the
  first press copies and clears the selection, and the next one interrupts.
- `Ctrl+V` goes to the program in full-screen apps such as vim and nano, which
  use it themselves. `Ctrl+Shift+V` always pastes. At a normal shell prompt
  `Ctrl+V` pastes, so bash's quoted-insert isn't available there.

### Keyboard

| Keys | Action |
| --- | --- |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous session |
| `Ctrl+Shift+1` to `9` | Go to session 1 to 9 |
| `Ctrl+Shift+T` | New session |
| `Ctrl+Shift+D` | Duplicate session |
| `Ctrl+Shift+W` | Close session |

`Ctrl`+right-click in a terminal gives PuTTY's usual menu (Change Settings,
Event Log and so on). A bell in a background session turns its dot amber.

## Known limitations

- Moving the window to a monitor with a different scale doesn't rescale
  terminal text in sessions that are already open. New sessions are fine.
- Full Screen is disabled, because the deck owns the window.
- The list of sessions isn't restored on the next launch.

## Building

Needs the Visual Studio 2022 Build Tools (C++), which include CMake and Ninja.

    build-deck.cmd

The result is `build-deck\puttydeck.exe`. This branch is PuTTY 0.85 plus the deck, with the host in `windows\deck.c` and `windows\deck.h`.
The changes to PuTTY itself are guarded by `deck_parent`, so the ordinary
`putty` target still builds as before.

## Licence

PuTTY is copyright Simon Tatham and contributors, under the MIT licence: see
`LICENCE`. PuTTYDeck is an unofficial modification, not a PuTTY project release.
