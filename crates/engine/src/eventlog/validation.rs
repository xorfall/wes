//! Joined bounded validation, with one cumulative work allowance per writer lifetime.
use super::*;
pub(super) struct Batch {
    pub items: Vec<archive::Item>,
    pub rows: Vec<DatasetRow>,
    pub policy: FlowPolicy,
    pub charged_bytes: u64,
    pub charged_work: u64,
    pub failure: Option<RecordingEnd>,
    pub restricted: bool,
}
pub(super) fn batch(
    items: Vec<archive::Item>,
    schema: ResolvedContractBundle,
    current: Status,
    policy: FlowPolicy,
    limits: Limits,
) -> Batch {
    let mut result = Batch {
        items,
        rows: vec![],
        policy,
        charged_bytes: current.charged_bytes,
        charged_work: current.charged_work,
        failure: None,
        restricted: false,
    };
    for item in &result.items {
        let policy = item.value.provenance().policy();
        if policy.is_private() || policy.is_unknown() {
            result.failure = Some(RecordingEnd::Rejected);
            result.restricted = true;
            break;
        }
        // Admission already computed the conservative value charge while reserving ingress.
        // Prepay traversal/contract/encoding work before touching the retained tree again.
        let work = item
            .charge
            .checked_mul(64)
            .and_then(|n| n.checked_add((schema.encoded().len() as u64).saturating_mul(32)))
            .and_then(|n| n.checked_add(4096));
        let Some(charged) = work
            .and_then(|n| result.charged_work.checked_add(n))
            .filter(|n| *n <= limits.work)
        else {
            result.failure = Some(RecordingEnd::Limit);
            break;
        };
        result.charged_work = charged;
        if current
            .reference
            .records()
            .saturating_add(result.rows.len() as u64)
            >= limits.records
            || result
                .charged_bytes
                .checked_add(item.charge)
                .is_none_or(|n| n > limits.bytes)
        {
            result.failure = Some(RecordingEnd::Limit);
            break;
        }
        let expected = current
            .coverage
            .committed_through
            .checked_add(result.rows.len() as u64 + 1);
        if expected != Some(item.sequence)
            || !item.value.data().is_inline()
            || !item.value.shape().is_inline()
            || item.value.shape().contains_meta()
            || item.value.management_authority().is_some()
            || !item.value.shape().is_assignable_to(&schema.root().shape())
            || !schema
                .root()
                .issues_with_budget(item.value.data(), &|| false, &mut 65_536)
                .is_ok_and(|issues| issues.is_empty())
        {
            result.failure = Some(RecordingEnd::Rejected);
            break;
        }
        result.charged_bytes += item.charge;
        result.policy = result.policy.join(policy);
        result.rows.push(DatasetRow {
            ordinal: current.reference.records() + result.rows.len() as u64,
            source_start: item.sequence - 1,
            source_end: item.sequence,
            value: item.value.clone(),
        });
    }
    result
}
