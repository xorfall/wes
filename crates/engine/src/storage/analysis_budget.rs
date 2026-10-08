//! Immutable resource grants are separate from a captured analysis's semantics.
//! A receipt identifies bounds; it is never an execution or read capability.
use crate::scan::Totals;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisBudget {
    pub version: u16,
    pub analysis: String,
    /// Attempt that explicitly issued these bounds; ordinary Resume keeps this receipt.
    pub issued_attempt: String,
    pub previous: Option<String>,
    pub totals: Totals,
    pub ceilings: Totals,
    /// Only an explicit increase of the work total supplies this additional allowance.
    pub work_grant: u64,
    pub authorized_work: u64,
}
impl AnalysisBudget {
    pub fn initial(analysis: String, attempt: String, totals: Totals, ceilings: Totals) -> Self {
        Self {
            version: 1,
            analysis,
            issued_attempt: attempt,
            previous: None,
            totals,
            ceilings,
            work_grant: 0,
            authorized_work: 0,
        }
    }
    pub fn valid(&self) -> bool {
        let uuid = |s: &str| {
            s.len() == 36 && uuid::Uuid::parse_str(s).is_ok_and(|u| u.hyphenated().to_string() == s)
        };
        self.version == 1
            && uuid(&self.analysis)
            && uuid(&self.issued_attempt)
            && self.totals.within(self.ceilings)
            && self.ceilings.valid()
            && self.work_grant <= self.authorized_work
            && self.authorized_work <= self.totals.work
            && match &self.previous {
                None => self.work_grant == 0 && self.authorized_work == 0,
                Some(previous) => valid_digest(previous),
            }
    }
    /// A canonical domain-separated identity; no ambient policy or current run is hashed in.
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"wes.analysis.budget.v1\0");
        hash.update(self.version.to_le_bytes());
        for field in [
            self.analysis.as_str(),
            self.issued_attempt.as_str(),
            self.previous.as_deref().unwrap_or(""),
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
        for n in self
            .totals
            .values()
            .into_iter()
            .chain(self.ceilings.values())
            .chain([self.work_grant, self.authorized_work])
        {
            hash.update(n.to_le_bytes());
        }
        format!("sha256:{:x}", hash.finalize())
    }
    pub fn raises(&self, old: &Self, attempt: &str) -> bool {
        self.valid()
            && old.valid()
            && self.analysis == old.analysis
            && self.issued_attempt == attempt
            && self.issued_attempt != old.issued_attempt
            && self.previous.as_deref() == Some(old.digest().as_str())
            && self.totals.contains(old.totals)
            && self.totals != old.totals
            && self.totals.work.checked_sub(old.totals.work) == Some(self.work_grant)
            && old.authorized_work.checked_add(self.work_grant) == Some(self.authorized_work)
    }
    pub fn allowance_after(&self, old: &Self, allowance: u64, attempt: &str) -> Option<u64> {
        self.raises(old, attempt).then_some(())?;
        if allowance > old.totals.work {
            return None;
        }
        allowance
            .checked_add(self.work_grant)
            .map(|n| n.min(self.totals.work))
    }
}
fn valid_digest(s: &str) -> bool {
    s.len() == 71
        && s.starts_with("sha256:")
        && s[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn budget() -> AnalysisBudget {
        let ceilings = crate::scan::Settings::capture().totals();
        let mut totals = ceilings;
        totals.work /= 2;
        AnalysisBudget::initial(
            "00000000-0000-0000-0000-000000000001".into(),
            "00000000-0000-0000-0000-000000000002".into(),
            totals,
            ceilings,
        )
    }
    #[test]
    fn explicit_delta_is_not_input_credit_or_a_debit_reset() {
        let old = budget();
        let mut next = old.clone();
        next.issued_attempt = "00000000-0000-0000-0000-000000000003".into();
        next.previous = Some(old.digest());
        next.totals.work += 100;
        next.work_grant = 100;
        next.authorized_work = 100;
        assert!(next.raises(&old, &next.issued_attempt));
        assert_eq!(
            next.allowance_after(&old, 700, &next.issued_attempt),
            Some(800)
        );
        next.work_grant += 1;
        assert!(!next.raises(&old, &next.issued_attempt));
        assert_eq!(next.allowance_after(&old, 700, &next.issued_attempt), None);
    }
    #[test]
    fn no_lowering_wrong_predecessor_or_above_ceiling_grant() {
        let old = budget();
        let mut next = old.clone();
        next.issued_attempt = "00000000-0000-0000-0000-000000000003".into();
        next.previous = Some(old.digest());
        next.totals.work += 1;
        next.work_grant = 1;
        next.authorized_work = 1;
        assert!(next.raises(&old, &next.issued_attempt));
        next.totals.input_records -= 1;
        assert!(!next.raises(&old, &next.issued_attempt));
        next.totals.input_records += 1;
        next.previous = Some(format!("sha256:{}", "0".repeat(64)));
        assert!(!next.raises(&old, &next.issued_attempt));
        next.previous = Some(old.digest());
        next.totals.work = old.ceilings.work + 1;
        assert!(!next.valid());
    }
    #[test]
    fn raising_another_total_does_not_mint_work() {
        let old = budget();
        let mut next = old.clone();
        next.issued_attempt = "00000000-0000-0000-0000-000000000003".into();
        next.previous = Some(old.digest());
        // Give the original receipt duration headroom within the same active policy.
        let mut old = old;
        old.totals.duration_ms /= 2;
        next.previous = Some(old.digest());
        next.totals.duration_ms = old.totals.duration_ms + 1;
        assert!(next.raises(&old, &next.issued_attempt));
        assert_eq!(
            next.allowance_after(&old, 700, &next.issued_attempt),
            Some(700)
        );
    }
}
