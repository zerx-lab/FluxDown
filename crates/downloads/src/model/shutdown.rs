//! 定时关机 / 睡眠：跨 `downloads` / `app` 边界的最小共享契约。
//!
//! 状态机与实际执行归 agent（`agent.power.*` / `PowerChanged`），app 侧 `power` 只做
//! 投影与请求转发；本文件只定义状态栏渲染与请求发起所需的类型，避免 `downloads` 依赖 `app`。

use std::{cell::Cell, rc::Rc, time::Duration};

use fluxdown_protocol::PowerAction;
use serde::Deserialize as _;
use serde_json::Value;

/// 本机偏好：状态栏上次选中的电源操作（`shutdown` / `sleep`；设备本地，不进云同步）。
pub(crate) const POWER_ACTION_PREF: &str = "download.power_action";

/// 解析偏好值；缺省或无法识别时为关机。
pub(crate) fn read_power_action(value: Option<&Value>) -> PowerAction {
    value
        .and_then(|value| PowerAction::deserialize(value).ok())
        .unwrap_or_default()
}

/// 写回偏好的值（与 `PowerAction` 的 wire 名一致，见单测往返）。
pub(crate) fn power_action_pref_value(action: PowerAction) -> Value {
    let name = match action {
        PowerAction::Shutdown => "shutdown",
        PowerAction::Sleep => "sleep",
    };
    Value::String(name.to_owned())
}

/// 状态栏发出的电源请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownRequest {
    /// 取消已排定的操作（无论是等待任务完成还是正在倒计时）。
    Disarm,
    /// 排定 `action`：有任务时全部完成后再等 `delay`，没有任务时立即开始 `delay` 倒计时；
    /// `Duration::ZERO` = 完成后立即（只在有任务时可用）。
    Arm {
        delay: Duration,
        action: PowerAction,
    },
}

/// 调度器的只读状态投影，供任意窗口的状态栏渲染。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShutdownStatus {
    /// 已排定的延迟；`None` = 未启用。
    pub armed_delay: Option<Duration>,
    /// 真正开始倒计时后的剩余时间；等待任务完成阶段为 `None`。
    pub countdown_remaining: Option<Duration>,
    /// 已排定的操作（未启用时无意义）。
    pub action: PowerAction,
}

/// 跨窗口共享的状态（app 从 agent 快照投影写入，任意窗口只读渲染）。
pub type SharedShutdownStatus = Rc<Cell<ShutdownStatus>>;

/// 状态栏发起电源请求的端口（由 app 在主窗口装配时注入，转成 `agent.power.*`）。
pub type ShutdownPort = Rc<dyn Fn(ShutdownRequest, &mut gpui::App)>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn power_action_pref_round_trips_and_falls_back_to_shutdown() {
        for action in [PowerAction::Shutdown, PowerAction::Sleep] {
            let value = power_action_pref_value(action);
            assert_eq!(read_power_action(Some(&value)), action);
            assert_eq!(serde_json::to_value(action).ok(), Some(value));
        }
        assert_eq!(
            read_power_action(Some(&json!("hibernate"))),
            PowerAction::Shutdown
        );
        assert_eq!(read_power_action(Some(&Value::Null)), PowerAction::Shutdown);
        assert_eq!(read_power_action(None), PowerAction::Shutdown);
    }
}
