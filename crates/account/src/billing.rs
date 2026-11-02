//! 套餐购买与推介的纯逻辑（与 Web `billing.ts` 逐条镜像，两端测试用同一组用例）。
//!
//! 不依赖 GPUI 状态，只做金额格式化、价格 / 状态判定、倒计时、邀请码校验与映射表。

use fluxdown_protocol::{CloudPlan, CloudSubscription, ErrorReason};
use fluxdown_ui_components::FluxIcon;

/// 每个账号最多可创建的推荐码数量。
pub(crate) const MAX_REFERRAL_CODES: usize = 10;
/// 自定义推荐码的长度范围（字符数，规范化之后）。
const REFERRAL_CODE_LEN: std::ops::RangeInclusive<usize> = 4..=16;

/// 整数运算格式化金额：CNY → `¥12.30`，其余 → `USD 12.30`；负数整体前置 `-`。
pub(crate) fn format_minor(minor: i64, currency: &str) -> String {
    let abs = minor.unsigned_abs();
    let number = format!("{}.{:02}", abs / 100, abs % 100);
    let sign = if minor < 0 { "-" } else { "" };
    if currency.eq_ignore_ascii_case("CNY") {
        format!("{sign}¥{number}")
    } else {
        format!("{sign}{} {number}", currency.to_ascii_uppercase())
    }
}

/// 生效价：有活动取活动价，否则目录价。
pub(crate) fn effective_price(plan: &CloudPlan) -> i64 {
    plan.campaign
        .as_ref()
        .map_or(plan.price_minor, |campaign| campaign.effective_price_minor)
}

/// 可购买会员列表：`purchasable && code != "free" && 生效价 > 0`，按 `sort` 升序稳定排序。
pub(crate) fn purchasable_plans(catalog: &[CloudPlan]) -> Vec<&CloudPlan> {
    let mut plans: Vec<&CloudPlan> = catalog
        .iter()
        .filter(|plan| plan.purchasable && plan.code != "free" && effective_price(plan) > 0)
        .collect();
    plans.sort_by_key(|plan| plan.sort);
    plans
}

/// 已持有套餐的基准价：在完整目录里找当前套餐 code 的 `priceMinor`；找不到或 free → 0。
pub(crate) fn owned_price(catalog: &[CloudPlan], current_code: Option<&str>) -> i64 {
    match current_code {
        Some(code) if code != "free" => catalog
            .iter()
            .find(|plan| plan.code == code)
            .map_or(0, |plan| plan.price_minor),
        _ => 0,
    }
}

/// 已持有永久套餐的 `sort`：目录里找不到（含未登录）→ `None`（不据此拦截会员卡）。
pub(crate) fn owned_sort(catalog: &[CloudPlan], current_code: Option<&str>) -> Option<i64> {
    let code = current_code?;
    catalog
        .iter()
        .find(|plan| plan.code == code)
        .map(|plan| plan.sort)
}

/// 周期会员卡（`billingPeriodDays` 非空的会员套餐）。
pub(crate) fn is_subscription_plan(plan: &CloudPlan) -> bool {
    plan.billing_period_days.is_some()
}

/// 套餐卡片状态；只有 [`CardState::Available`] 可选。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardState {
    /// 当前持有的套餐。
    Current,
    /// 价格不高于已持有套餐（买断制不可降档 / 平档）。
    TierBlocked,
    /// 已付抵扣额不低于该套餐生效价。
    CreditBlocked,
    /// 当前生效的会员卡（同卡续费）。
    Renewable,
    Available,
}

impl CardState {
    pub(crate) fn selectable(self) -> bool {
        matches!(self, Self::Available | Self::Renewable)
    }
}

