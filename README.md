# xmouse-psnbridge

A plugin for XMouse (the mouse-and-keyboard console streaming client) that adds PlayStation
Remote Play: it finds PS5 and PS4 consoles on your home network, pairs with them, wakes them from
rest mode, and streams the game to XMouse while XMouse's mouse-and-keyboard mapper drives the
controller.

It is a plugin, not a program you use by itself. XMouse starts it as a separate process and
talks to it over stdin/stdout (XMouse plugin API 1). XMouse does the decoding, display, sound
and input mapping; this plugin does everything PlayStation-specific.

The Remote Play protocol is not implemented in this repository. It comes from an existing
open-source library (see [Credits](#credits)); this project is the glue between that library
and XMouse's plugin protocol.

## Status: untested on real hardware

**Nobody who worked on this plugin owns a PlayStation.** Everything that can be checked
without a console is covered by automated tests (the XMouse protocol, controller mapping,
account ID handling, settings, video unit assembly). Everything that involves a real console
is untested: discovery, pairing, wake-up, connecting, video, audio, input and rumble. Expect
problems, and please report what you find.

## Install

**The easy way:** in XMouse, choose PlayStation on the "Which console do you play?" screen and
click **Install PSNBridge** (or More > Plugins > PSNBridge). XMouse downloads the latest release
from this repository and installs it.

**By hand:**

1. Download `xmouse-psnbridge.zip` from the latest release (or build it, see [Build](#build)).
2. In XMouse, open More > Plugins folder (that is `%APPDATA%\XMouse\config\plugins`).
3. Extract the zip there, so you get `plugins\psnbridge\plugin.json` and
   `plugins\psnbridge\xmouse-psnbridge.exe`.
4. Restart XMouse. PlayStation can now be chosen as your console.

The folder must be called `psnbridge` (XMouse requires the folder name to match the plugin
ID). The plugin keeps its own files in `psnbridge\data\`.

## Set the PSN account ID

A console only accepts a pairing that names the PlayStation Network account signed in on it,
by its account ID: an 8-byte number that is not your sign-in name or e-mail address. You need
to set it once, before pairing:

```
cd %APPDATA%\XMouse\config\plugins\psnbridge
xmouse-psnbridge --set-account <id>
```

`<id>` can be written as

- base64, the form the chiaki tools print, e.g. `AbCdEfGhIjk=`;
- 16 hex digits (the same eight bytes in the same order);
- `dec:` followed by the account number in decimal, e.g. `dec:1234567890123456789`.

`xmouse-psnbridge --show-account` prints what is stored.

To find your account ID, use one of the upstream projects' tools: chiaki-ng obtains it when
you sign in to PSN in its settings, and chiaki ships a script, `psn-account-id.py`, that
prints it as base64 after you sign in to PSN in your browser. This plugin does not sign in
to PSN and never sees your password.

XMouse asks for the ID when you choose PlayStation as your console and passes it with each pairing request (the `extra` map of the `pair` message,
key `account_id`); it is then stored the same way.

## Pairing

1. On the console: Settings > System > Remote Play, switch on "Enable Remote Play", then
   open "Link Device" (PS4: Settings > Remote Play Connection Settings > Add Device). The
   console shows an 8-digit PIN for a few minutes.
2. In XMouse, choose the console from the PlayStation list and enter the PIN.

The plugin sends the PIN and your account ID to the console over your home network. The
console answers with two keys, which the plugin stores in
`psnbridge\data\psnbridge.json`. From then on no PIN is needed. Those keys let anyone on your
network start Remote Play on that console, so treat the file like a password. Deleting it
(or the console's entry in it) unpairs this PC from the plugin's side.

If pairing fails, the usual reasons are: the PIN expired or was mistyped, the account ID
belongs to a different account than the one signed in on the console, or the console is in
rest mode (it must be on, showing the PIN).

## Using it

- **Wake:** a paired console in rest mode can be woken from XMouse, if "Enable Turning On
  PS5 from Network" (PS4: "Enable Turning On PS4 from Network") is switched on in the
  console's power saving settings. Starting a session wakes it automatically.
- **Picture:** H.264 at the largest of the console's sizes that fits what XMouse asks for
  (360p, 540p, 720p, 1080p; 1080p only on PS5 and PS4 Pro), at 30 or 60 frames per second.
- **TV mode:** in XMouse's "tv" mode the plugin asks the console to send no video or sound
  and only forwards your input.
- **Controller mapping** (XMouse's Xbox layout to the PlayStation pad):

  | XMouse / Xbox | PlayStation |
  | --- | --- |
  | A | Cross |
  | B | Circle |
  | X | Square |
  | Y | Triangle |
  | LB / RB | L1 / R1 |
  | LT / RT | L2 / R2 (analog) |
  | Left / right stick | Left / right stick |
  | Left / right stick click | L3 / R3 |
  | D-pad | D-pad |
  | View | Create / Share |
  | Menu | Options |
  | Nexus | PS |

  The touchpad, motion sensors, microphone and DualSense haptics and adaptive triggers have
  no counterpart in XMouse's plugin protocol and are not available. Rumble is passed on.

### Limits

- Home network only. Playing over the internet through Sony's servers is a different
  connection path and is not included.
- If the console's user account has a login passcode, the console asks for it when Remote
  Play starts. XMouse's plugin protocol has no way to ask you for it, so the session ends
  with a message saying so.
- Consoles are found by a broadcast on this PC's main network connection, plus a direct
  question to the last known address of each paired console.

## Build

Windows, with the Rust MSVC toolchain:

```
cargo +stable-x86_64-pc-windows-msvc build --release
cargo +stable-x86_64-pc-windows-msvc test
```

To make the release zip XMouse installs from, run `.\tools\package.ps1`: it builds the release
program and writes `dist\xmouse-psnbridge.zip` with a `psnbridge` folder holding the program,
`plugin.json`, `LICENSE` and this README. Attach it to a GitHub release under exactly that name.

The program is `target\release\xmouse-psnbridge.exe`. `xmouse-psnbridge --self-test` runs a
short built-in check (no network or console needed). The tests and the self-test never open
an audio device and never contact a console.

Set the environment variable `XMOUSE_PSNBRIDGE_LOG` to `debug` or `trace` for more detail
from the protocol library in XMouse's log.

## Credits

All of the Remote Play protocol work is other people's:

- **chiaki-rust** by muentzee, a Rust port of chiaki-ng's library. This plugin depends on
  its `chiaki-core` crate (protocol, encryption, error correction), pulled in by Cargo as a
  git dependency:
  <https://github.com/muentzee/chiaki-rust>, commit
  `7b95e825389ace25307d20c9c89e2d5fc906c474` (version 1.11.4), AGPL-3.0-only.
  The exact revision is recorded in `Cargo.toml` and `Cargo.lock`.
- **chiaki-ng** by Street Pea and contributors, which chiaki-rust is ported from:
  <https://github.com/streetpea/chiaki-ng>
- **Chiaki** by Florian Märkl (thestr4ng3r), the original open-source Remote Play client
  that both build on: <https://git.sr.ht/~thestr4ng3r/chiaki>

None of them are involved in or responsible for this plugin.

## Licence

Copyright (C) the xmouse-psnbridge authors.

This program is free software: you can redistribute it and/or modify it under the terms of
the GNU Affero General Public License, version 3, as published by the Free Software
Foundation. It is distributed without any warranty. The full text is in [LICENSE](LICENSE).

The source code is at <https://github.com/thedarkcr0w/xmouse-psnbridge>. A built program
contains chiaki-rust's `chiaki-core` (AGPL-3.0-only; its files carry upstream's
"AGPL-3.0-only with OpenSSL exception" notice) and the other Rust crates listed in
`Cargo.lock` under their own licences; the source of every one of them is fetched by Cargo
at the versions recorded there.

XMouse itself is a separate program under a different licence (CC BY-NC-SA). The two do not
share code: XMouse runs the plugin as its own process and exchanges messages with it.

## A note on Sony's terms

This is not a Sony product and has no connection to Sony Interactive Entertainment.
"PlayStation", "PS5", "PS4", "DualSense" and "DualShock" are trademarks of Sony Interactive
Entertainment Inc.

Sony's terms for PlayStation Network and the system software do not permit using
third-party Remote Play clients such as this one. Using it is your own decision and at your
own risk.
