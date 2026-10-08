//! Unpublished bounded candidates. Only a store acknowledgement advances public
//! state, source position, output counters or input-earned work allowance.
use super::*;
impl Runner {
    pub(super) fn working_state(&self) -> &Value {
        self.candidate.as_ref().map_or(&self.state, |c| &c.state)
    }
    pub(super) fn working_state_charge(&self) -> u64 {
        self.candidate
            .as_ref()
            .map_or(self.state_charge, |c| c.state_charge)
    }
    pub(super) fn working_provenance(&self) -> &Provenance {
        self.candidate
            .as_ref()
            .map_or(&self.provenance, |c| &c.provenance)
    }
    pub(super) fn held_state_charge(&self) -> u64 {
        self.state_charge
            .saturating_add(self.candidate.as_ref().map_or(0, |c| c.state_charge))
    }
    pub(super) fn working_usage(&self) -> Usage {
        self.candidate
            .as_ref()
            .map_or(self.ledger.usage(), |c| c.usage)
    }
    pub(super) fn candidate_ready(&self) -> bool {
        self.candidate.as_ref().is_some_and(|c| {
            c.flush
                || c.terminal
                || c.inputs >= self.settings.commit_records
                || c.outputs.len() >= self.settings.commit_records
                || c.output_charge >= self.settings.commit_bytes
                || self.ledger.prepaid_remaining() <= self.settings.scratch.work
        })
    }
    pub(super) fn stage_batch(&mut self, next: Candidate) -> Result<(), Stop> {
        let Some(mut batch) = self.candidate.take() else {
            self.candidate = Some(next);
            return Ok(());
        };
        // Each record retains its original range; coalescing the checkpoint
        // boundary must never make every output claim the whole batch's span.
        batch.range = match (batch.range, next.range) {
            (Some((start, _, charged)), Some((_, end, amount))) => Some((
                start,
                end,
                charged.checked_add(amount).ok_or_else(|| {
                    stop(Failure::new(
                        "CAL006",
                        self.span,
                        "scan batch input charge overflow",
                    ))
                })?,
            )),
            (prior, None) => prior,
            (None, range) => range,
        };
        batch.output_charge = batch
            .output_charge
            .checked_add(next.output_charge)
            .ok_or_else(|| {
                stop(Failure::new(
                    "CAL006",
                    self.span,
                    "scan batch output charge overflow",
                ))
            })?;
        batch.inputs += next.inputs;
        batch.state = next.state;
        batch.state_charge = next.state_charge;
        batch.outputs.extend(next.outputs);
        batch.coverage.extend(next.coverage);
        batch.output_ranges.extend(next.output_ranges);
        batch.provenance = Provenance::agreed_by([&batch.provenance, &next.provenance]);
        batch.usage = next.usage;
        batch.held_after = next.held_after;
        batch.finishing = next.finishing;
        batch.terminal = next.terminal;
        batch.flush |= next.flush;
        self.candidate = Some(batch);
        Ok(())
    }
    pub(super) fn acknowledge_batch(&mut self, batch: Candidate) -> Result<(), Stop> {
        self.ledger.acknowledge_usage(batch.usage);
        self.state = batch.state;
        self.state_charge = batch.state_charge;
        self.provenance = batch.provenance;
        if let Some((_, end, charge)) = batch.range {
            self.committed_position = end;
            self.ledger
                .earn(charge.saturating_mul(self.settings.work_per_input_unit));
        }
        self.finish_applied = batch.finishing;
        self.phase = if batch.terminal {
            Phase::Complete
        } else {
            Phase::Reading
        };
        self.ledger
            .held(
                self.base_charge
                    .saturating_add(self.state_charge)
                    .saturating_add(if self.pending_source.is_some() {
                        self.settings.record_charge
                    } else {
                        0
                    }),
            )
            .map_err(|e| refusal(e, self.span))?;
        Ok(())
    }
}
