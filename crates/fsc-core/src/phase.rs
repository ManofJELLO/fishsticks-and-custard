/// What the assistant is doing right now. Drives the status label, the
/// visualiser and the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// Capturing the user's voice (red everywhere).
    Listening,
    /// Prompt sent, waiting for the first token.
    Thinking,
    /// Claude is running a tool.
    Working,
    /// Reply text is streaming in (later: being spoken).
    Responding,
    Error,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Idle => "READY",
            Phase::Listening => "LISTENING",
            Phase::Thinking => "THINKING",
            Phase::Working => "WORKING",
            Phase::Responding => "RESPONDING",
            Phase::Error => "ERROR",
        }
    }

    /// A Claude turn is in flight.
    pub fn busy(self) -> bool {
        matches!(self, Phase::Thinking | Phase::Working | Phase::Responding)
    }
}
