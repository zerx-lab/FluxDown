//! BT 任务级操作与会话设置（文件优先级 / 代理 / 重命名 / 移动 / 强制校验…）的端到端测试：
//! 真实做种端 + 本地 tracker + 完整引擎，见 [`swarm`]。

#![cfg(feature = "bt")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod file_priorities;
mod proxy;
mod storage_ops;
mod swarm;
mod versions;
