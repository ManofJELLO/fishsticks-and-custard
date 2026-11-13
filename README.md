# Fishsticks & Custard

A Claude desktop assistant for Arch + Hyprland, written in Rust. You summon it with the
wake phrase *"fishsticks and custard"*, a hotkey, or the tray icon.
It runs on your Claude subscription through Claude Code's headless stream-json mode.

## Status

| Step | What | State |
|---|---|---|
| 1 | Tray icon, popup (translucent panel, dot-matrix status and visualiser), text input, `fsc` CLI, Claude session | **done** |
| 2 | Push-to-talk: cpal (PipeWire) → Silero VAD → whisper-rs (Vulkan) → Claude → Piper, spoken sentence by sentence; real FFT in the visualiser | **done** |
| 3 | Wake phrase "fishsticks and custard" (VAD-gated whisper, say a command in the same breath) | **done** |
| 4 | Safe system control: permission prompts routed to the popup | next |
| 5 | Polish: wake chime, echo cancel + barge-in, openWakeWord first stage, visuals | |

## Layout

```
crates/fsc-core   IPC protocol, config, model registry, Claude stream-json session (no GTK)
crates/fsc-audio  mic/speaker (cpal), Silero VAD + Piper (ONNX Runtime), whisper.cpp STT, spectrum
crates/fsc-app    `fscd`: GTK4 layer-shell popup, ksni tray, portal theme watcher, socket server
crates/fsc-cli    `fsc`: control CLI for Hyprland binds and scripts
voice-home/       defaults seeded into ~/.local/share/fsc/home (persona + tool allow-list)
packaging/        Hyprland snippet, example config
```

## Install

Build dependencies (Arch):

```sh
sudo pacman -S --needed clang vulkan-headers espeak-ng
```

`clang` generates C bindings, `vulkan-headers` builds whisper.cpp's GPU backend, and
`espeak-ng` provides the phoneme data Piper needs. The first build compiles
whisper.cpp and its Vulkan shaders. If your machine is sensitive to sustained
all-core load, limit it: `CMAKE_BUILD_PARALLEL_LEVEL=4 cargo build -j 4`.

```sh
cargo install --locked --root ~/.local --path crates/fsc-app
cargo install --locked --root ~/.local --path crates/fsc-cli
```

Download the speech models (~640 MB into `~/.local/share/fsc/models`), then check them:

```sh
fsc fetch-models
fscd --self-test     # Piper speaks a sentence, whisper transcribes it back, with timings
```

**Trust the assistant's workspace once.** Claude Code ignores the allow-list in
`~/.local/share/fsc/home/.claude/settings.json` until that directory is trusted.
Until then every tool is denied, which is safe but not useful:

```sh
cd ~/.local/share/fsc/home && claude    # accept the trust prompt, then /exit
```

Then paste `packaging/hyprland/fsc.lua` into `~/.config/hypr/hyprland.lua`.

## Use

| | |
|---|---|
| `SUPER+A` / tray click / `fsc type` | open the popup with the text field focused |
| `Enter` | send |
| `Esc` | hide; in a voice conversation this also hangs up (a reply keeps streaming as text, with a notification) |
| `Ctrl+C` (nothing selected) / `fsc stop` | interrupt the reply |
| `Ctrl+N` / `fsc new` | new conversation |
| `SUPER+SHIFT+A` hold / `fsc listen` + `fsc stop` | push-to-talk; a quick tap listens until you stop talking |
| `fsc ask "…"` | send without opening the field |
| *"Fishsticks and custard[, question]"* | wake it by voice; a question in the same breath is sent straight away |
| `fsc mute [on\|off]` / tray | mute the wake word |
| `fsc status` | `ready`, `thinking`, `listening`, … |

## Customise

- **Persona and reply style:** `~/.local/share/fsc/home/persona.md`
- **What Claude may run:** `~/.local/share/fsc/home/.claude/settings.json`. In headless mode,
  anything not allowed is denied automatically. Run `fsc new` after editing either file.
- **Look and model:** `~/.config/fsc/config.toml` (see `packaging/config.example.toml`).
- Preview the tray icons: `fscd --dump-icons /some/dir`.

## Wake phrase

When nothing else is using the mic, `fscd` keeps it open and watches for the phrase:
Silero VAD cuts speech into segments, each segment is transcribed locally by whisper on
the GPU (~0.25 s), and a fuzzy match checks whether it *starts* with the phrase. Nothing
is sent anywhere unless it does. Long segments are checked after 2.5 s and dropped early
if they don't start with the phrase. Mute it from the tray or with `fsc mute`.

This means the GPU does a little work whenever someone speaks nearby. A trained
openWakeWord model can be added later as a cheaper first stage.

Pick the mic with `voice.input_device` (a substring of its name, e.g. `"C920"`). A wired
or USB mic is best for an always-on wake word; see below for Bluetooth headsets.

## Bluetooth headsets

Earbuds in high-quality playback mode (A2DP) have no microphone. When `fscd` opens the
mic, WirePlumber switches them to headset mode (HFP), which takes about 1–5 s. The popup
says **WAITING FOR MIC…** until audio actually arrives. The mic then stays open until the
spoken reply finishes (`voice.hold_mic_while_speaking`), because closing it flips the
headset back and removes the device the reply would play on. Replies therefore play at
headset quality. An always-on wake word (step 3) works better with a separate mic.

**Conversation mode** (`voice.conversation`, on by default): after a spoken reply it
listens again automatically, like a phone call, keeping the mic and a warm output
stream open (with inaudible dither so earbuds don't sleep and clip the next reply).
Closing the popup (Esc) hangs up and releases the headset. While a conversation is
open, everything it hears is sent to Claude; the red LISTENING status shows the line
is live.

## Design notes

- **Theme:** follows `org.freedesktop.appearance color-scheme` from the XDG settings
  portal, live. The popup, visualiser and tray icon all switch together.
- **Panel:** translucent, most see-through behind the visualiser and most solid behind
  text, with faint halos behind lit dots for contrast. The Hyprland `layer_rule` blur
  is optional; it uses your global blur settings.
- **Wayland only:** the popup is a layer-shell surface, so there is no X11 fallback.
- **Type:** 0xProto Nerd Font for body text. The status label uses a built-in 5×7
  dot-matrix font drawn as circles, matching the visualiser grid.
- **Colour:** monochrome. Red (`#D71921`) marks state only: recording, peaks and errors.
