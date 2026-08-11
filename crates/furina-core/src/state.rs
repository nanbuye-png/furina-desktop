//! Agent loop state machine.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Planning,
    AwaitingApproval,
    Executing,
    Verifying,
    Repairing,
    Done,
    Failed,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Planning => "planning",
            AgentState::AwaitingApproval => "awaiting_approval",
            AgentState::Executing => "executing",
            AgentState::Verifying => "verifying",
            AgentState::Repairing => "repairing",
            AgentState::Done => "done",
            AgentState::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names() {
        assert_eq!(AgentState::Idle.as_str(), "idle");
        assert_eq!(AgentState::Verifying.as_str(), "verifying");
        assert_eq!(AgentState::Failed.as_str(), "failed");
    }
}
