// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 配额窗口周期数学（从 [`crate::middleware::quota`] 拆出的纯函数面）：
//! 窗口周期两形态（[`crate::config::QuotaWindowConfig`] 的 period/calendar
//! 两字段编译产物）、自然边界对齐与翻转判定。数学自实现（chrono 无时区
//! 特性依赖）：只在 Unix 秒上做欧几里得除法 + 民用日换算（Hinnant 算法）。
//!
//! 时区口径：`tz_offset_secs` 是配置声明的固定偏移（无 DST），全部边界
//! 计算 = 先平移到本地帧取自然边界，再减回偏移卷回 UTC。月窗的
//! 「下个自然月 1 日」同样在本地帧取日期（[`Period::reset_at`]）——
//! 混用 UTC 帧取月首会让 tz>0 的月窗塌缩为月首 |tz| 小时。

/// 窗口周期：rolling 从窗内首次用量起算（起点由状态承载）；calendar
/// 对齐自然边界（日 00:00、ISO 周一、自然月 1 日），时区用配置声明的
/// `tz_offset_secs`（默认 0=UTC）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    /// 固定长度滚动窗（秒）
    Rolling(u64),
    /// 自然日窗（本地 00:00 对齐）
    Daily,
    /// ISO 周窗（本地周一 00:00 对齐；ISO 周恒 7 天）
    Weekly,
    /// 自然月窗（本地 1 日 00:00 对齐；长度随月份 28-31 天变长）
    Monthly,
}

/// 一天的秒数
pub const DAY_SECS: i64 = 86_400;

/// 自然月窗长的上界（31 天）：月窗长度依赖起点月份，保留/清理按上界
/// 推导下界，宁可多留一行不可误删活动行。
pub const MAX_MONTH_SECS: i64 = 31 * DAY_SECS;

impl Period {
    /// 从窗口配置编译（`period` + `calendar` 两字段；validate 已保证闭集，
    /// 此处 None = 手构配置的运行期兜底）。rolling 窗长取 `rolling_secs`；
    /// 两形态混用返回 None（与 validate 的互斥拒绝对称，不静默择一）。
    pub fn from_config(period: &str, calendar: &str, rolling_secs: u64) -> Option<Self> {
        let calendar = calendar.trim();
        match period.trim() {
            "rolling" if calendar.is_empty() => Some(Period::Rolling(rolling_secs)),
            "calendar" => match calendar {
                "daily" => Some(Period::Daily),
                "weekly" => Some(Period::Weekly),
                "monthly" => Some(Period::Monthly),
                _ => None,
            },
            _ => None,
        }
    }

    /// 包含 `now` 的自然窗起点（Unix 秒，已含 tz 偏移回卷）；rolling 返回
    /// `None`——其起点 = 窗内首次用量时刻，由窗口状态承载。
    pub fn window_start(&self, now_secs: i64, tz_offset_secs: i64) -> Option<i64> {
        let shifted = now_secs + tz_offset_secs;
        let start_shifted = match self {
            Period::Rolling(_) => return None,
            Period::Daily => shifted.div_euclid(DAY_SECS) * DAY_SECS,
            Period::Weekly => {
                let days = shifted.div_euclid(DAY_SECS);
                // 1970-01-01 为周四：days + 3 ≡ 0 当且仅当周一（ISO 周起点）
                (days - (days + 3).rem_euclid(7)) * DAY_SECS
            }
            Period::Monthly => {
                let days = shifted.div_euclid(DAY_SECS);
                let (year, month, _) = civil_from_days(days);
                days_from_civil(year, month, 1) * DAY_SECS
            }
        };
        Some(start_shifted - tz_offset_secs)
    }

    /// `start`（本窗起点，Unix 秒）起的窗在 `now` 是否已翻转。tz 参与月窗
    /// 翻转时刻（与 [`Self::window_start`] 同一本地帧口径，二者必须同参
    /// 成对使用）。
    pub fn is_expired(&self, start_secs: i64, now_secs: i64, tz_offset_secs: i64) -> bool {
        now_secs >= self.reset_at(start_secs, tz_offset_secs)
    }

