//! Local calendar days for the thread list: when a snooze ends and which day a date falls on.
//!
//! The arithmetic is pure over the offset from UTC, which the caller supplies for any Unix time
//! (see [`system_offset`]), so a test can pick any zone, daylight saving included.

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;

/// Seconds east of UTC at the Unix time `seconds`.
pub type OffsetAt<'a> = &'a dyn Fn(i64) -> i64;

/// The system time zone's offset from UTC at `seconds`, 0 if it cannot be read.
pub fn system_offset(seconds: i64) -> i64 {
    let seconds: libc::time_t = seconds;
    let mut value = std::mem::MaybeUninit::uninit();
    if unsafe { libc::localtime_r(&seconds, value.as_mut_ptr()) }.is_null() {
        return 0;
    }
    unsafe { value.assume_init() }.tm_gmtoff
}

/// The local day `ms` (Unix milliseconds) falls on, counted from 1970-01-01.
pub fn day_of(ms: i64, offset_at: OffsetAt) -> i64 {
    let seconds = ms.div_euclid(1000);
    (seconds + offset_at(seconds)).div_euclid(DAY)
}

/// The weekday of `day`, 0 for Sunday.
pub fn weekday(day: i64) -> i64 {
    (day + 4).rem_euclid(7)
}

/// The moment (Unix milliseconds) `seconds_into_day` after the local midnight that starts `day`.
pub fn at_local(day: i64, seconds_into_day: i64, offset_at: OffsetAt) -> i64 {
    let local = day * DAY + seconds_into_day;
    let mut seconds = local - offset_at(local);
    seconds = local - offset_at(seconds);
    seconds * 1000
}

/// When a snooze ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnoozeUntil {
    /// Three hours from now.
    LaterToday,
    /// 9:00 tomorrow.
    Tomorrow,
    /// 9:00 on the next Monday, never today.
    NextWeek,
}

impl SnoozeUntil {
    pub const ALL: [SnoozeUntil; 3] = [Self::LaterToday, Self::Tomorrow, Self::NextWeek];

    pub fn label(self) -> &'static str {
        match self {
            Self::LaterToday => "Later today",
            Self::Tomorrow => "Tomorrow",
            Self::NextWeek => "Next week",
        }
    }

    /// The hint beside the label in a menu: when it ends.
    pub fn hint(self) -> &'static str {
        match self {
            Self::LaterToday => "in 3 hours",
            Self::Tomorrow => "9:00",
            Self::NextWeek => "Monday 9:00",
        }
    }

    pub fn at(self, now_ms: i64, offset_at: OffsetAt) -> i64 {
        const NINE: i64 = 9 * HOUR;
        let today = day_of(now_ms, offset_at);
        match self {
            Self::LaterToday => now_ms + 3 * HOUR * 1000,
            Self::Tomorrow => at_local(today + 1, NINE, offset_at),
            Self::NextWeek => {
                let days_ahead = match (1 - weekday(today)).rem_euclid(7) {
                    0 => 7,
                    days => days,
                };
                at_local(today + days_ahead, NINE, offset_at)
            }
        }
    }
}

/// Where a snoozed thread's wake time falls, for the Snoozed panel's groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeDay {
    LaterToday,
    Tomorrow,
    Later,
}

impl WakeDay {
    pub fn label(self) -> &'static str {
        match self {
            Self::LaterToday => "Later today",
            Self::Tomorrow => "Tomorrow",
            Self::Later => "Later",
        }
    }

    pub fn of(until_ms: i64, now_ms: i64, offset_at: OffsetAt) -> Self {
        match day_of(until_ms, offset_at) - day_of(now_ms, offset_at) {
            ..=0 => Self::LaterToday,
            1 => Self::Tomorrow,
            _ => Self::Later,
        }
    }
}

/// How long ago a settled thread was put away, for the Settled panel's groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettledAge {
    Today,
    ThisWeek,
    Earlier,
}

impl SettledAge {
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::ThisWeek => "This week",
            Self::Earlier => "Earlier",
        }
    }

    pub fn of(settled_ms: i64, now_ms: i64, offset_at: OffsetAt) -> Self {
        match day_of(now_ms, offset_at) - day_of(settled_ms, offset_at) {
            ..=0 => Self::Today,
            1..=6 => Self::ThisWeek,
            _ => Self::Earlier,
        }
    }
}

/// The time in a panel row, short: "2 PM", "Mon 9 AM" or "Oct 28", by how far `ms` is from
/// `now_ms`. A time on today's date or tomorrow's shows the clock; within a week the weekday;
/// further the month and day. Times with minutes keep them: "9:30 AM".
pub fn short_when(ms: i64, now_ms: i64, offset_at: OffsetAt) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let day = day_of(ms, offset_at);
    let ahead = day - day_of(now_ms, offset_at);
    let seconds = ms.div_euclid(1000);
    let into_day = (seconds + offset_at(seconds)).rem_euclid(DAY);
    let (hour, minute) = (into_day / HOUR, into_day % HOUR / 60);
    let clock = format!(
        "{}{} {}",
        if hour % 12 == 0 { 12 } else { hour % 12 },
        if minute == 0 {
            String::new()
        } else {
            format!(":{minute:02}")
        },
        if hour < 12 { "AM" } else { "PM" }
    );
    let weekday_name = DAYS[weekday(day) as usize];
    match ahead {
        0..=1 => clock,
        -6..=-1 => weekday_name.to_string(),
        2..=6 => format!("{weekday_name} {clock}"),
        _ => {
            let (_, month, day_of_month) = civil(day);
            format!("{} {day_of_month}", MONTHS[month as usize - 1])
        }
    }
}

