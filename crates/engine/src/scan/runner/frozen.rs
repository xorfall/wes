//! Frozen invocation, framing and scheduling knobs; totals belong to a budget receipt.
use super::Limits;
use super::{Settings, calc};
use crate::scan::Totals;
use std::time::Duration;
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenSettings {
    pub memory_bytes: u64,
    pub startup_work: u64,
    pub work_per_input_unit: u64,
    pub source_charge: u64,
    pub state_charge: u64,
    pub context_charge: u64,
    pub record_charge: u64,
    pub scratch: calc::Limits,
    pub outputs_per_record: u64,
    pub patterns: usize,
    pub block: usize,
    pub page_rows: usize,
    pub page_bytes: usize,
    pub page_segments: usize,
    pub commit_records: usize,
    pub commit_bytes: u64,
}
impl FrozenSettings {
    pub fn capture(settings: Settings) -> Self {
        Self {
            memory_bytes: settings.limits.memory_bytes,
            startup_work: settings.startup_work,
            work_per_input_unit: settings.work_per_input_unit,
            source_charge: settings.source_charge,
            state_charge: settings.state_charge,
            context_charge: settings.context_charge,
            record_charge: settings.record_charge,
            scratch: settings.scratch,
            outputs_per_record: settings.outputs_per_record,
            patterns: settings.patterns,
            block: settings.block,
            page_rows: settings.page_rows,
            page_bytes: settings.page_bytes,
            page_segments: settings.page_segments,
            commit_records: settings.commit_records,
            commit_bytes: settings.commit_bytes,
        }
    }
    pub fn restore(self, totals: Totals) -> Settings {
        Settings {
            limits: Limits {
                work: totals.work,
                input_bytes: totals.input_bytes,
                input_records: totals.input_records,
                memory_bytes: self.memory_bytes,
                output_bytes: totals.output_bytes,
                output_records: totals.output_records,
            },
            duration: Duration::from_millis(totals.duration_ms),
            startup_work: self.startup_work,
            work_per_input_unit: self.work_per_input_unit,
            source_charge: self.source_charge,
            state_charge: self.state_charge,
            context_charge: self.context_charge,
            record_charge: self.record_charge,
            scratch: self.scratch,
            outputs_per_record: self.outputs_per_record,
            patterns: self.patterns,
            block: self.block,
            page_rows: self.page_rows,
            page_bytes: self.page_bytes,
            page_segments: self.page_segments,
            commit_records: self.commit_records,
            commit_bytes: self.commit_bytes,
        }
    }
}