/// 状态优先级：Current > TierBlocked > CreditBlocked > Available。
/// 周期会员卡不抵扣：同卡 → Renewable；有其他生效卡 → TierBlocked；`sort` 不高于永久套餐 → TierBlocked；否则 Available。
pub(crate) fn card_state(
    plan: &CloudPlan,
    current_code: Option<&str>,
    owned: i64,
    credit: i64,
    active_subscription: Option<&CloudSubscription>,
    base_sort: Option<i64>,
) -> CardState {
    if is_subscription_plan(plan) {
        match active_subscription {
            Some(sub) if sub.plan_code == plan.code => CardState::Renewable,
            Some(_) => CardState::TierBlocked,
            None if base_sort.is_some_and(|sort| plan.sort <= sort) => CardState::TierBlocked,
            None => CardState::Available,
        }
    } else if current_code == Some(plan.code.as_str()) {
        CardState::Current
    } else if owned > 0 && plan.price_minor <= owned {
        CardState::TierBlocked
    } else if credit > 0 && effective_price(plan) <= credit {
        CardState::CreditBlocked
    } else {
        CardState::Available
    }
}

/// 应付金额：有抵扣额时生效价减抵扣额，否则生效价。
pub(crate) fn payable(plan: &CloudPlan, credit: i64) -> i64 {
    let price = effective_price(plan);
    if credit > 0 { price - credit } else { price }
}

/// 卡片价格区的展示内容。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PriceDisplay {
    /// 主价（应付）。
    pub main: String,
    /// 划线价：有抵扣时为生效价，否则有活动时为目录价。
    pub strike: Option<String>,
    /// 有抵扣额时的抵扣金额（配 `accountPlanCreditApplied`）。
    pub credit: Option<String>,
    /// 无抵扣且有活动时的当前档位名称。
    pub stage_label: Option<String>,
}

/// 抵扣额只作用于可购买（[`CardState::Available`]）的卡片：当前 / 不可选套餐不是升级目标，
/// 用已付金额去减它们的价格会把「已付」误显示成「已抵扣」。
pub(crate) fn price_display(plan: &CloudPlan, state: CardState, credit: i64) -> PriceDisplay {
    let credit = if state == CardState::Available {
        credit
    } else {
        0
    };
    let currency = plan.currency.as_str();
    let main = format_minor(payable(plan, credit), currency);
    if credit > 0 {
        return PriceDisplay {
            main,
            strike: Some(format_minor(effective_price(plan), currency)),
            credit: Some(format_minor(credit, currency)),
            stage_label: None,
        };
    }
    match &plan.campaign {
        Some(campaign) => PriceDisplay {
            main,
            strike: Some(format_minor(plan.price_minor, currency)),
            credit: None,
            stage_label: usize::try_from(campaign.current_stage_index)
                .ok()
                .and_then(|index| campaign.stages.get(index))
                .map(|stage| stage.label.clone())
                .filter(|label| !label.is_empty()),
        },
        None => PriceDisplay {
            main,
            strike: None,
            credit: None,
            stage_label: None,
        },
    }
}

/// 活动当前档位的限量进度：`(已售, 限量)`；当前档位无限量时为 `None`。
pub(crate) fn campaign_progress(plan: &CloudPlan) -> Option<(i64, i64)> {
    let campaign = plan.campaign.as_ref()?;
    let index = usize::try_from(campaign.current_stage_index).ok()?;
    let quota = campaign.stages.get(index)?.quota?;
    let sold = campaign.stage_sold.get(index).copied().unwrap_or(0);
    Some((sold, quota))
}

/// 倒计时：负数按 0；< 3600 秒 → `mm:ss`，否则 `h:mm:ss`。
pub(crate) fn format_countdown(secs: i64) -> String {
    let secs = secs.max(0);
    let (hours, minutes, seconds) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if secs < 3600 {
        format!("{minutes:02}:{seconds:02}")
    } else {
        format!("{hours}:{minutes:02}:{seconds:02}")
    }
}