    /// `start` 起窗的重置时刻 = 下一窗起点（Unix 秒）。rolling 长度固定；
    /// daily/weekly 窗长为日的整数倍（固定偏移下边界逐窗平移，与 tz 无关）；
    /// monthly 按「下个自然月 1 日」在 **本地帧** 取 civil 日期再卷回 UTC
    /// （28-31 天变长）——起点是本地月初午夜（UTC 帧落在前一日的 |tz| 时刻），
    /// 直接在 UTC 帧取月首会把窗长塌缩为月首 |tz| 小时。
    pub fn reset_at(&self, start_secs: i64, tz_offset_secs: i64) -> i64 {
        match *self {
            Period::Rolling(secs) => start_secs + secs.max(1) as i64,
            Period::Daily => start_secs + DAY_SECS,
            Period::Weekly => start_secs + 7 * DAY_SECS,
            Period::Monthly => {
                let local_days = (start_secs + tz_offset_secs).div_euclid(DAY_SECS);
                let (year, month, _) = civil_from_days(local_days);
                let (next_y, next_m) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                days_from_civil(next_y, next_m, 1) * DAY_SECS - tz_offset_secs
            }
        }
    }

    /// 窗长（秒；monthly 依起点变长）。pace 因子的 `budget/period` 分母。
    pub fn period_secs(&self, start_secs: i64, tz_offset_secs: i64) -> i64 {
        match *self {
            Period::Monthly => self.reset_at(start_secs, tz_offset_secs) - start_secs,
            Period::Rolling(secs) => secs.max(1) as i64,
            Period::Daily => DAY_SECS,
            Period::Weekly => 7 * DAY_SECS,
        }
    }
}

