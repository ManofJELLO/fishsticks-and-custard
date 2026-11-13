use clap::{Parser, Subcommand, ValueEnum};
use fsc_core::ipc::{self, Command};
use std::process::ExitCode;

mod fetch;

/// Control the Fishsticks & Custard assistant daemon (fscd).
#[derive(Parser)]
#[command(name = "fsc", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the popup if hidden, hide it if shown
    Toggle,
    /// Show the popup
    Show,
    /// Hide the popup
    Hide,
    /// Show the popup with the text field focused
    Type,
    /// Start voice input now, skipping the wake word
    Listen,
    /// Stop listening, or interrupt the current reply
    Stop,
    /// Push-to-talk key released: finish recording (never interrupts a reply)
    Release,
    /// Send a prompt
    Ask {
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Start a fresh conversation
    New,
    /// Mute or unmute the wake word
    Mute {
        #[arg(value_enum, default_value_t = MuteMode::Toggle)]
        mode: MuteMode,
    },
    /// Print the current state
    Status,
    /// Show what the assistant remembers between sessions
    Memory,
    /// Delete everything the assistant remembers and start a fresh session
    Forget,
    /// Quit the daemon
    Quit,
    /// Download any missing speech models (whisper, silero VAD, piper voice)
    FetchModels,
}

#[derive(Clone, Copy, ValueEnum)]
enum MuteMode {
    On,
    Off,
    Toggle,
}

fn main() -> ExitCode {
    let cmd = match Cli::parse().cmd {
        Cmd::FetchModels => return fetch::run(),
        Cmd::Toggle => Command::Toggle,
        Cmd::Show => Command::Show,
        Cmd::Hide => Command::Hide,
        Cmd::Type => Command::Type,
        Cmd::Listen => Command::Listen,
        Cmd::Stop => Command::Stop,
        Cmd::Release => Command::Release,
        Cmd::Ask { text } => Command::Ask { text: text.join(" ") },
        Cmd::New => Command::New,
        Cmd::Mute { mode } => Command::Mute {
            on: match mode {
                MuteMode::On => Some(true),
                MuteMode::Off => Some(false),
                MuteMode::Toggle => None,
            },
        },
        Cmd::Status => Command::Status,
        Cmd::Memory => Command::Memory,
        Cmd::Forget => Command::Forget,
        Cmd::Quit => Command::Quit,
    };

    match ipc::send(&cmd) {
        Ok(reply) if reply.ok => {
            if let Some(state) = reply.state {
                println!("{state}");
            }
            ExitCode::SUCCESS
        }
        Ok(reply) => {
            eprintln!("fsc: {}", reply.error.unwrap_or_else(|| "failed".into()));
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("fsc: {e:#}");
            ExitCode::FAILURE
        }
    }
}
