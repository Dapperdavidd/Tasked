use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

/// When a task template fires.
///
/// The serialised shape is a contract in three places at once: the
/// `task_templates.cadence` JSONB column, the `cadence_has_known_type` check
/// constraint that guards it, and the ingestion pipeline's generated output.
/// It is defined once, here, so those three cannot drift apart.
///
/// ```json
/// { "type": "daily" }
/// { "type": "weekly_days", "days": [1, 3, 5] }
/// { "type": "n_per_week", "count": 3 }
/// { "type": "once", "day_offset": 12 }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// `deny_unknown_fields` is deliberately absent: serde does not support it on
// internally tagged enums, so adding it would imply a guarantee that silently
// does not hold. Unknown *types* are still rejected, which is the case that
// matters — an unrecognised cadence reaching the materialiser means a user's
// day quietly has no tasks in it.
#[serde(tag = "type", rename_all = "snake_case")]
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

    /// The wire shape is a contract with the database check constraint and with
    /// the ingestion pipeline. Assert it literally rather than by round trip
    /// alone, so a rename cannot pass while silently breaking stored rows.
    #[test]
    fn serialises_to_the_shape_the_database_constraint_expects() {
        let cases = [
            (Cadence::Daily, r#"{"type":"daily"}"#),
            (
                Cadence::WeeklyDays {
                    days: vec![1, 3, 5],
                },
                r#"{"type":"weekly_days","days":[1,3,5]}"#,
            ),
            (
                Cadence::NPerWeek { count: 3 },
                r#"{"type":"n_per_week","count":3}"#,
            ),
            (
                Cadence::Once { day_offset: 12 },
                r#"{"type":"once","day_offset":12}"#,
            ),
        ];

        for (cadence, json) in cases {
            let encoded = serde_json::to_string(&cadence).expect("cadence serialises");
            assert_eq!(encoded, json);

            let decoded: Cadence = serde_json::from_str(json).expect("cadence deserialises");
            assert_eq!(decoded, cadence);
        }
    }

    #[test]
    fn refuses_an_unknown_cadence_type_at_the_boundary() {
        // Matches the database's cadence_has_known_type constraint. Discovering
        // an unknown cadence in the materialiser means a user's day silently
        // has no tasks in it.
        assert!(serde_json::from_str::<Cadence>(r#"{"type":"every_other_tuesday"}"#).is_err());
        assert!(serde_json::from_str::<Cadence>(r#"{"days":[1]}"#).is_err());
        assert!(serde_json::from_str::<Cadence>(r#"{"type":"weekly_days"}"#).is_err());

        // Documented limitation rather than an oversight: serde ignores extra
        // fields on internally tagged enums and offers no way to refuse them.
        // Harmless here, because the tag alone determines the semantics.
        assert_eq!(
            serde_json::from_str::<Cadence>(r#"{"type":"daily","count":3}"#).expect("tolerated"),
            Cadence::Daily
        );
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