/// The calendar date of `day` days since 1970-01-01 as (year, month 1..=12, day of month).
fn civil(day: i64) -> (i64, i64, i64) {
    let shifted = day + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day_of_month = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day_of_month)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IST: i64 = 5 * HOUR + 1800;

    fn utc(_: i64) -> i64 {
        0
    }

    fn ist(_: i64) -> i64 {
        IST
    }

    fn ms(day: i64, hour: i64, minute: i64) -> i64 {
        (day * DAY + hour * HOUR + minute * 60) * 1000
    }

    #[test]
    fn days_and_weekdays_count_from_the_epoch() {
        assert_eq!(day_of(0, &utc), 0);
        assert_eq!(weekday(0), 4);
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_453), (2025, 12, 31));
        assert_eq!(civil(20_511), (2026, 2, 27));
        assert_eq!(civil(20_573), (2026, 4, 30));
        // 20:00 UTC on day 0 is 01:30 on day 1 at +5:30.
        assert_eq!(day_of(ms(0, 20, 0), &utc), 0);
        assert_eq!(day_of(ms(0, 20, 0), &ist), 1);
    }

    #[test]
    fn a_snooze_ends_three_hours_on_tomorrow_at_nine_or_next_monday_at_nine() {
        let wednesday = 20_761;
        assert_eq!(civil(wednesday), (2026, 11, 4));
        assert_eq!(weekday(wednesday), 3);
        let now = ms(wednesday, 14, 10);
        assert_eq!(SnoozeUntil::LaterToday.at(now, &utc), now + 3 * HOUR * 1000);
        assert_eq!(SnoozeUntil::Tomorrow.at(now, &utc), ms(wednesday + 1, 9, 0));
        assert_eq!(SnoozeUntil::NextWeek.at(now, &utc), ms(wednesday + 5, 9, 0));
        assert_eq!(weekday(wednesday + 5), 1);

        let monday = wednesday - 2;
        let on_monday = ms(monday, 3, 0);
        assert_eq!(
            SnoozeUntil::NextWeek.at(on_monday, &utc),
            ms(monday + 7, 9, 0),
            "never today"
        );
        let sunday = wednesday - 3;
        assert_eq!(
            SnoozeUntil::NextWeek.at(ms(sunday, 20, 0), &utc),
            ms(sunday + 1, 9, 0)
        );
    }

    #[test]
    fn local_nine_follows_the_zone_and_its_clock_changes() {
        let wednesday = 20_761;
        let now = ms(wednesday, 3, 0);
        // 03:00 UTC is 08:30 IST the same day: tomorrow 09:00 IST is 03:30 UTC.
        assert_eq!(
            SnoozeUntil::Tomorrow.at(now, &ist),
            ms(wednesday + 1, 3, 30)
        );
        // A zone that springs forward the night before: 9:00 local is at +2 after the change.
        let spring = |seconds: i64| {
            if seconds >= (wednesday + 1) * DAY {
                2 * HOUR
            } else {
                HOUR
            }
        };
        assert_eq!(
            SnoozeUntil::Tomorrow.at(ms(wednesday, 12, 0), &spring),
            ms(wednesday + 1, 7, 0)
        );
    }

    #[test]
    fn panels_group_by_the_day_in_the_users_zone() {
        let wednesday = 20_761;
        let now = ms(wednesday, 14, 0);
        let day = |days: i64, hour| ms(wednesday + days, hour, 0);
        assert_eq!(WakeDay::of(day(0, 18), now, &utc), WakeDay::LaterToday);
        assert_eq!(WakeDay::of(day(1, 9), now, &utc), WakeDay::Tomorrow);
        assert_eq!(WakeDay::of(day(5, 9), now, &utc), WakeDay::Later);
        assert_eq!(SettledAge::of(day(0, 2), now, &utc), SettledAge::Today);
        assert_eq!(SettledAge::of(day(-1, 23), now, &utc), SettledAge::ThisWeek);
        assert_eq!(SettledAge::of(day(-6, 1), now, &utc), SettledAge::ThisWeek);
        assert_eq!(SettledAge::of(day(-7, 1), now, &utc), SettledAge::Earlier);
        // The same instant is a different day at +5:30.
        assert_eq!(
            SettledAge::of(day(-1, 20), day(0, 1), &ist),
            SettledAge::Today
        );
    }

    #[test]
    fn a_row_says_the_clock_the_weekday_or_the_date() {
        let wednesday = 20_761;
        let now = ms(wednesday, 14, 0);
        let at =
            |days: i64, hour, minute| short_when(ms(wednesday + days, hour, minute), now, &utc);
        assert_eq!(at(0, 14, 0), "2 PM");
        assert_eq!(at(0, 0, 0), "12 AM");
        assert_eq!(at(0, 9, 30), "9:30 AM");
        assert_eq!(at(1, 9, 0), "9 AM");
        assert_eq!(at(5, 9, 0), "Mon 9 AM");
        assert_eq!(at(-1, 11, 0), "Tue");
        assert_eq!(at(-6, 11, 0), "Thu");
        assert_eq!(at(-7, 11, 0), "Oct 28");
        assert_eq!(at(40, 11, 0), "Dec 14");
    }
}
