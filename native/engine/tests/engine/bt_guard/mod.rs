//! bt_guard（BT 防吸血）集成测试：核心逻辑 + SQLite 存储；`e2e` / `filter_fetch` / `policy` 覆盖宿主胶水
//! （librqbit 策略适配器、事件泵、订阅拉取），其中 `e2e` 起真实的回环 BT 会话。

#![cfg(feature = "bt")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod cloud;
mod e2e;
mod evidence;
mod filter_fetch;
mod filter_file;
mod ip;
mod ladder;
mod layers;
mod policy;
mod ranges;
mod review;
mod rules;
mod store;
mod support;
mod swarm;
