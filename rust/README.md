# PuTTYDeck (Rust)

PuTTY's programs rewritten in Rust, with terminal sessions in one window, listed
on the left. Every session runs inside one process. SSH and its cryptography come
from [russh](https://crates.io/crates/russh), the terminal from
[alacritty_terminal](https://crates.io/crates/alacritty_terminal), and the rest
is written here, following PuTTY's source and file formats.

| PuTTY | Here | Notes |
| --- | --- | --- |
| PuTTY | `puttydeck` | Windows program, many sessions in one window |
| Plink | `plink` | Console sessions, scripts and tunnels |
| PSCP | `pscp` | SFTP, or the old SCP protocol with `-scp` or when there's no SFTP |
| PSFTP | `psftp` | Interactive, or `-b` for a batch file |
| PuTTYgen | `puttygen` | Command line, like PuTTY's `cmdgen`. No window |
| Pageant | `pageant` | Runs in a console. No tray icon |

It reads your PuTTY saved sessions and trusted host keys, and never changes them.
Anything saved here goes in `HKCU\Software\PuTTYDeck`, and a session saved there
takes the place of a PuTTY session with the same name.

## Using it

Type `user@host` at the top left and press Enter, or double-click a saved
session. Right-click the saved sessions list to change a session's settings or
make a new one. `telnet://host`, `rlogin://host`, `supdup://host`, `raw://host:port`
and `serial://COM3` also work. Login prompts appear in the terminal, as they do
in PuTTY.

    puttydeck -ssh user@host
    puttydeck -load "saved session"
    puttydeck -i key.ppk -L 8080:web:80 -D 1080 user@host

### Copy and paste

| Action | How |
| --- | --- |
| Copy | Select with the mouse: it's copied immediately |
| Copy | `Ctrl+C` while text is selected, `Ctrl+Shift+C`, or the Copy button |
| Paste | Right-click, `Ctrl+V`, `Ctrl+Shift+V`, or the Paste button |
| Copy a whole session's output | Copy all button |
| Paste into every session | Paste to all button (asks first) |

`Ctrl+C` with nothing selected is an interrupt. `Ctrl+V` goes to the program in
full-screen apps such as vim, and `Ctrl+Shift+V` always pastes. When a program
uses the mouse, hold Shift to select text.

### Keyboard

| Keys | Action |
| --- | --- |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous session |
| `Ctrl+Shift+1` to `9` | Go to session 1 to 9 |
| `Ctrl+Shift+T` | New session |
| `Ctrl+Shift+D` | Duplicate session |
| `Ctrl+Shift+W` | Close session |
| `Shift+PgUp` / `Shift+PgDn` | Scroll back and forward |

Right-click a session in the list for more, or middle-click to close it. A bell
in a background session puts a `*` next to it.

## What's included

- Protocols: SSH-2, Telnet, Rlogin, SUPDUP, raw and serial
- Proxies: SOCKS 4 and 5, HTTP with Basic or Digest login, telnet, local command,
  and SSH
- Forwarding: local, remote and dynamic ports, Pageant, and X11 with MIT or XDM
  logins
- Login: passwords, keyboard-interactive, keys, OpenSSH certificates, Pageant and
  PuTTY's authentication plugins
- Keys: `.ppk` versions 2 and 3, encrypted or not, and OpenSSH files
- Logging, remote commands, environment variables, keepalives and per-session
  colours and fonts

## What isn't

`deck-core/src/options.rs` lists every one of PuTTY's saved settings as working,
not applicable, or not done with the reason, and a test checks that list against
PuTTY's. A setting that isn't done is still kept, so a saved session loses
nothing. What's left can't be done with the libraries this is built on:

- GSSAPI and Kerberos login, SSH-1 and single DES, which russh doesn't have.
  A session set to SSH-1 only is refused rather than connected over SSH-2
- PuTTY's workarounds for old servers: russh makes those choices, and may refuse
  very old servers
- Background colour erase, blinking text, right-to-left text and PuTTY's
  double-width and line-drawing options, which alacritty_terminal has no way to do

The tools also differ: `puttygen` is command line only, `pageant` runs in a
console with no tray icon, and the Rlogin window size is sent without waiting for
the server's urgent request.

Some things differ on purpose, for safety:

- Pasting several lines asks first, unless the program is using bracketed paste.
  `PasteWarnLines` turns this off
- An HTTP proxy only gets a password if it offers Basic or Digest, and Basic
  shows a warning
- `pscp -scp` refuses files other than the one asked for. Wildcards need `-unsafe`
- A proxy or pre-connect command won't run if a host name, user name or password
  it uses has a character `cmd.exe` would act on
- Pageant is reached by its window message, not its named pipe
- `puttygen` asks for a passphrase on a new key and keeps a converted key's.
  `--no-passphrase` writes one without
- Session logs hold what the host sent, not this end's prompts
- Hosts can't hold the display back with synchronized updates

And some differ for other reasons:

- Algorithms PuTTY lists below "warn" aren't offered at all, because russh has
  nowhere to stop and ask
- `Ctrl+Shift+C` and `V` always work, whatever `CtrlShiftCV` says
- Connection sharing works between sessions in one program. It can't join a
  connection held by a separate PuTTY, and forwardings on a joining session
  aren't set up
- Rich text copying keeps the text and font, not its colours
- Resizing a session can't change its font

## Building

Needs Rust and the Visual Studio 2022 Build Tools (C++).

    build.cmd

The results are in `dist`. The installer is per-user and unsigned, so Windows
SmartScreen may warn about it on another PC.

## Testing

    cargo test -p deck-core -p deck-tools
    cargo test -p puttydeck --features test-hooks
    cargo test -p deck-core --test sftp -- --ignored --test-threads=1

The first two run real servers on localhost. The app tests drive a special build
that opens off-screen, can't take focus and has a private clipboard, so they're
safe to run while you use the PC. The third has the real PuTTY's `plink.exe` log
in with key files and a Pageant from here, so it needs PuTTY installed, the debug
tools built, and no Pageant running.

## Licence

PuTTYDeck follows PuTTY, copyright Simon Tatham and contributors, under the MIT
licence: see `LICENCE`. It is unofficial, and not a PuTTY project release. russh
and alacritty_terminal are under the Apache 2.0 licence; before redistributing
the binaries, include their licence notices.
