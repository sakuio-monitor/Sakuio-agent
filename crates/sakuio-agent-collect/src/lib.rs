//! Host metrics collection for Linux, macOS, Windows, and BSD.
//! 主机指标采集（Linux、macOS、Windows、BSD）。

mod config;
mod connections;
mod cpu;
mod disk;
mod gpu;
// Referenced only by the Linux GPU backend (`gpu::platform_impl`); excluded
// on other platforms to avoid dead-code warnings.
// 仅 Linux GPU 后端（`gpu::platform_impl`）引用；其他平台排除编译。
#[cfg(target_os = "linux")]
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
