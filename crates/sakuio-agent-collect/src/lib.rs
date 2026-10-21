//! Host metrics collection for Linux, macOS, Windows, and BSD.
//! 主机指标采集（Linux、macOS、Windows、BSD）。

mod config;
mod connections;
mod cpu;
mod disk;
mod gpu;
// Referenced by the Linux GPU backend and the Windows GPU backend
// (system_profiler covers macOS); excluded elsewhere to avoid dead-code warnings.
// Linux 与 Windows 的 GPU 后端都引用（macOS 走 system_profiler）；其余平台排除编译。
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod nvidia_smi;
mod ip;
mod load;
mod mem;
mod net;
mod netstatic;
mod nic;
mod os;
mod ping;
mod process;
mod report;
mod virt;

pub use config::CollectConfig;
pub use ping::run_ping;
pub use report::{CollectState, HostStaticInfo, Reporter};
