#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayStatus {
    Complete,
    Partial,
    Missed,
    Rest,
}

pub fn task_points(difficulty: u8, estimated_minutes: u16) -> i32 {
    const BASE: [i32; 5] = [10, 15, 25, 40, 60];

    let clamped_difficulty = difficulty.clamp(1, 5);
    let base = BASE[(clamped_difficulty - 1) as usize] as f32;
    let weight = (0.5 + estimated_minutes as f32 / 60.0).clamp(0.5, 2.5);

    (base * weight).round() as i32
}

pub fn day_score(earned_points: i32, available_points: i32) -> u8 {
    if available_points <= 0 {
        return 0;
    }

    let score = (earned_points.max(0) as f32 / available_points as f32 * 100.0).round();
    score.clamp(0.0, 100.0) as u8
}

pub fn status_from_score(score: u8, declared_rest: bool) -> DayStatus {
    if declared_rest {
        return DayStatus::Rest;
    }

    match score {
        80..=100 => DayStatus::Complete,
        50..=79 => DayStatus::Partial,
        _ => DayStatus::Missed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_task_points_from_prd_formula() {
        assert_eq!(task_points(1, 0), 5);
        assert_eq!(task_points(1, 30), 10);
        assert_eq!(task_points(3, 60), 38);
        assert_eq!(task_points(5, 480), 150);
    }

    #[test]
    fn clamps_invalid_difficulty() {
        assert_eq!(task_points(0, 30), task_points(1, 30));
        assert_eq!(task_points(6, 30), task_points(5, 30));
    }

    #[test]
    fn derives_day_score_and_status() {
        assert_eq!(day_score(4, 5), 80);
        assert_eq!(status_from_score(80, false), DayStatus::Complete);
        assert_eq!(status_from_score(79, false), DayStatus::Partial);
        assert_eq!(status_from_score(49, false), DayStatus::Missed);
        assert_eq!(status_from_score(100, true), DayStatus::Rest);
    }
}
