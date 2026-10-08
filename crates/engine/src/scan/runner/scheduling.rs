//! Bounded native yields preserve RAM carry without advancing a committed record boundary.
use super::*;

impl Runner {
    pub(super) fn needs_handoff_grant(&self, item: &SourcePoll) -> Result<bool, Stop> {
        if self.durable.is_none() {
            return Ok(false);
        }
        let needed = match item {
            SourcePoll::Rejected(row) => crate::storage::datasets::rejection_charge(row)
                .ok_or_else(|| stop(self.durable_failure("rejected-frame charge overflow")))?,
            SourcePoll::Record { value, .. } => self.invocation_reservation(value)?,
            SourcePoll::End if self.finish.is_some() => {
                self.invocation_reservation(&source_end(
                    self.source.position(),
                    self.source.framed(),
                    self.working_provenance().clone(),
                    self.source.producer_complete(),
                ))?
            }
            _ => return Ok(false),
        };
        // Reserving a full invocation prevents a lease boundary inside the VM. An actually
        // smaller earned remainder may still run: only real work debits can exhaust it.
        Ok((self.candidate.is_some()
            && self.ledger.work_allowance() < self.settings.limits.work
            && self.ledger.remaining_work() < needed)
            || self.ledger.prepaid_remaining() < needed.min(self.ledger.remaining_work()))
    }
    fn invocation_reservation(&self, item: &Value) -> Result<u64, Stop> {
        sum(
            &[
                charge(item, self.settings.record_charge, self.span).map_err(stop)?,
                self.working_state_charge(),
                1024,
                self.settings.scratch.work,
                self.settings.scratch.bytes,
                1024,
            ],
            self.span,
        )
        .map_err(stop)
    }
    pub(super) fn request_renewal(&mut self) -> Result<Poll, Stop> {
        self.phase = Phase::Committing;
        if let Some(candidate) = &mut self.candidate {
            candidate.flush = true;
            Ok(Poll::Commit)
        } else {
            if self
                .durable
                .as_ref()
                .is_none_or(|d| self.ledger.usage().work <= d.grant_start_work.saturating_add(4096))
            {
                return Err(stop(self.durable_failure(
                    "fresh durable work lease cannot make progress under its captured bounds",
                )));
            }
            self.renewal = true;
            Ok(Poll::Settle)
        }
    }
}

impl Runner {
    /// Called only after a source read is physically joined. No storage error can retry an effect.
    pub fn refuse_source_read(&mut self, error: crate::storage::StoreError) {
        use crate::storage::{StoreError, datasets::ReadWorkDimension};
        let refused = match error {
            StoreError::ReadWork(refused) => refused,
            StoreError::DatasetRowCharge { limit } => {
                self.fail(refusal(
                    Refusal {
                        dimension: Dimension::RecordMemory,
                        limit,
                    },
                    self.span,
                ));
                return;
            }
            StoreError::DatasetRowBytes { limit } => {
                self.fail(refusal(
                    Refusal {
                        dimension: Dimension::SourcePageBytes,
                        limit,
                    },
                    self.span,
                ));
                return;
            }
            error => {
                self.refuse_scan(Failure::new("CAL004", self.span, error.to_string()));
                return;
            }
        };
        if self.candidate.is_some() || self.invocation.is_some() {
            self.refuse_scan(
                self.durable_failure("source read refused outside its owned read boundary"),
            );
            return;
        }
        let result = match refused.dimension() {
            ReadWorkDimension::Prepaid if self.durable.is_some() => {
                let needed = refused.needed().checked_add(4096).unwrap_or(u64::MAX);
                if needed <= self.read_work_demand {
                    Err(stop(self.durable_failure(
                        "source read renewal did not reveal a larger work demand",
                    )))
                } else {
                    self.read_work_demand = needed;
                    self.phase = Phase::Committing;
                    self.renewal = true;
                    Ok(Poll::Settle)
                }
            }
            ReadWorkDimension::Prepaid => Err(stop(
                self.durable_failure("a source read has no durable prepaid owner"),
            )),
            cause => Err(refusal(
                Refusal {
                    dimension: if cause == ReadWorkDimension::Allowance {
                        Dimension::WorkAllowance
                    } else {
                        Dimension::Work
                    },
                    limit: if cause == ReadWorkDimension::Allowance {
                        self.ledger.work_allowance()
                    } else {
                        self.settings.limits.work
                    },
                },
                self.span,
            )),
        };
        if let Err(stopped) = result {
            self.fail(stopped);
        }
    }
}
