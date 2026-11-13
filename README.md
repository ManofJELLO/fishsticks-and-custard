# Fishsticks & Custard

A Claude desktop assistant for Arch + Hyprland, written in Rust. You summon it with the
wake phrase *"fishsticks and custard"* (step 3), a hotkey, or the tray icon.
It runs on your Claude subscription through Claude Code's headless stream-json mode.

## Status

| Step | What | State |
|---|---|---|
| 1 | Tray icon, popup (fluted glass, dot-matrix status and visualiser), text input, `fsc` CLI, Claude session | **done** |
| 2 | Hotkey push-to-talk: cpal → Silero VAD → whisper-rs (Vulkan) → Piper; real FFT in the visualiser | next |
| 3 | PipeWire echo-cancel + openWakeWord "fishsticks and custard" | |
| 4 | Safe system control: permission prompts routed to the popup | |
| 5 | Polish: barge-in, conversation memory, overlay tweaks | |

## Layout

```
crates/fsc-core   IPC protocol, config, Claude stream-json session (no GTK)
crates/fsc-app    `fscd`: GTK4 layer-shell popup, ksni tray, portal theme watcher, socket server
crates/fsc-cli    `fsc`: control CLI for Hyprland binds and scripts
voice-home/       defaults seeded into ~/.local/share/fsc/home (persona + tool allow-list)
packaging/        Hyprland snippet, example config
```

## Install

```sh
cargo install --locked --root ~/.local --path crates/fsc-app
cargo install --locked --root ~/.local --path crates/fsc-cli
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
| `Esc` | hide (a reply keeps streaming; you get a notification when it finishes) |
| `Ctrl+C` (nothing selected) / `fsc stop` | interrupt the reply |
| `Ctrl+N` / `fsc new` | new conversation |
| `fsc ask "…"` | send without opening the field |
| `fsc mute [on\|off]` | mute the wake word (step 3) |
| `fsc status` | `ready`, `thinking`, `listening`, … |

## Customise

- **Persona and reply style:** `~/.local/share/fsc/home/persona.md`
- **What Claude may run:** `~/.local/share/fsc/home/.claude/settings.json`. In headless mode,
  anything not allowed is denied automatically. Run `fsc new` after editing either file.
- **Look and model:** `~/.config/fsc/config.toml` (see `packaging/config.example.toml`).
- Preview the tray icons: `fscd --dump-icons /some/dir`.

## Design notes

- **Theme:** follows `org.freedesktop.appearance color-scheme` from the XDG settings
  portal, live. The popup, visualiser and tray icon all switch together.
- **Fluted glass:** clients can't sample what's behind them on Wayland, so Hyprland
  blurs the backdrop (the `layer_rule`) and the app draws translucent vertical reeds
  on top of the blur.
- **Type:** 0xProto Nerd Font for body text. The status label uses a built-in 5×7
  dot-matrix font drawn as circles, matching the visualiser grid.
- **Colour:** monochrome. Red (`#D71921`) marks state only: recording, peaks and errors.
