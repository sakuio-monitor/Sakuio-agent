//! SakuLink agent session loop (transport-agnostic).
//! SakuLink Agent 会话循环（与传输无关）。

mod runner;
mod update;

pub use runner::{AgentConfig, AgentRunner};
pub use update::{run_forced_update, run_update_loop, BoardLinkSnapshot, RESTART_EXIT_CODE};