/// 剩余秒数（向下取整）：`expiresAt`（RFC 3339）− 现在；解析失败按 0。
pub(crate) fn remaining_secs(expires_at: &str, now_ms: i64) -> i64 {
    chrono::DateTime::parse_from_rfc3339(expires_at).map_or(0, |expires| {
        (expires.timestamp_millis() - now_ms).div_euclid(1000)
    })
}

/// 订单轮询结果的处理方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PollAction {
    /// 已支付：刷新资料后进入成功步骤。
    Paid,
    /// 订单已失效（过期 / 失败）：回到选择步骤并提示。
    Ended,
    /// 其余状态（含未知状态）：继续下一轮轮询。
    Pending,
}

pub(crate) fn poll_action(status: &str) -> PollAction {
    match status {
        "paid" => PollAction::Paid,
        "expired" | "failed" => PollAction::Ended,
        _ => PollAction::Pending,
    }
}

/// RFC 3339 时间 → 本地日期 `YYYY-MM-DD`；解析失败原样返回。
pub(crate) fn format_local_date(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw).map_or_else(
        |_| raw.to_owned(),
        |parsed| {
            parsed
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        },
    )
}

/// 推荐码规范化：去首尾空白并转大写。
pub(crate) fn normalize_code(input: &str) -> String {
    input.trim().to_uppercase()
}

/// 创建推荐码的本地校验：空（随机生成）或规范化后 `^[A-Z0-9]{4,16}$`。
pub(crate) fn create_code_is_valid(input: &str) -> bool {
    let code = normalize_code(input);
    code.is_empty()
        || (REFERRAL_CODE_LEN.contains(&code.len())
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()))
}

/// `agent.referral.validate` 返回 `valid=false` 时的 reason → 文案键。
pub(crate) fn validate_reason_key(reason: Option<&str>) -> &'static str {
    match reason {
        Some("self_use") => "accountPlanErrorReferralSelfUse",
        Some("already_used") => "accountPlanErrorReferralAlreadyUsed",
        Some("not_found") => "accountPlanErrorReferralNotFound",
        Some("plan_excluded") => "accountPlanErrorReferralPlanExcluded",
        _ => "accountPlanErrorReferralInvalid",
    }
}

/// 购买 / 推介相关 `ErrorReason` → 文案键；其它 reason 返回 `None`。
pub(crate) fn billing_reason_key(reason: ErrorReason) -> Option<&'static str> {
    Some(match reason {
        ErrorReason::PaymentDisabled => "accountPlanErrorPaymentDisabled",
        ErrorReason::PaymentGatewayFailed => "accountPlanErrorGateway",
        ErrorReason::PlanNotPurchasable => "accountPlanErrorNotPurchasable",
        ErrorReason::PlanAlreadyOwned => "accountPlanErrorAlreadyOwned",
        ErrorReason::PlanNotUpgrade => "accountPlanErrorNotUpgrade",
        ErrorReason::PlanTierNotHigher => "accountPlanErrorTierNotHigher",
        ErrorReason::SubscriptionConflict => "errReasonSubscriptionConflict",
        ErrorReason::ReferralCodeInvalid => "accountPlanErrorReferralInvalid",
        ErrorReason::ReferralSelfUse => "accountPlanErrorReferralSelfUse",
        ErrorReason::ReferralAlreadyUsed => "accountPlanErrorReferralAlreadyUsed",
        ErrorReason::ReferralPlanExcluded => "accountPlanErrorReferralPlanExcluded",
        ErrorReason::ReferralCodeTaken => "accountReferralCodeTaken",
        _ => return None,
    })
}

