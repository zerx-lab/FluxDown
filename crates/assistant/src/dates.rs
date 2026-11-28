//! 会话列表日期：云端 RFC 3339 时间戳 → 本机时区的「今天 / 昨天 / 日期」。
//!
//! 纯计算：当前时刻与时区由调用方传入，保证可单测。

use jiff::{Timestamp, ZonedDateTime, tz::TimeZone};

/// 日期回退（解析失败）时取原字符串的前几位（`YYYY-MM-DD`）。
const RAW_DATE_CHARS: usize = 10;

/// 会话时间在本机时区下相对「现在」的归类。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionDate {
    Today,
    Yesterday,
    /// 本地日期，`YYYY-MM-DD`。
    Day(String),
}

/// 本机时区下的当前时刻。
pub(crate) fn local_now() -> ZonedDateTime {
    Timestamp::now().to_zoned_datetime(TimeZone::system())
}

/// 把云端时间戳换到 `now` 所在时区，再与 `now` 的日期比较。时间戳解析失败时回退原字符串前 10 位。
pub(crate) fn session_date(raw: &str, now: &ZonedDateTime) -> SessionDate {
    let raw = raw.trim();
    let Ok(timestamp) = raw.parse::<Timestamp>() else {
        return SessionDate::Day(raw.get(..RAW_DATE_CHARS).unwrap_or(raw).to_owned());
    };
    let day = timestamp.to_zoned_datetime(now.time_zone().clone()).date();
    let today = now.date();
    if day == today {
        SessionDate::Today
    } else if today.yesterday().is_ok_and(|yesterday| yesterday == day) {
        SessionDate::Yesterday
    } else {
        SessionDate::Day(day.strftime("%Y-%m-%d").to_string())
    }
}

#[cfg(test)]
mod tests {
    use jiff::tz::Offset;

    use super::*;

    /// UTC+8 下的 `now`：2026-11-28 01:00（对应 UTC 2026-11-27 17:00）。
    fn now_utc8() -> ZonedDateTime {
        let zone = TimeZone::fixed(Offset::constant(8));
        "2026-11-27T17:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned_datetime(zone)
    }

    #[test]
    fn utc_late_evening_is_next_day_locally() {
        // UTC 11-27 16:30 在 UTC+8 已是 11-28 00:30 = 今天；按 UTC 前 10 位会误显示成前一天。
        assert_eq!(
            session_date("2026-11-27T16:30:00Z", &now_utc8()),
            SessionDate::Today
        );
    }

    #[test]
    fn yesterday_and_older_days() {
        let now = now_utc8();
        assert_eq!(
            session_date("2026-11-27T10:00:00+00:00", &now),
            SessionDate::Yesterday
        );
        assert_eq!(
            session_date("2026-11-20T10:00:00.123456Z", &now),
            SessionDate::Day("2026-11-20".to_owned())
        );
    }

    #[test]
    fn unparsable_falls_back_to_the_first_ten_chars() {
        let now = now_utc8();
        assert_eq!(
            session_date("2026-11-20 10:00", &now),
            SessionDate::Day("2026-11-20".to_owned())
        );
        assert_eq!(
            session_date("n/a", &now),
            SessionDate::Day("n/a".to_owned())
        );
    }

    #[test]
    fn yesterday_crosses_month_and_year_boundaries() {
        let zone = TimeZone::fixed(Offset::constant(8));
        // 本地 2026-12-01 03:00（UTC 11-30 19:00）：UTC 11-30 05:00 = 本地 11-30 13:00 = 昨天。
        let now = "2026-11-30T19:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned_datetime(zone.clone());
        assert_eq!(
            session_date("2026-11-30T05:00:00Z", &now),
            SessionDate::Yesterday
        );
        // 本地 2027-01-01 08:00：昨天是上一年的 12-31。
        let now = "2027-01-01T00:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned_datetime(zone);
        assert_eq!(
            session_date("2026-12-31T04:00:00Z", &now),
            SessionDate::Yesterday
        );
        assert_eq!(
            session_date("2026-12-30T04:00:00Z", &now),
            SessionDate::Day("2026-12-30".to_owned())
        );
    }

    #[test]
    fn offsets_in_the_timestamp_are_normalised_to_the_local_zone() {
        let now = now_utc8();
        // -05:00 的 11-27 20:00 = UTC 11-28 01:00 = 本地 11-28 09:00。
        assert_eq!(
            session_date("2026-11-27T20:00:00-05:00", &now),
            SessionDate::Today
        );
        // 本地午夜整点属于今天，前一秒属于昨天。
        assert_eq!(
            session_date("2026-11-28T00:00:00+08:00", &now),
            SessionDate::Today
        );
        assert_eq!(
            session_date("2026-11-27T23:59:59+08:00", &now),
            SessionDate::Yesterday
        );
    }

    #[test]
    fn future_and_whitespace_and_empty_inputs() {
        let now = now_utc8();
        // 明天（时钟偏差）不是「今天」：回退日期。
        assert_eq!(
            session_date("2026-11-29T10:00:00+08:00", &now),
            SessionDate::Day("2026-11-29".to_owned())
        );
        assert_eq!(
            session_date("  2026-11-28T00:30:00+08:00  ", &now),
            SessionDate::Today
        );
        assert_eq!(session_date("", &now), SessionDate::Day(String::new()));
        // 多字节内容按字符边界回退，不 panic。
        assert_eq!(
            session_date("今天今天今天今天", &now),
            SessionDate::Day("今天今天今天今天".to_owned())
        );
    }
}