/// 民用日期 → Unix 天数（Hinnant days_from_civil；proleptic Gregorian）。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let m = month as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix 天数 → 民用日期（Hinnant civil_from_days）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn rfc3339(secs: i64) -> String {
        chrono::DateTime::from_timestamp(secs, 0)
            .unwrap()
            .to_rfc3339()
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        at_secs(y, m, d, h, min, 0)
    }

    fn at_secs(y: i32, m: u32, d: u32, h: u32, min: u32, sec: u32) -> i64 {
        chrono::Utc
            .with_ymd_and_hms(y, m, d, h, min, sec)
            .unwrap()
            .timestamp()
    }

    /// rolling 窗起点不可由 now 推导（= 窗内首次用量时刻，状态承载）；
    /// 重置时刻 = 起点 + 窗长；翻转判定含端点（now == reset_at 即翻）。
    #[test]
    fn rolling_window_starts_on_first_use() {
        let period = Period::Rolling(3600);
        assert!(
            period.window_start(1_770_000_000, 0).is_none(),
            "rolling 起点由首次用量状态承载，不由 now 推导"
        );
        let start = 1_770_000_000;
        assert_eq!(period.reset_at(start, 0), start + 3600);
        assert!(!period.is_expired(start, start + 3599, 0));
        assert!(period.is_expired(start, start + 3600, 0), "端点即翻转");
        assert_eq!(period.period_secs(start, 0), 3600);
        // 0 秒窗兜底 ≥1（与旧 window_secs.max(1) 同口径）
        assert_eq!(Period::Rolling(0).period_secs(start, 0), 1);
    }

    /// calendar 日窗对齐 UTC 00:00；tz 偏移把边界卷到本地零点
    ///（+8h：本地 2026-01-15 00:00 = UTC 2026-01-14 16:00）。
    #[test]
    fn calendar_daily_aligns_utc_midnight() {
        let now = at(2026, 1, 15, 10, 30);
        let start = Period::Daily.window_start(now, 0).expect("daily 有起点");
        assert_eq!(rfc3339(start), "2026-01-15T00:00:00+00:00");
        // 窗内任意时刻同起点；重置 = 次日零点
        assert_eq!(
            Period::Daily.window_start(now + 3600, 0),
            Some(start),
            "同日内起点稳定"
        );
        assert_eq!(Period::Daily.reset_at(start, 0), start + DAY_SECS);
        // tz 偏移 +8h：本地日界 = UTC 前一日 16:00
        let start_tz = Period::Daily
            .window_start(now, 8 * 3600)
            .expect("daily 有起点");
        assert_eq!(rfc3339(start_tz), "2026-01-14T16:00:00+00:00");
        // 负偏移 -5h：本地日界 = UTC 当日 05:00
        let start_neg = Period::Daily.window_start(now, -5 * 3600).unwrap();
        assert_eq!(rfc3339(start_neg), "2026-01-15T05:00:00+00:00");
        // 重置 = 下一个本地零点（与起点同参成对）
        assert_eq!(
            Period::Daily.reset_at(start_tz, 8 * 3600) - start_tz,
            DAY_SECS
        );
    }

    /// calendar 周窗对齐 ISO 周一 00:00（2026-01-05 为周一；周三正午落在
    /// 当周一窗），跨年边界取整周。
    #[test]
    fn calendar_weekly_aligns_iso_monday() {
        let wednesday_noon = at(2026, 1, 7, 12, 0);
        let start = Period::Weekly
            .window_start(wednesday_noon, 0)
            .expect("weekly 有起点");
        assert_eq!(rfc3339(start), "2026-01-05T00:00:00+00:00");
        assert_eq!(Period::Weekly.reset_at(start, 0), start + 7 * DAY_SECS);
        // 周一 00:00 本身即起点（含端点）；周日深夜同窗
        let monday = at(2026, 1, 5, 0, 0);
        assert_eq!(Period::Weekly.window_start(monday, 0), Some(start));
        let sunday = at_secs(2026, 1, 11, 23, 59, 59);
        assert_eq!(Period::Weekly.window_start(sunday, 0), Some(start));
        assert_eq!(
            Period::Weekly.window_start(sunday + 1, 0).unwrap(),
            start + 7 * DAY_SECS,
            "周一 00:00 起翻入下窗"
        );
    }

    /// calendar 月窗对齐自然月 1 日 00:00；重置 = 下月 1 日（28-31 天变长，
    /// 含闰年 2 月）；tz 偏移同日窗卷动。
    #[test]
    fn calendar_monthly_aligns_first_day_and_variable_length() {
        let now = at(2026, 3, 15, 8, 0);
        let start = Period::Monthly
            .window_start(now, 0)
            .expect("monthly 有起点");
        assert_eq!(rfc3339(start), "2026-03-01T00:00:00+00:00");
        // 3 月 31 天
        assert_eq!(Period::Monthly.reset_at(start, 0) - start, 31 * DAY_SECS);
        assert_eq!(
            rfc3339(Period::Monthly.reset_at(start, 0)),
            "2026-04-01T00:00:00+00:00"
        );
        // 非闰年 2 月 28 天；跨年 12 月 → 1 月
        let feb = at(2026, 2, 10, 0, 0);
        let feb_start = Period::Monthly.window_start(feb, 0).unwrap();
        assert_eq!(
            Period::Monthly.reset_at(feb_start, 0) - feb_start,
            28 * DAY_SECS
        );
        let dec = at(2026, 12, 31, 23, 0);
        let dec_start = Period::Monthly.window_start(dec, 0).unwrap();
        assert_eq!(
            rfc3339(Period::Monthly.reset_at(dec_start, 0)),
            "2027-01-01T00:00:00+00:00",
            "跨年月窗正常翻入 1 月"
        );
        // 闰年 2 月 29 天
        let leap_feb = at(2028, 2, 10, 0, 0);
        let leap_start = Period::Monthly.window_start(leap_feb, 0).unwrap();
        assert_eq!(
            Period::Monthly.reset_at(leap_start, 0) - leap_start,
            29 * DAY_SECS
        );
    }

    /// 月窗 + tz 回归（monthly+tz 曾把 reset_at 算在 UTC 帧上：tz=+8h 的
    /// 月窗塌缩为 8 小时、tz=-8h 提前 8 小时翻转）。修复口径：起点与重置
    /// 时刻同取本地帧——tz=+8h 的 3 月窗 = 本地 3-01 00:00（UTC 2-28T16:00）
    /// 至本地 4-01 00:00（UTC 3-31T16:00），恒 31 天。
    #[test]
    fn monthly_with_tz_keeps_full_calendar_month() {
        let tz_east8 = 8 * 3600;
        // 本地 2026-03-15 12:00 (+08) = UTC 03-15 04:00 → 窗起点 = 本地 3-01 00:00
        let now = at(2026, 3, 15, 4, 0);
        let start = Period::Monthly.window_start(now, tz_east8).expect("起点");
        assert_eq!(rfc3339(start), "2026-02-28T16:00:00+00:00", "本地月初午夜");
        let reset = Period::Monthly.reset_at(start, tz_east8);
        assert_eq!(
            rfc3339(reset),
            "2026-03-31T16:00:00+00:00",
            "重置 = 本地 4-01 00:00（UTC 帧落在 3-31T16:00）"
        );
        assert_eq!(
            Period::Monthly.period_secs(start, tz_east8),
            31 * DAY_SECS,
            "窗长必须等于完整自然月，不得塌缩为 |tz| 小时"
        );
        // tz=+14h（审查复现例）：本地 2026-02 月窗（起点 UTC 1-31T10:00）
        // 恒 28 天（2026 平年 2 月），不得塌缩为 14 小时
        let tz_east14 = 14 * 3600;
        let now_feb = at(2026, 2, 10, 0, 0);
        let feb14_start = Period::Monthly.window_start(now_feb, tz_east14).unwrap();
        assert_eq!(
            rfc3339(feb14_start),
            "2026-01-31T10:00:00+00:00",
            "本地月初午夜"
        );
        assert_eq!(
            Period::Monthly.period_secs(feb14_start, tz_east14),
            28 * DAY_SECS
        );
        assert_eq!(
            Period::Monthly.reset_at(feb14_start, tz_east14),
            days_from_civil(2026, 3, 1) * DAY_SECS - tz_east14,
            "重置 = 本地 3-01 00:00 (+14) = UTC 2-28T10:00"
        );
        // tz=-5h：本地 3-01 00:00 = UTC 3-01T05:00，重置 = UTC 4-01T05:00
        let tz_west5 = -5 * 3600;
        let start_west = Period::Monthly.window_start(now, tz_west5).unwrap();
        assert_eq!(rfc3339(start_west), "2026-03-01T05:00:00+00:00");
        assert_eq!(
            Period::Monthly.period_secs(start_west, tz_west5),
            31 * DAY_SECS
        );
        // 活动期判定：窗内未翻转、reset_at 时刻端点即翻转（同参成对）
        assert!(!Period::Monthly.is_expired(start, reset - 1, tz_east8));
        assert!(Period::Monthly.is_expired(start, reset, tz_east8));
    }

    /// 起点与重置的闭环不变量：任意 tz 下 `reset_at(window_start(now))` 恰为
    /// 下一窗起点（对下一窗再取起点得自身）。
    #[test]
    fn monthly_reset_is_next_window_start_across_timezones() {
        for tz in [-12 * 3600, -5 * 3600, 0, 8 * 3600, 14 * 3600] {
            for month in 1..=12u32 {
                let now = at(2026, month, 15, 8, 0);
                let start = Period::Monthly
                    .window_start(now, tz)
                    .expect("monthly 恒有起点");
                let reset = Period::Monthly.reset_at(start, tz);
                assert!(reset > start, "tz={tz} month={month}: 重置必须晚于起点");
                assert_eq!(
                    Period::Monthly.window_start(reset, tz),
                    Some(reset),
                    "tz={tz} month={month}: 重置时刻必须是下一窗起点（本地帧闭环）"
                );
                assert!(
                    Period::Monthly.period_secs(start, tz) >= 28 * DAY_SECS
                        && Period::Monthly.period_secs(start, tz) <= 31 * DAY_SECS,
                    "tz={tz} month={month}: 窗长必须落在 28-31 天"
                );
            }
        }
    }

    /// 配置编译单点：period/calendar/rolling_secs 三字段 → Period；
    /// 非法组合 None（validate 已拒，此处仅手构兜底）。
    #[test]
    fn period_from_config_covers_both_forms() {
        assert_eq!(
            Period::from_config("rolling", "", 1800),
            Some(Period::Rolling(1800))
        );
        assert_eq!(
            Period::from_config("calendar", "daily", 0),
            Some(Period::Daily)
        );
        assert_eq!(
            Period::from_config("calendar", "weekly", 0),
            Some(Period::Weekly)
        );
        assert_eq!(
            Period::from_config("calendar", "monthly", 0),
            Some(Period::Monthly)
        );
        assert_eq!(Period::from_config("rolling", "daily", 60), None);
        assert_eq!(Period::from_config("hourly", "", 60), None);
        assert_eq!(Period::from_config("calendar", "yearly", 0), None);
    }
}
