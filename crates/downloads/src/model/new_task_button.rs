//! 顶栏「下载任务」主按钮的外观（设备本地偏好 `ui.new_task_button`，布局编辑模式下切换）。

use serde::{Deserialize, de::DeserializeOwned};

pub(crate) const NEW_TASK_BUTTON_PREF: &str = "ui.new_task_button";

/// 按钮形态：「+」图标按钮或带加号的文字按钮。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NewTaskButtonShape {
    Icon,
    #[default]
    Label,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NewTaskButtonStyle {
    pub(crate) shape: NewTaskButtonShape,
    /// 主题主色填充；关闭时为无底色的工具栏按钮。
    pub(crate) accent: bool,
}

impl Default for NewTaskButtonStyle {
    fn default() -> Self {
        Self {
            shape: NewTaskButtonShape::default(),
            accent: true,
        }
    }
}

impl NewTaskButtonStyle {
    /// 逐字段容错：缺失或未知取值只让该字段回到默认（新版本写入的形态不连带重置颜色）。
    pub(crate) fn from_value(value: Option<&serde_json::Value>) -> Self {
        let mut style = Self::default();
        let Some(map) = value.and_then(serde_json::Value::as_object) else {
            return style;
        };
        read_field(map, "shape", &mut style.shape);
        read_field(map, "accent", &mut style.accent);
        style
    }

    pub(crate) fn to_value(self) -> serde_json::Value {
        serde_json::json!({
            "shape": match self.shape {
                NewTaskButtonShape::Icon => "icon",
                NewTaskButtonShape::Label => "label",
            },
            "accent": self.accent,
        })
    }
}

fn read_field<T: DeserializeOwned>(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    slot: &mut T,
) {
    if let Some(value) = map.get(key).and_then(|value| T::deserialize(value).ok()) {
        *slot = value;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{NewTaskButtonShape, NewTaskButtonStyle};

    #[test]
    fn unknown_field_values_fall_back_per_field() {
        let style =
            NewTaskButtonStyle::from_value(Some(&json!({ "shape": "pill", "accent": false })));
        assert_eq!(style.shape, NewTaskButtonShape::Label);
        assert!(!style.accent);

        let style = NewTaskButtonStyle::from_value(Some(&json!({ "shape": "icon" })));
        assert_eq!(style.shape, NewTaskButtonShape::Icon);
        assert!(style.accent);
    }

    #[test]
    fn round_trips_through_preference_value() {
        let style = NewTaskButtonStyle {
            shape: NewTaskButtonShape::Icon,
            accent: false,
        };
        assert_eq!(
            NewTaskButtonStyle::from_value(Some(&style.to_value())),
            style
        );
    }
}
