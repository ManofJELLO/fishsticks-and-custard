# Fishsticks & Custard

A voice desktop assistant for Arch + Hyprland, written in Rust. You summon it with the
wake phrase *"fishsticks and custard"*, a hotkey, or the tray icon.
It runs on the AI subscription you already have: Claude (through Claude Code), ChatGPT
(through Codex) or Google (through Antigravity CLI). See [Choosing an agent](#choosing-an-agent).

## Status

| Step | What | State |
|---|---|---|
| 1 | Tray icon, popup (translucent panel, dot-matrix status and visualiser), text input, `fsc` CLI, Claude session | **done** |
| 2 | Push-to-talk: cpal (PipeWire) → Silero VAD → whisper-rs (Vulkan) → Claude → Piper, spoken sentence by sentence; real FFT in the visualiser | **done** |
| 3 | Wake phrase "fishsticks and custard" (VAD-gated whisper, say a command in the same breath) | **done** |
| 4 | Safe system control: permission prompts routed to the popup | **done** |
| 5 | Polish: police-box tray icon, LED-segment visualiser, window/layer/headless modes, spoken-reply cleanup | **done** |
| 6 | Any agent: Codex and Antigravity through the Agent Client Protocol, beside Claude Code | in progress |

## Layout

```
crates/fsc-core   IPC protocol, config, model registry, agent sessions: Claude stream-json, ACP (no GTK)
crates/fsc-audio  mic/speaker (cpal), Silero VAD + Piper (ONNX Runtime), whisper.cpp STT, spectrum
crates/fsc-app    `fscd`: GTK4 popup (window or layer-shell), ksni tray, portal theme/accent watcher, socket server
crates/fsc-cli    `fsc`: control CLI for Hyprland binds and scripts
voice-home/       defaults seeded into ~/.local/share/fsc/home (persona + tool allow-list)
packaging/        Hyprland snippet, example config, Arch PKGBUILD + signing key
.github/workflows CI: build, sign and publish the pacman repository
```

## Install (pacman repository)

Signed packages are built by GitHub Actions for every release and published to this
repo's [`repo` release](https://github.com/ManofJELLO/fishsticks-and-custard/releases/tag/repo).
Updates then arrive with a normal `pacman -Syu`.

1. Trust the packaging key:

   ```sh
   curl -sL https://raw.githubusercontent.com/ManofJELLO/fishsticks-and-custard/main/packaging/arch/fishsticks-and-custard.asc \
     | sudo pacman-key --add -
   sudo pacman-key --lsign-key 3286286EB33C3D8E9B47D6EF0A83688698B6FA30
   ```

2. Add the repository to `/etc/pacman.conf`:

   ```ini
   [fishsticks-and-custard]
   Server = https://github.com/ManofJELLO/fishsticks-and-custard/releases/download/repo
   ```

3. Install: `sudo pacman -Sy fishsticks-and-custard`, then follow the printed setup steps
   (agent login, `fsc fetch-models`, Hyprland lines).

You also need an agent, signed in with your account: the [Claude Code](https://claude.com/claude-code)
CLI by default, or one of the others in [Choosing an agent](#choosing-an-agent).

## Choosing an agent

Set `backend` in the `[agent]` section of `~/.config/fsc/config.toml`, then restart
`fscd` (`fsc quit; fscd &`).

| `backend` | Needs | Sign in with |
|---|---|---|
| `"claude"` (default) | Claude Code CLI (`claude`) | `claude`, once, interactively |
| `"codex"` | Codex CLI and `codex-acp` (AUR `codex-acp`, or `npm i -g @agentclientprotocol/codex-acp`) | `codex login` |
| `"antigravity"` | Antigravity CLI `agy` 1.5 or newer (AUR `antigravity-cli`) | `agy acp --login` |
| `"acp"` | any [Agent Client Protocol](https://agentclientprotocol.com) agent: set `command` and `args` | the agent's own way |

```toml
[agent]
backend = "codex"
# model = "gpt-6-luna[low]"     # as the agent lists it; empty = its default
# mode = "read-only"            # the agent's approval mode; Codex defaults to read-only here
```

Claude runs through Claude Code's own stream-json mode, with its settings in `[claude]`.
The others speak ACP: each session gets the persona as a system prompt, conversations
rotate to fresh sessions in the same agent process, and permission questions arrive
on the same card (see [Permission questions](#permission-questions)). ACP agents work in
`~/.local/share/fsc/home/workspace`, an empty folder of their own. Agents let themselves
write there freely (Antigravity also writes in `/tmp` without asking), but changes to the
assistant's own files and to anything of yours are asked about. Reading, searching, web
fetches and thinking run without asking; set `auto_allow` to change which tool kinds do.
If an agent isn't signed in, the popup says which command to run.

## Build from source

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

Or build the same package the repository ships: `cd packaging/arch && makepkg -si`.

For a launcher entry (rofi `drun`, etc.) when installing with cargo:

```sh
install -Dm644 packaging/desktop/dev.fsc.FishsticksAndCustard.desktop -t ~/.local/share/applications/
install -Dm644 packaging/desktop/dev.fsc.FishsticksAndCustard.svg -t ~/.local/share/icons/hicolor/scalable/apps/
```

### Releasing

Bump `version` in `Cargo.toml` and `pkgver` in `packaging/arch/PKGBUILD` (they must match),
commit, then `git tag vX.Y.Z && git push --tags`. The `package` workflow builds, signs and
publishes it.

Download the speech models (~640 MB into `~/.local/share/fsc/models`), then check them:

```sh
fsc fetch-models
fscd --self-test     # Piper speaks a sentence, whisper transcribes it back, with timings
```

Then paste `packaging/hyprland/fsc.lua` into `~/.config/hypr/hyprland.lua`.

The package installs a launcher entry, **Fishsticks & Custard**. Opening it starts `fscd`, or
toggles the popup if it's already running. Its actions (Type, Talk, New conversation, Quit)
show in rofi with `rofi -show drun -drun-show-actions`.

## Use

| | |
|---|---|
| `SUPER+A` / tray click / `fsc type` | open the popup with the text field focused |
| `Enter` | send |
| `Esc` | hide; in a voice conversation this also hangs up (a reply keeps streaming as text, with a notification) |
| `Ctrl+C` (nothing selected) / `fsc stop` | interrupt the reply |
| `Ctrl+N` / `fsc new` | new conversation |
| `SUPER+SHIFT+A` hold / `fsc listen` + `fsc release` | push-to-talk; a quick tap listens until you stop talking |
| `fsc ask "…"` | send without opening the field |
| **ALLOW / ALWAYS / DENY** / `fsc allow [--always]` / `fsc deny` | answer a permission question; by voice, say *yes*, *always* or *no*; typing `y` or `n` works too |
| *"Fishsticks and custard[, question]"* | wake it by voice; a question in the same breath is sent straight away |
| `fsc mute [on\|off]` / tray | mute the wake word |
| `fsc status` | `ready`, `thinking`, `listening`, … |
| `fsc memory` | show what it remembers between sessions |
| `fsc forget` / tray **Forget memories** | delete that memory and start a fresh session |

## Customise

- **Persona and reply style:** `~/.local/share/fsc/home/persona.md`
- **What Claude may run** (Claude only; ACP agents keep their own rules):
  `~/.local/share/fsc/home/.claude/settings.local.json`. Anything
  on `allow` just runs, anything on `deny` never does, and anything else is asked about (see
  below). **ALWAYS** answers are added here. Run `fsc new` after editing it or the persona.
  (Local settings are used because Claude Code ignores a folder's shared `settings.json`
  allow-list until you've trusted the folder interactively.) File edits (`Edit`, `Write`)
  are asked about. Versions before 0.6.4 denied them; updating removes those three deny rules
  once, so add them back to `deny` if you'd rather it never edits files.
- **Look and model:** `~/.config/fsc/config.toml` (see `packaging/config.example.toml`).
- Preview the tray icons: `fscd --dump-icons /some/dir`.

## Permission questions

When the agent wants a tool that isn't allowed or denied (a command, a web page, a file
write), the reply pauses: the status turns red (**ALLOW?**) and a card shows what it wants
to do and the exact command, URL or path, with **ALLOW**, **ALWAYS** and **DENY**. After a
spoken question it also asks out loud ("May I search the web for…? Yes, always, or no?")
and listens for the answer. Closing the popup counts as no. **ALWAYS** saves the rule
Claude Code suggests, so the same kind of request just runs next time. With ACP agents it
picks the agent's own "always allow" option, when it offers one.

## Sessions and memory

One session lasts while you're actively talking to it. After
`agent.session_idle_minutes` (default 20) without an exchange, or on `fsc new` / Ctrl+N,
or when `fscd` quits, the session ends and a new one starts, so context doesn't grow
forever. Older exchanges stay visible in the popup above a **NEW SESSION** divider.

Before a session ends, the agent is asked (hidden) to update a short list of durable facts
about you: preferences, location, people, setup, ongoing plans, things you asked it to
remember. It leaves out one-off questions and sensitive details. The list is saved to
`~/.local/share/fsc/home/memory.md`, a plain file you can read or edit, and is loaded
into every new session. `fsc memory` shows it, `fsc forget` deletes it, and
`agent.memory = false` turns the feature off. (Both were in `[claude]` before 0.7,
and are still read from there.) Saving costs one small extra turn per
session.

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
Closing the popup (Esc) hangs up and releases the headset. A minute with nobody
speaking (`voice.hang_up_after_secs`) hangs up too, and the popup closes itself. While a conversation is open, everything
it hears is sent to Claude; the red LISTENING status shows the line is live.

**Talking over a reply:** say the wake phrase while Claude is thinking or speaking
("fishsticks and custard, actually…") to stop it and ask something else. Pressing the
talk shortcut does the same.

## Design notes

- **Surface:** `ui.surface = "window"` (default) is a normal window, so Hyprland's
  window rules give it border, shadow, rounding, blur, opacity and animation (see
  `packaging/hyprland/fsc.lua`). `"layer"` is a layer-shell popup pinned to an edge or
  corner (`ui.anchor`, `ui.margin`, `ui.margin_x`, `ui.monitor`), with its own
  `ui.opacity` and optional `ui.gradient`. `"headless"` shows nothing: talk to it, the
  tray icon shows the state, permission questions and typed replies arrive as
  notifications.
- **Theme:** panel and text colours come from your GTK theme, the accent from the
  desktop (or the theme's selection colour; `ui.accent` pins one). Light/dark for the
  tray icon follows `org.freedesktop.appearance color-scheme`, live.
- **Wayland only:** GDK runs on Wayland only (layer mode needs layer-shell).
- **Type:** 0xProto Nerd Font for body text. The status label uses a built-in 5×7
  dot-matrix font drawn as circles.
- **Visualiser:** chunky LED-segment bars rising from the bottom, like an old car-stereo
  spectrum analyser (`ui.visualiser = "spectrum"`, `ui.bands`, `ui.block_percent`), or a
  scrolling loudness history (`"loudness"`). Red while listening, white for replies.
- **Tray icon:** a blue police box with white windows (`ui.tray_icon = "police-box"`) or a fishbone at 45°
  (`"fishbone"`). A red dot in the corner means the mic is recording.
- **Colour:** the accent marks state only: listening, permission questions. Errors use
  the theme's error colour.
