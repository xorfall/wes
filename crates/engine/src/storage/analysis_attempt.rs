//! One accounting law for native admission and encoded checkpoint replay.
//! Identity and captured-value equality are checked by their owning representation.
use super::datasets::{AnalysisBudget, AnalysisDuration, AnalysisUsage, AnalysisWork};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisAttemptKind {
    Resume,
    Continue,
}

/// Charging a reservation changes charged totals, never measured completed work.
pub fn charge_interruption(work: &AnalysisWork, duration: &AnalysisDuration) -> Option<(u64, u64)> {
    Some((
        work.charged.checked_add(work.outstanding)?,
        duration.spent_ms.checked_add(duration.outstanding_ms)?,
    ))
}

pub struct AttemptAccounting<'a> {
    pub budget: &'a AnalysisBudget,
    pub work: &'a AnalysisWork,
    pub usage: &'a AnalysisUsage,
    pub duration: &'a AnalysisDuration,
}
impl AttemptAccounting<'_> {
    /// Does not establish caller authority; kind must come from an admitted operation.
    pub fn follows(&self, old: &Self, attempt: &str, kind: AnalysisAttemptKind) -> bool {
        let Some((charged, spent_ms)) = charge_interruption(old.work, old.duration) else {
            return false;
        };
        let allowance = match kind {
            AnalysisAttemptKind::Resume if self.budget == old.budget => {
                Some(old.usage.work_allowance)
            }
            AnalysisAttemptKind::Continue => {
                self.budget
                    .allowance_after(old.budget, old.usage.work_allowance, attempt)
            }
            _ => None,
        };
        let mut usage = old.usage.clone();
        usage.work_allowance = allowance.unwrap_or(0);
        allowance.is_some()
            && self.budget.valid()
            && *self.usage == usage
            && self.work.completed == old.work.completed
            && self.work.charged == charged
            && self.work.outstanding > 0
            && self
                .work
                .charged
                .checked_add(self.work.outstanding)
                .is_some_and(|n| n <= usage.work_allowance && n <= self.budget.totals.work)
            && old.work.granted.checked_add(self.work.outstanding) == Some(self.work.granted)
            && old.work.grants.checked_add(1) == Some(self.work.grants)
            && self.duration.spent_ms == spent_ms
            && self.budget.totals.duration_ms.checked_sub(spent_ms)
                == Some(self.duration.outstanding_ms)
            && self.duration.outstanding_ms > 0
            && self.duration.valid(self.budget.totals.duration_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raised_bounds_need_explicit_admission_and_preserve_every_prior_debit() {
        let mut totals = crate::scan::Settings::capture().totals();
        totals.work = 1000;
        totals.duration_ms = 1000;
        let old = AnalysisBudget::initial(
            "00000000-0000-0000-0000-000000000001".into(),
            "00000000-0000-0000-0000-000000000002".into(),
            totals,
            crate::scan::Settings::capture().totals(),
        );
        let mut new = old.clone();
        new.issued_attempt = "00000000-0000-0000-0000-000000000003".into();
        new.previous = Some(old.digest());
        new.totals.work = 1200;
        new.totals.duration_ms = 1200;
        new.work_grant = 200;
        new.authorized_work = 200;
        let old_work = AnalysisWork {
            granted: 150,
            completed: 75,
            charged: 100,
            outstanding: 50,
            grants: 1,
        };
        let new_work = AnalysisWork {
            granted: 350,
            completed: 75,
            charged: 150,
            outstanding: 200,
            grants: 2,
        };
        let old_usage = AnalysisUsage {
            work_allowance: 400,
            ..Default::default()
        };
        let new_usage = AnalysisUsage {
            work_allowance: 600,
            ..Default::default()
        };
        let old_time = AnalysisDuration {
            spent_ms: 20,
            outstanding_ms: 980,
        };
        let new_time = AnalysisDuration {
            spent_ms: 1000,
            outstanding_ms: 200,
        };
        let prior = AttemptAccounting {
            budget: &old,
            work: &old_work,
            usage: &old_usage,
            duration: &old_time,
        };
        let next = AttemptAccounting {
            budget: &new,
            work: &new_work,
            usage: &new_usage,
            duration: &new_time,
        };
        assert!(next.follows(&prior, &new.issued_attempt, AnalysisAttemptKind::Continue));
        assert!(!next.follows(&prior, &new.issued_attempt, AnalysisAttemptKind::Resume));
        let mut forged_usage = new_usage.clone();
        forged_usage.input_bytes = 1;
        assert!(
            !AttemptAccounting {
                usage: &forged_usage,
                ..next
            }
            .follows(&prior, &new.issued_attempt, AnalysisAttemptKind::Continue)
        );
        let mut forged_work = new_work.clone();
        forged_work.charged = 100;
        assert!(
            !AttemptAccounting {
                work: &forged_work,
                ..next
            }
            .follows(&prior, &new.issued_attempt, AnalysisAttemptKind::Continue)
        );
        let reset_time = AnalysisDuration {
            spent_ms: 20,
            outstanding_ms: 1180,
        };
        assert!(
            !AttemptAccounting {
                duration: &reset_time,
                ..next
            }
            .follows(&prior, &new.issued_attempt, AnalysisAttemptKind::Continue)
        );
    }

    #[test]
    fn active_time_is_reserved_through_creation_and_commit_gaps() {
        let old = AnalysisDuration {
            spent_ms: 10,
            outstanding_ms: 90,
        };
        assert!(old.reservation_valid(100, true));
        assert!(!old.reservation_valid(100, false));
        assert!(
            !AnalysisDuration {
                spent_ms: 20,
                outstanding_ms: 0
            }
            .settles(&old, 100, true)
        );
        assert!(
            AnalysisDuration {
                spent_ms: 20,
                outstanding_ms: 80
            }
            .settles(&old, 100, true)
        );
        assert!(
            AnalysisDuration {
                spent_ms: 20,
                outstanding_ms: 0
            }
            .settles(&old, 100, false)
        );
        assert!(
            !AnalysisDuration {
                spent_ms: 9,
                outstanding_ms: 91
            }
            .settles(&old, 100, true)
        );
        assert!(
            !AnalysisDuration {
                spent_ms: 101,
                outstanding_ms: 0
            }
            .settles(&old, 100, false)
        );
    }
    #[test]
    fn interruption_charges_once_without_claiming_unmeasured_work() {
        let mut work = AnalysisWork {
            granted: 40,
            completed: 3,
            charged: 10,
            outstanding: 30,
            grants: 2,
        };
        let mut time = AnalysisDuration {
            spent_ms: 5,
            outstanding_ms: 20,
        };
        assert_eq!(charge_interruption(&work, &time), Some((40, 25)));
        work.charged = 40;
        work.outstanding = 0;
        time.spent_ms = 25;
        time.outstanding_ms = 0;
        assert_eq!(charge_interruption(&work, &time), Some((40, 25)));
        assert_eq!(work.completed, 3);
        work.charged = u64::MAX;
        work.outstanding = 1;
        assert_eq!(charge_interruption(&work, &time), None);
    }
}
