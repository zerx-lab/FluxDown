//! fluxdown_protocol 集成测试唯一二进制：按领域分模块，新增测试加模块，不要新建 tests/*.rs。

mod agent_wire_safety;
mod bt_guard_wire;
mod bt_ops_wire;
mod error_literals;
mod legacy_api_dto_wire;
mod legacy_server_wire;
mod method_literals;
mod plugin_view_fixtures;
mod private_plugin_wire;
mod selection_wire;
