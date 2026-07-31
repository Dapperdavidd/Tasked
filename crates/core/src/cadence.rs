use chrono::{Datelike, NaiveDate};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cadence {
    Daily,
    WeeklyDays { days: Vec<u8> },
    NPerWeek { count: u8 },
    Once { day_offset: u32 },
}

impl Cadence {
    pub fn fires_on(&self, day_index: u32, local_date: NaiveDate) -> bool {
        match self {
            Self::Daily => true,
            Self::WeeklyDays { days } => {
                let iso_weekday = local_date.weekday().number_from_monday() as u8;
                days.contains(&iso_weekday)
            }
            Self::NPerWeek { count } => *count > 0,
            Self::Once { day_offset } => *day_offset == day_index,
        }
    }

    pub fn validate_for_program(&self, is_standing: bool) -> Result<(), CadenceError> {
        match self {
            Self::Daily => Ok(()),
            Self::WeeklyDays { days } => {
                if days.is_empty() || days.iter().any(|day| !(1..=7).contains(day)) {
                    return Err(CadenceError::InvalidWeekday);
                }
                Ok(())
            }
            Self::NPerWeek { count } => {
                if !(1..=7).contains(count) {
                    return Err(CadenceError::InvalidWeeklyCount);
                }
                Ok(())
            }
            Self::Once { .. } if is_standing => Err(CadenceError::OnceCadenceOnStandingProgram),
            Self::Once { .. } => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CadenceError {
    InvalidWeekday,
    InvalidWeeklyCount,
    OnceCadenceOnStandingProgram,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("test date is valid")
    }

    #[test]
    fn daily_fires_every_day() {
        assert!(Cadence::Daily.fires_on(99, date(2026, 7, 31)));
    }

    #[test]
    fn weekly_days_uses_iso_weekdays() {
        let friday = date(2026, 7, 31);
        assert!(Cadence::WeeklyDays { days: vec![5] }.fires_on(0, friday));
        assert!(!Cadence::WeeklyDays { days: vec![1] }.fires_on(0, friday));
    }

    #[test]
    fn n_per_week_is_floating_and_visible_until_bucket_logic_hides_it() {
        assert!(Cadence::NPerWeek { count: 3 }.fires_on(0, date(2026, 7, 31)));
        assert!(!Cadence::NPerWeek { count: 0 }.fires_on(0, date(2026, 7, 31)));
    }

    #[test]
    fn once_fires_only_on_matching_day_index() {
        let cadence = Cadence::Once { day_offset: 12 };
        assert!(cadence.fires_on(12, date(2026, 8, 12)));
        assert!(!cadence.fires_on(11, date(2026, 8, 11)));
    }

    #[test]
    fn rejects_invalid_cadence_shapes() {
        assert_eq!(
            Cadence::WeeklyDays { days: vec![0] }.validate_for_program(false),
            Err(CadenceError::InvalidWeekday)
        );
        assert_eq!(
            Cadence::NPerWeek { count: 8 }.validate_for_program(false),
            Err(CadenceError::InvalidWeeklyCount)
        );
        assert_eq!(
            Cadence::Once { day_offset: 1 }.validate_for_program(true),
            Err(CadenceError::OnceCadenceOnStandingProgram)
        );
    }
}
