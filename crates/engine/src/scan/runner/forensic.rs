//! Callback-free malformed-frame candidates share the same acknowledgement boundary.
use super::*;
use crate::storage::datasets::CoverageProgress;
use wes_core::framing::Rejection;

impl Runner {
    pub(super) fn validate_source_boundary(
        &self,
        ordinal: u64,
        start: u64,
        end: u64,
    ) -> Result<(), Stop> {
        let boundary = self
            .candidate
            .as_ref()
            .and_then(|c| c.range)
            .map_or(self.committed_position, |(_, end, _)| end);
        if ordinal != self.working_usage().input_records
            || start != boundary
            || end < start
            || end > self.source.total()
        {
            return Err(stop(Failure::new(
                "CAL004",
                self.span,
                "source frame does not match its captured acknowledgement boundary",
            )));
        }
        Ok(())
    }
    pub(super) fn coverage_for_candidate(
        &self,
        candidate: &Candidate,
    ) -> Result<Option<CoverageProgress>, Failure> {
        let mut coverage = self.coverage.clone();
        if !candidate.coverage.is_empty() {
            let progress = coverage.as_mut().ok_or_else(|| {
                self.durable_failure("strict analysis cannot publish rejected-frame evidence")
            })?;
            let position = candidate
                .range
                .map_or(self.committed_position, |(_, end, _)| end);
            for row in &candidate.coverage {
                progress
                    .observe(row, candidate.usage.input_records, position)
                    .map_err(|_| {
                        self.durable_failure(
                            "rejected-frame evidence does not match its acknowledged source",
                        )
                    })?;
            }
            if !progress.valid(
                candidate.usage.input_records,
                position,
                candidate.usage.input_bytes,
            ) {
                return Err(
                    self.durable_failure("rejected-frame counters exceed acknowledged input")
                );
            }
        }
        Ok(coverage)
    }
    pub(super) fn accept_rejection(&mut self, row: Rejection) -> Result<(), Stop> {
        self.validate_source_boundary(row.ordinal, row.source.start, row.delimiter.end)?;
        if row.unterminated && row.delimiter.end != self.source.total() {
            return Err(stop(self.durable_failure(
                "unterminated rejection precedes the captured extent end",
            )));
        }
        let input_charge = row.delimiter.end - row.source.start;
        let end = row.delimiter.end;
        // Charge the retained native evidence (including bounded attribution/record wrappers),
        // independently of ordinary output rows. This also covers its unpublished copies.
        let output_charge = crate::storage::datasets::rejection_charge(&row)
            .ok_or_else(|| stop(self.durable_failure("rejected-frame charge overflow")))?;
        self.ledger
            .admit_input_from(self.working_usage(), input_charge)
            .map_err(|e| refusal(e, self.span))?;
        self.ledger
            .work(output_charge)
            .map_err(|e| refusal(e, self.span))?;
        let held_after = sum(
            &[
                self.base_charge,
                self.held_state_charge(),
                self.retained_output_charge(),
                output_charge,
            ],
            self.span,
        )
        .map_err(stop)?;
        let usage = self
            .ledger
            .candidate_usage_from(
                self.working_usage(),
                Some(input_charge),
                0,
                output_charge,
                held_after,
            )
            .map_err(|e| refusal(e, self.span))?;
        let candidate = Candidate {
            state: self.working_state().clone(),
            state_charge: self.working_state_charge(),
            outputs: vec![],
            coverage: vec![row],
            output_charge,
            held_after,
            provenance: self.working_provenance().clone(),
            range: Some((
                self.candidate
                    .as_ref()
                    .and_then(|c| c.range)
                    .map_or(self.committed_position, |(_, end, _)| end),
                end,
                input_charge,
            )),
            finishing: false,
            terminal: false,
            usage,
            inputs: 1,
            output_ranges: vec![],
            flush: false,
        };
        self.coverage_for_candidate(&candidate).map_err(stop)?;
        self.stage_batch(candidate)
    }
}
