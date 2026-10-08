//! Requested cumulative totals share one literal parser and ceiling check.
//! Per-record semantics, held memory and earning rates are not adjustable totals.
use super::Settings;
use crate::plan::{Input, MetaTask};
use std::time::Duration;
use wes_core::Data;
use wes_language::{Diagnostic, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    pub work: u64,
    pub input_bytes: u64,
    pub input_records: u64,
    pub output_bytes: u64,
    pub output_records: u64,
    pub duration_ms: u64,
}
impl Totals {
    pub fn valid(self) -> bool {
        self.values()
            .into_iter()
            .all(|n| n > 0 && n <= i64::MAX as u64)
            && self.work <= 1_000_000_000
            && self.duration_ms <= 86_400_000
    }
    pub fn within(self, ceiling: Self) -> bool {
        self.valid()
            && ceiling.valid()
            && self
                .values()
                .into_iter()
                .zip(ceiling.values())
                .all(|(n, cap)| n > 0 && n <= cap)
    }
    pub fn contains(self, prior: Self) -> bool {
        prior.within(self)
    }
    pub fn values(self) -> [u64; 6] {
        [
            self.work,
            self.input_bytes,
            self.input_records,
            self.output_bytes,
            self.output_records,
            self.duration_ms,
        ]
    }
    pub(crate) fn apply(self, mut settings: Settings) -> Settings {
        settings.limits.work = self.work;
        settings.limits.input_bytes = self.input_bytes;
        settings.limits.input_records = self.input_records;
        settings.limits.output_bytes = self.output_bytes;
        settings.limits.output_records = self.output_records;
        settings.duration = Duration::from_millis(self.duration_ms);
        settings
    }
}
impl Settings {
    pub fn totals(self) -> Totals {
        Totals {
            work: self.limits.work,
            input_bytes: self.limits.input_bytes,
            input_records: self.limits.input_records,
            output_bytes: self.limits.output_bytes,
            output_records: self.limits.output_records,
            duration_ms: self.duration.as_millis() as u64,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RequestedBounds([Option<u64>; 6]);
impl RequestedBounds {
    pub(crate) const NAMES: [&'static str; 6] =
        ["work", "input", "records", "output", "outputs", "duration"];
    pub(crate) fn parse(task: &MetaTask, span: Span) -> Result<Self, Diagnostic> {
        let mut result = Self::default();
        for (index, name) in Self::NAMES.into_iter().enumerate() {
            let Some(input) = task.inputs.get(name) else {
                continue;
            };
            let Some(n) = (match input {
                Input::Literal(value) => match value.data() {
                    Data::Int(n) if *n > 0 => Some(*n as u64),
                    _ => None,
                },
                _ => None,
            }) else {
                return Err(Diagnostic::error(
                    "CAL009",
                    span,
                    format!(
                        "scan {name}: must be a literal positive integer; totals cannot change reactively"
                    ),
                ));
            };
            result.0[index] = Some(n);
        }
        Ok(result)
    }
    pub(crate) fn requested(self, baseline: Totals) -> Totals {
        let prior = baseline.values();
        let n: [u64; 6] = std::array::from_fn(|i| self.0[i].unwrap_or(prior[i]));
        Totals {
            work: n[0],
            input_bytes: n[1],
            input_records: n[2],
            output_bytes: n[3],
            output_records: n[4],
            duration_ms: n[5],
        }
    }
    pub(crate) fn resolve(
        self,
        baseline: Totals,
        ceiling: Totals,
        span: Span,
    ) -> Result<Totals, Diagnostic> {
        let resolved = self.requested(baseline);
        let values = resolved.values();
        if !resolved.within(ceiling) {
            let index = values
                .into_iter()
                .zip(ceiling.values())
                .position(|(n, cap)| n == 0 || n > cap)
                .expect("invalid total");
            return Err(Diagnostic::error(
                "CAL009",
                span,
                format!(
                    "scan {}: exceeds the active cumulative ceiling ({})",
                    Self::NAMES[index],
                    ceiling.values()[index]
                ),
            ));
        }
        Ok(resolved)
    }
    pub(crate) fn fresh(self, ceiling: Settings, span: Span) -> Result<Settings, Diagnostic> {
        let mut settings = self
            .resolve(ceiling.totals(), ceiling.totals(), span)?
            .apply(ceiling);
        // Only initial admission clips startup credit. Continuing never recaptures it.
        settings.startup_work = settings.startup_work.min(settings.limits.work);
        if !settings.valid() {
            return Err(Diagnostic::error(
                "CAL009",
                span,
                "requested scan totals do not form a valid captured budget",
            ));
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn span() -> Span {
        Span::new(0, 0).unwrap()
    }
    #[test]
    fn total_arguments_accept_only_positive_literal_integers() {
        for name in RequestedBounds::NAMES {
            for data in [
                Data::Int(0),
                Data::Int(-1),
                Data::Text("123".into()),
                Data::Bool(true),
            ] {
                let task = MetaTask {
                    spec: wes_language::vocabulary::MetaCommand::Scan.spec(&[]),
                    target: None,
                    tail: vec![],
                    subjects: vec![],
                    inputs: [(
                        name.into(),
                        Input::Literal(
                            wes_core::Value::new(
                                wes_core::Shape::Unknown,
                                data,
                                Default::default(),
                            )
                            .unwrap(),
                        ),
                    )]
                    .into(),
                };
                assert!(RequestedBounds::parse(&task, span()).is_err(), "{name}");
            }
        }
    }
    #[test]
    fn requested_totals_preserve_frozen_knobs_and_independent_dimensions() {
        let ceiling = Settings::capture();
        let mut requested = RequestedBounds::default();
        requested.0[0] = Some(1_000_000.min(ceiling.limits.work));
        requested.0[2] = Some(2);
        requested.0[4] = Some(1);
        let result = requested.fresh(ceiling, span()).unwrap();
        assert_eq!(result.limits.work, 1_000_000.min(ceiling.limits.work));
        assert_eq!(result.limits.input_records, 2);
        assert_eq!(result.limits.output_records, 1);
        assert_eq!(result.limits.input_bytes, ceiling.limits.input_bytes);
        assert_eq!(result.limits.output_bytes, ceiling.limits.output_bytes);
        assert_eq!(result.duration, ceiling.duration);
        assert_eq!(result.limits.memory_bytes, ceiling.limits.memory_bytes);
        assert_eq!(result.record_charge, ceiling.record_charge);
        assert_eq!(result.scratch.work, ceiling.scratch.work);
        assert_eq!(result.work_per_input_unit, ceiling.work_per_input_unit);
        assert_eq!(
            result.startup_work,
            ceiling.startup_work.min(result.limits.work)
        );
        assert_eq!(result.outputs_per_record, ceiling.outputs_per_record);
    }
    #[test]
    fn every_total_refuses_zero_and_above_ceiling_without_partial_admission() {
        let ceiling = Settings::capture();
        for index in 0..6 {
            for n in [0, ceiling.totals().values()[index] + 1] {
                let mut request = RequestedBounds::default();
                request.0[index] = Some(n);
                let error = request.fresh(ceiling, span()).unwrap_err();
                assert_eq!(error.code, "CAL009");
                assert!(error.message.contains(RequestedBounds::NAMES[index]));
            }
        }
        assert_eq!(
            RequestedBounds::default()
                .fresh(ceiling, span())
                .unwrap()
                .totals(),
            ceiling.totals()
        );
    }
    #[test]
    fn unchanged_means_prior_totals_and_lowering_is_not_a_continuation() {
        let ceiling = Settings::capture().totals();
        let mut prior = ceiling;
        prior.work /= 2;
        let unchanged = RequestedBounds::default()
            .resolve(prior, ceiling, span())
            .unwrap();
        assert_eq!(unchanged, prior);
        let mut request = RequestedBounds::default();
        request.0[0] = Some(prior.work + 1);
        let raised = request.resolve(prior, ceiling, span()).unwrap();
        assert!(raised.contains(prior));
        assert!(!prior.contains(raised));
        assert_eq!(raised.input_bytes, prior.input_bytes);
        assert_eq!(raised.duration_ms, prior.duration_ms);
    }
}
