//! WebSocket `BoardTransport` client (maps `BoardFrame` to WS text/binary).
//! WebSocket 板级传输客户端（`BoardFrame` ↔ WS 文本/二进制）。

mod client;
mod session;

pub use client::WsBoardTransport;
