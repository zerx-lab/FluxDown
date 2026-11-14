//! 任务「链路」wire 标签 → 展示文案（Auto 路由或站点规则标签）。

use fluxdown_ui_i18n::Translator;
use gpui::SharedString;

/// 站点规则标签整串精确匹配，必须先于 `:system` / `:manual` 后缀拆分，
/// 否则 `rule:system` 会被误拆。未知值原样返回。
pub(crate) fn route_label(route: &str, translator: &Translator) -> SharedString {
    let rule_key = match route {
        "rule:direct" => Some("taskRouteRuleDirect"),
        "rule:system" => Some("taskRouteRuleSystem"),
        "rule:manual" => Some("taskRouteRuleManual"),
        "rule:custom" => Some("taskRouteRuleCustom"),
        _ => None,
    };
    if let Some(key) = rule_key {
        return SharedString::from(translator.text(key).to_owned());
    }
    let (base, via) = if let Some(base) = route.strip_suffix(":system") {
        (base, Some("taskRouteViaSystem"))
    } else if let Some(base) = route.strip_suffix(":manual") {
        (base, Some("taskRouteViaManual"))
    } else {
        (route, None)
    };
    let base_key = match base {
        "direct" => Some("taskRouteDirect"),
        "direct:sampled" => Some("taskRouteDirectSampled"),
        "direct:pinned" => Some("taskRouteDirectPinned"),
        "direct:failover" => Some("taskRouteDirectFailover"),
        "proxy:cached" => Some("taskRouteProxyCached"),
        "proxy:sampled" => Some("taskRouteProxySampled"),
        "proxy:failover" => Some("taskRouteProxyFailover"),
        _ => None,
    };
    let label = base_key.map_or(route, |key| translator.text(key));
    match via {
        Some(via) => SharedString::from(format!("{label} · {}", translator.text(via))),
        None => SharedString::from(label.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_ui_i18n::{I18nCatalog, I18nError};

    use super::route_label;

    #[test]
    fn rule_labels_match_exactly_before_suffix_split() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        let t = catalog.translator("en");
        assert_eq!(route_label("rule:direct", &t), "Site rule · direct");
        assert_eq!(route_label("rule:system", &t), "Site rule · system proxy");
        assert_eq!(route_label("rule:manual", &t), "Site rule · manual proxy");
        assert_eq!(route_label("rule:custom", &t), "Site rule · custom proxy");
        Ok(())
    }

    #[test]
    fn auto_labels_and_suffixes() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        let t = catalog.translator("en");
        assert_eq!(route_label("direct", &t), "Direct");
        assert_eq!(
            route_label("proxy:cached:system", &t),
            "Proxy (same-site decision) · system proxy"
        );
        assert_eq!(
            route_label("proxy:failover:manual", &t),
            "Proxy (direct failed, failed over) · manual proxy"
        );
        Ok(())
    }

    #[test]
    fn unknown_labels_pass_through() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        let t = catalog.translator("en");
        assert_eq!(route_label("weird", &t), "weird");
        assert_eq!(
            route_label("weird:system", &t),
            "weird:system · system proxy"
        );
        assert_eq!(route_label("rule:other", &t), "rule:other");
        Ok(())
    }
}
