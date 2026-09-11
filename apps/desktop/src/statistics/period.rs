use std::collections::BTreeMap;

use chrono::{Datelike, Duration, Months, NaiveDate};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Period {
    #[default]
    Week,
    Month,
    Year,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DateRange {
    pub start: Option<NaiveDate>,
    pub end: NaiveDate,
}

impl Period {
    pub fn distribution(
        self,
        range: DateRange,
        days: &BTreeMap<String, u64>,
    ) -> Vec<(NaiveDate, u64)> {
        match self {
            Self::Week | Self::Month => {
                let mut values = Vec::new();
                let mut day = range.start.unwrap();
                while day <= range.end {
                    values.push((day, days.get(&day.to_string()).copied().unwrap_or(0)));
                    let Some(next) = day.succ_opt() else { break };
                    day = next;
                }
                values
            }
            Self::Year => range.months(days),
            Self::All => {
                let mut years = BTreeMap::<i32, u64>::new();
                for (day, ms) in days.iter().filter(|(day, _)| range.contains(day)) {
                    let year = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap().year();
                    *years.entry(year).or_default() += ms;
                }
                let first = years.keys().next().copied().unwrap_or(range.end.year());
                (first..=range.end.year())
                    .map(|year| {
                        (
                            NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
                            years.get(&year).copied().unwrap_or(0),
                        )
                    })
                    .collect()
            }
        }
    }

    pub fn range(self, today: NaiveDate, offset: i32) -> DateRange {
        let start = match self {
            Self::Week => {
                today - Duration::days(i64::from(today.weekday().num_days_from_monday()))
                    + Duration::weeks(i64::from(offset))
            }
            Self::Month => {
                let first = today.with_day(1).unwrap();
                let months = Months::new(offset.unsigned_abs());
                if offset < 0 {
                    first.checked_sub_months(months).unwrap()
                } else {
                    first.checked_add_months(months).unwrap()
                }
            }
            Self::Year => NaiveDate::from_ymd_opt(today.year() + offset, 1, 1).unwrap(),
            Self::All => {
                return DateRange {
                    start: None,
                    end: today,
                };
            }
        };
        let end = match self {
            Self::Week => start + Duration::days(6),
            Self::Month => start.checked_add_months(Months::new(1)).unwrap() - Duration::days(1),
            Self::Year => NaiveDate::from_ymd_opt(start.year(), 12, 31).unwrap(),
            Self::All => unreachable!(),
        };
        DateRange {
            start: Some(start),
            end,
        }
    }
}

impl DateRange {
    pub fn contains(self, day: &str) -> bool {
        NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .is_ok_and(|date| self.start.is_none_or(|start| date >= start) && date <= self.end)
    }

    pub fn total(self, days: &BTreeMap<String, u64>) -> u64 {
        days.iter()
            .filter(|(day, _)| self.contains(day))
            .map(|(_, ms)| ms)
            .sum()
    }

    pub fn months(self, days: &BTreeMap<String, u64>) -> Vec<(NaiveDate, u64)> {
        let mut totals = BTreeMap::<NaiveDate, u64>::new();
        for (day, ms) in days.iter().filter(|(day, _)| self.contains(day)) {
            let month = NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .unwrap()
                .with_day(1)
                .unwrap();
            *totals.entry(month).or_default() += ms;
        }
        let first = self
            .start
            .or_else(|| totals.keys().next().copied())
            .unwrap_or(self.end);
        let mut month = first.with_day(1).unwrap();
        let last = self.end.with_day(1).unwrap();
        let mut values = Vec::new();
        while month <= last {
            values.push((month, totals.get(&month).copied().unwrap_or(0)));
            let Some(next) = month.checked_add_months(Months::new(1)) else {
                break;
            };
            month = next;
        }
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn all_time_distribution_groups_years_and_keeps_empty_years() {
        let days = BTreeMap::from([
            ("2023-01-01".into(), 100),
            ("2023-12-31".into(), 200),
            ("2025-06-01".into(), 400),
            ("2026-01-01".into(), 800),
            ("2027-01-01".into(), 1600),
        ]);
        let range = Period::All.range(date("2026-09-11"), 0);
        assert_eq!(
            Period::All.distribution(range, &days),
            [
                (date("2023-01-01"), 300),
                (date("2024-01-01"), 0),
                (date("2025-01-01"), 400),
                (date("2026-01-01"), 800),
            ]
        );
    }

    #[test]
    fn daily_distribution_covers_calendar_week_and_each_month_length() {
        let days = BTreeMap::from([("2025-12-31".into(), 120), ("2026-01-01".into(), 240)]);
        let week = Period::Week.distribution(Period::Week.range(date("2026-01-01"), 0), &days);
        assert_eq!(week.len(), 7);
        assert_eq!(week[0], (date("2025-12-29"), 0));
        assert_eq!(week[2].1, 120);
        assert_eq!(week[3].1, 240);
        assert_eq!(week[6], (date("2026-01-04"), 0));
        for (today, count) in [
            ("2024-02-15", 29),
            ("2025-02-15", 28),
            ("2026-04-15", 30),
            ("2026-01-15", 31),
        ] {
            let values = Period::Month.distribution(Period::Month.range(date(today), 0), &days);
            assert_eq!(values.len(), count);
            assert_eq!(values.last().unwrap().0.day(), count as u32);
        }
    }

    #[test]
    fn calendar_periods_cross_years_and_include_leap_days() {
        assert_eq!(
            Period::Week.range(date("2026-01-01"), 0),
            DateRange {
                start: Some(date("2025-12-29")),
                end: date("2026-01-04"),
            }
        );
        assert_eq!(
            Period::Week.range(date("2026-01-01"), -1).end,
            date("2025-12-28")
        );
        assert_eq!(
            Period::Month.range(date("2024-03-31"), -1),
            DateRange {
                start: Some(date("2024-02-01")),
                end: date("2024-02-29"),
            }
        );
        assert_eq!(
            Period::Month.range(date("2026-01-31"), -1).start,
            Some(date("2025-12-01"))
        );
        assert_eq!(
            Period::Year.range(date("2026-09-11"), -1),
            DateRange {
                start: Some(date("2025-01-01")),
                end: date("2025-12-31"),
            }
        );
    }

    #[test]
    fn monthly_distribution_respects_partial_months_and_fills_gaps() {
        let days = BTreeMap::from([
            ("2025-12-28".into(), 900),
            ("2025-12-29".into(), 100),
            ("2026-01-04".into(), 200),
            ("2026-01-05".into(), 800),
            ("2026-03-01".into(), 400),
        ]);
        let week = Period::Week.range(date("2026-01-01"), 0);
        assert_eq!(week.total(&days), 300);
        assert_eq!(
            week.months(&days),
            [(date("2025-12-01"), 100), (date("2026-01-01"), 200)]
        );
        let year = Period::Year.range(date("2026-09-11"), 0).months(&days);
        assert_eq!(year.len(), 12);
        assert_eq!(year[0].1, 1000);
        assert_eq!(year[1].1, 0);
        assert_eq!(year[2].1, 400);
        let all = Period::All.range(date("2026-09-11"), 0).months(&days);
        assert_eq!(all.len(), 10);
        assert_eq!(all.iter().map(|(_, ms)| ms).sum::<u64>(), 2400);
    }
}