/// 套餐图标名（去空白、不区分大小写）→ Lucide 图标；未知名回退皇冠。
pub(crate) fn plan_icon(name: &str) -> FluxIcon {
    match name.trim().to_ascii_lowercase().as_str() {
        "rocket" => FluxIcon::Rocket,
        "star" => FluxIcon::Star,
        "gem" => FluxIcon::Gem,
        "diamond" => FluxIcon::Diamond,
        "zap" => FluxIcon::Zap,
        "shield" => FluxIcon::Shield,
        "sparkles" => FluxIcon::Sparkles,
        _ => FluxIcon::Crown,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use fluxdown_protocol::{CloudPlanCampaign, CloudPlanCampaignStage};

    use super::*;

    fn plan(code: &str, price: i64, sort: i64) -> CloudPlan {
        serde_json::from_value(serde_json::json!({
            "code": code,
            "priceMinor": price,
            "sort": sort,
        }))
        .expect("plan json")
    }

    fn with_campaign(mut plan: CloudPlan, effective: i64) -> CloudPlan {
        plan.campaign = Some(CloudPlanCampaign {
            name: "launch".to_owned(),
            end_at: None,
            stages: vec![
                CloudPlanCampaignStage {
                    label: "早鸟".to_owned(),
                    price_minor: effective,
                    quota: Some(100),
                },
                CloudPlanCampaignStage {
                    label: "常规".to_owned(),
                    price_minor: plan.price_minor,
                    quota: None,
                },
            ],
            sold_total: 130,
            stage_sold: vec![100, 30],
            current_stage_index: 0,
            effective_price_minor: effective,
        });
        plan
    }

    #[test]
    fn format_minor_cases() {
        let cases: &[(i64, &str, &str)] = &[
            (1230, "CNY", "¥12.30"),
            (1230, "cny", "¥12.30"),
            (0, "CNY", "¥0.00"),
            (5, "CNY", "¥0.05"),
            (99, "CNY", "¥0.99"),
            (100, "CNY", "¥1.00"),
            (1230, "USD", "USD 12.30"),
            (1230, "usd", "USD 12.30"),
            (-1230, "CNY", "-¥12.30"),
            (-5, "USD", "-USD 0.05"),
            (123_456_789, "CNY", "¥1234567.89"),
        ];
        for (minor, currency, expected) in cases {
            assert_eq!(
                format_minor(*minor, currency),
                *expected,
                "{minor} {currency}"
            );
        }
    }

    #[test]
    fn effective_price_prefers_campaign() {
        assert_eq!(effective_price(&plan("pro", 3000, 1)), 3000);
        assert_eq!(
            effective_price(&with_campaign(plan("pro", 3000, 1), 1990)),
            1990
        );
    }

    #[test]
    fn purchasable_filters_and_sorts_stably() {
        let mut hidden = plan("hidden", 100, 0);
        hidden.purchasable = false;
        let catalog = vec![
            plan("c", 3000, 2),
            plan("free", 0, 0),
            plan("zero", 0, 1),
            hidden,
            plan("a", 1000, 2),
            plan("b", 2000, 1),
        ];
        let codes: Vec<&str> = purchasable_plans(&catalog)
            .iter()
            .map(|plan| plan.code.as_str())
            .collect();
        // sort 相同的 c / a 保持目录原顺序。
        assert_eq!(codes, ["b", "c", "a"]);
        // free 即使价格被误配成正数也不可买。
        assert!(purchasable_plans(&[plan("free", 100, 0)]).is_empty());
        // 生效价为 0 的活动不可买。
        assert!(purchasable_plans(&[with_campaign(plan("x", 100, 0), 0)]).is_empty());
    }

    fn card(code: &str, price: i64, sort: i64) -> CloudPlan {
        let mut p = plan(code, price, sort);
        p.billing_period_days = Some(30);
        p
    }

    fn sub(code: &str) -> CloudSubscription {
        CloudSubscription {
            plan_code: code.to_owned(),
            ..CloudSubscription::default()
        }
    }

    #[test]
    fn subscription_card_states() {
        let c = card("m1", 1000, 5);
        let other = card("m2", 2000, 6);
        // 同卡 → Renewable；其他卡在有生效卡时 → TierBlocked。
        assert_eq!(
            card_state(&c, Some("pro"), 3000, 9999, Some(&sub("m1")), Some(1)),
            CardState::Renewable
        );
        assert_eq!(
            card_state(&other, Some("pro"), 3000, 9999, Some(&sub("m1")), Some(1)),
            CardState::TierBlocked
        );
        // 无生效卡：sort ≤ 永久套餐 → TierBlocked；更高 → Available（价格低于永久套餐也不拦、不抵扣）。
        assert_eq!(
            card_state(&c, Some("pro"), 3000, 9999, None, Some(5)),
            CardState::TierBlocked
        );
        assert_eq!(
            card_state(&c, Some("pro"), 3000, 9999, None, Some(4)),
            CardState::Available
        );
        assert_eq!(
            card_state(&c, None, 0, 9999, None, None),
            CardState::Available
        );
        assert!(CardState::Renewable.selectable());
        assert!(is_subscription_plan(&c));
        assert!(!is_subscription_plan(&plan("pro", 3000, 1)));
        assert_eq!(
            billing_reason_key(ErrorReason::SubscriptionConflict),
            Some("errReasonSubscriptionConflict")
        );
    }

    #[test]
    fn owned_price_uses_full_catalog() {
        let catalog = vec![
            plan("free", 0, 0),
            plan("pro", 3000, 1),
            plan("max", 9000, 2),
        ];
        assert_eq!(owned_price(&catalog, Some("pro")), 3000);
        assert_eq!(owned_price(&catalog, Some("free")), 0);
        assert_eq!(owned_price(&catalog, Some("missing")), 0);
        assert_eq!(owned_price(&catalog, None), 0);
    }

    #[test]
    fn card_state_priority() {
        let card_state = |plan: &CloudPlan, cur: Option<&str>, owned: i64, credit: i64| {
            super::card_state(plan, cur, owned, credit, None, None)
        };
        let pro = plan("pro", 3000, 1);
        // Current 优先于一切（即便同时满足 TierBlocked / CreditBlocked）。
        assert_eq!(
            card_state(&pro, Some("pro"), 3000, 5000),
            CardState::Current
        );
        // TierBlocked 优先于 CreditBlocked。
        assert_eq!(
            card_state(&pro, Some("other"), 3000, 5000),
            CardState::TierBlocked
        );
        assert_eq!(
            card_state(&pro, Some("other"), 4000, 0),
            CardState::TierBlocked
        );
        // CreditBlocked：生效价 <= credit。
        assert_eq!(
            card_state(&pro, Some("other"), 0, 3000),
            CardState::CreditBlocked
        );
        assert_eq!(card_state(&pro, None, 0, 4000), CardState::CreditBlocked);
        // 活动价参与 CreditBlocked 判定（目录价 3000 > credit 2000 ≥ 活动价 1990）。
        let promo = with_campaign(plan("pro", 3000, 1), 1990);
        assert_eq!(card_state(&promo, None, 0, 2000), CardState::CreditBlocked);
        // Available。
        assert_eq!(
            card_state(&pro, Some("lite"), 1000, 1000),
            CardState::Available
        );
        assert_eq!(card_state(&pro, None, 0, 0), CardState::Available);
        assert!(CardState::Available.selectable());
        assert!(!CardState::Current.selectable());
        assert!(!CardState::TierBlocked.selectable());
        assert!(!CardState::CreditBlocked.selectable());
    }

    #[test]
    fn payable_subtracts_credit() {
        let pro = plan("pro", 3000, 1);
        assert_eq!(payable(&pro, 0), 3000);
        assert_eq!(payable(&pro, 1000), 2000);
        let promo = with_campaign(plan("pro", 3000, 1), 1990);
        assert_eq!(payable(&promo, 0), 1990);
        assert_eq!(payable(&promo, 500), 1490);
    }

    #[test]
    fn price_display_variants() {
        let pro = plan("pro", 3000, 1);
        assert_eq!(
            price_display(&pro, CardState::Available, 0),
            PriceDisplay {
                main: "¥30.00".to_owned(),
                strike: None,
                credit: None,
                stage_label: None
            }
        );
        let promo = with_campaign(plan("pro", 3000, 1), 1990);
        assert_eq!(
            price_display(&promo, CardState::Available, 0),
            PriceDisplay {
                main: "¥19.90".to_owned(),
                strike: Some("¥30.00".to_owned()),
                credit: None,
                stage_label: Some("早鸟".to_owned())
            }
        );
        // 有抵扣：划线价是生效价，不再显示档位名。
        assert_eq!(
            price_display(&promo, CardState::Available, 500),
            PriceDisplay {
                main: "¥14.90".to_owned(),
                strike: Some("¥19.90".to_owned()),
                credit: Some("¥5.00".to_owned()),
                stage_label: None
            }
        );
        // 当前 / 不可选套餐不套用抵扣：显示原本价格。
        for state in [
            CardState::Current,
            CardState::TierBlocked,
            CardState::CreditBlocked,
        ] {
            assert_eq!(
                price_display(&pro, state, 2000),
                PriceDisplay {
                    main: "¥30.00".to_owned(),
                    strike: None,
                    credit: None,
                    stage_label: None
                }
            );
        }
    }

    #[test]
    fn campaign_progress_follows_current_stage() {
        assert_eq!(campaign_progress(&plan("pro", 3000, 1)), None);
        let mut promo = with_campaign(plan("pro", 3000, 1), 1990);
        assert_eq!(campaign_progress(&promo), Some((100, 100)));
        if let Some(campaign) = promo.campaign.as_mut() {
            campaign.current_stage_index = 1;
        }
        // 当前档位无限量。
        assert_eq!(campaign_progress(&promo), None);
    }

    #[test]
    fn countdown_boundaries() {
        let cases: &[(i64, &str)] = &[
            (-5, "00:00"),
            (0, "00:00"),
            (1, "00:01"),
            (59, "00:59"),
            (60, "01:00"),
            (900, "15:00"),
            (3599, "59:59"),
            (3600, "1:00:00"),
            (3661, "1:01:01"),
            (7199, "1:59:59"),
        ];
        for (secs, expected) in cases {
            assert_eq!(format_countdown(*secs), *expected, "{secs}");
        }
    }

    #[test]
    fn remaining_secs_parses_and_floors() {
        // 2026-10-17T00:00:00Z = 1792195200000 ms。
        let base = 1_792_195_200_000;
        assert_eq!(remaining_secs("2026-10-17T00:15:00Z", base), 900);
        assert_eq!(remaining_secs("2026-10-17T00:15:00Z", base + 500), 899);
        assert_eq!(remaining_secs("2026-10-17T08:15:00+08:00", base), 900);
        assert_eq!(remaining_secs("2026-10-16T23:59:00Z", base), -60);
        assert_eq!(remaining_secs("not a time", base), 0);
        assert_eq!(remaining_secs("", base), 0);
    }

    #[test]
    fn code_normalize_and_validate() {
        assert_eq!(normalize_code("  abc123 "), "ABC123");
        let valid = [
            "",
            "   ",
            "abcd",
            "ABCD",
            "a1b2",
            "ABCDEFGHIJKLMNOP",
            " abc123 ",
        ];
        for input in valid {
            assert!(create_code_is_valid(input), "{input:?}");
        }
        let invalid = [
            "abc",
            "ABCDEFGHIJKLMNOPQ",
            "ab-cd",
            "ab cd",
            "abc_",
            "中文码字符",
            "ABC!",
        ];
        for input in invalid {
            assert!(!create_code_is_valid(input), "{input:?}");
        }
        // 长度边界：3 / 4 / 16 / 17。
        assert!(!create_code_is_valid(&"A".repeat(3)));
        assert!(create_code_is_valid(&"A".repeat(4)));
        assert!(create_code_is_valid(&"A".repeat(16)));
        assert!(!create_code_is_valid(&"A".repeat(17)));
    }

    #[test]
    fn validate_reason_mapping() {
        let cases: &[(Option<&str>, &str)] = &[
            (Some("self_use"), "accountPlanErrorReferralSelfUse"),
            (Some("already_used"), "accountPlanErrorReferralAlreadyUsed"),
            (Some("not_found"), "accountPlanErrorReferralNotFound"),
            (
                Some("plan_excluded"),
                "accountPlanErrorReferralPlanExcluded",
            ),
            (Some("feature_disabled"), "accountPlanErrorReferralInvalid"),
            (Some("whatever"), "accountPlanErrorReferralInvalid"),
            (None, "accountPlanErrorReferralInvalid"),
        ];
        for (reason, key) in cases {
            assert_eq!(validate_reason_key(*reason), *key, "{reason:?}");
        }
    }

    #[test]
    fn billing_reasons_map_to_keys() {
        let cases = [
            (
                ErrorReason::PaymentDisabled,
                "accountPlanErrorPaymentDisabled",
            ),
            (ErrorReason::PaymentGatewayFailed, "accountPlanErrorGateway"),
            (
                ErrorReason::PlanNotPurchasable,
                "accountPlanErrorNotPurchasable",
            ),
            (
                ErrorReason::PlanAlreadyOwned,
                "accountPlanErrorAlreadyOwned",
            ),
            (ErrorReason::PlanNotUpgrade, "accountPlanErrorNotUpgrade"),
            (
                ErrorReason::PlanTierNotHigher,
                "accountPlanErrorTierNotHigher",
            ),
            (
                ErrorReason::ReferralCodeInvalid,
                "accountPlanErrorReferralInvalid",
            ),
            (
                ErrorReason::ReferralSelfUse,
                "accountPlanErrorReferralSelfUse",
            ),
            (
                ErrorReason::ReferralAlreadyUsed,
                "accountPlanErrorReferralAlreadyUsed",
            ),
            (
                ErrorReason::ReferralPlanExcluded,
                "accountPlanErrorReferralPlanExcluded",
            ),
            (ErrorReason::ReferralCodeTaken, "accountReferralCodeTaken"),
        ];
        for (reason, key) in cases {
            assert_eq!(billing_reason_key(reason), Some(key), "{reason:?}");
        }
        assert_eq!(billing_reason_key(ErrorReason::RateLimited), None);
    }

    #[test]
    fn plan_icons_fall_back_to_crown() {
        assert_eq!(plan_icon("rocket"), FluxIcon::Rocket);
        assert_eq!(plan_icon("sparkles"), FluxIcon::Sparkles);
        assert_eq!(plan_icon("crown"), FluxIcon::Crown);
        assert_eq!(plan_icon("nope"), FluxIcon::Crown);
        assert_eq!(plan_icon(""), FluxIcon::Crown);
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;

    #[test]
    fn poll_action_mapping() {
        assert_eq!(poll_action("paid"), PollAction::Paid);
        assert_eq!(poll_action("expired"), PollAction::Ended);
        assert_eq!(poll_action("failed"), PollAction::Ended);
        assert_eq!(poll_action("pending"), PollAction::Pending);
        assert_eq!(poll_action("something-new"), PollAction::Pending);
    }
}

/// 总页数（至少 1；非正总数按 1 页）。
pub(crate) fn page_count(total: i64, page_size: u32) -> u32 {
    let total = u64::try_from(total).unwrap_or(0);
    let pages = total.div_ceil(u64::from(page_size.max(1))).max(1);
    u32::try_from(pages).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod page_tests {
    use super::*;

    #[test]
    fn page_count_boundaries() {
        let cases: &[(i64, u32)] = &[(-1, 1), (0, 1), (1, 1), (20, 1), (21, 2), (40, 2), (41, 3)];
        for (total, pages) in cases {
            assert_eq!(page_count(*total, 20), *pages, "{total}");
        }
    }
}
