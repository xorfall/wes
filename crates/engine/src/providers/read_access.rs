//! One captured read-domain attempt at a provider boundary. Read probes cannot
//! be inferred from a result, retried, or run by a cooperative principal.
use super::*;
pub(super) enum Attempt {
    None,
    Read {
        permit: crate::environments::ReadPermit,
        origin: wes_core::flow::ProviderReadOrigin,
    },
    Probe(Option<crate::environments::ReadProbe>),
}
pub(super) fn refusal() -> InvocationError {
    InvocationError::Failed(RuntimeCode::ResultForbidden.error("Provider read scope is unavailable or changed; explicitly run its captured revalidation operation. Nothing was replayed.",None))
}
impl BoundCall {
    pub(super) fn read_access(
        &self,
        arguments: &IndexMap<String, Value>,
    ) -> Result<Attempt, InvocationError> {
        let Some(domain) = &self.provider.read_domain else {
            return Ok(Attempt::None);
        };
        let ExecutionScope::Environment {
            binding,
            authority: Some(authority),
        } = &self.provider.scope
        else {
            return Err(refusal());
        };
        let digest = domain.digest(arguments).map_err(|_| refusal())?;
        let alias = self.provider.description.name();
        if self.invocation.capability.path == domain.revalidate {
            if self.invocation.interactive || self.streaming() {
                return Err(refusal());
            }
            Ok(Attempt::Probe(Some(
                authority
                    .begin_read_probe(binding, alias, &digest, &self.context.authority)
                    .map_err(|_| refusal())?,
            )))
        } else {
            let origin = authority
                .provider_read_origin(binding, alias, &digest)
                .map_err(|_| refusal())?;
            let permit = authority.capture_read_permit(&origin).ok_or_else(refusal)?;
            Ok(Attempt::Read { permit, origin })
        }
    }
}
impl Attempt {
    pub(super) fn is_probe(&self) -> bool {
        matches!(self, Self::Probe(_))
    }
    pub(super) fn protect(&self, provenance: Provenance) -> Provenance {
        match self {
            Self::Read { origin, .. } => provenance.clone().with_policy(
                &provenance
                    .policy()
                    .clone()
                    .with_provider_read(origin.clone()),
            ),
            _ => provenance,
        }
    }
    pub(super) fn current(&self) -> bool {
        match self {
            Self::Read { permit, .. } => permit.current(),
            Self::Probe(Some(probe)) => probe.current(),
            Self::None => true,
            Self::Probe(None) => false,
        }
    }
    pub(super) fn finish(&mut self, value: &Value, cancelled: bool) -> Result<(), InvocationError> {
        if let Self::Probe(probe) = self {
            if cancelled {
                return Err(InvocationError::Cancelled);
            }
            if value.shape() != &Shape::Primitive(wes_core::Primitive::Bool) {
                return Err(refusal());
            }
            match value.data() {
                wes_core::Data::Bool(true) => probe
                    .take()
                    .ok_or_else(refusal)?
                    .confirm()
                    .map_err(|_| refusal()),
                wes_core::Data::Bool(false) => {
                    probe.take();
                    Ok(())
                }
                _ => Err(refusal()),
            }
        } else if self.current() {
            Ok(())
        } else {
            Err(refusal())
        }
    }
    pub(super) fn stream_guard(&self) -> Option<crate::provider_reads::ReadGuard> {
        match self {
            Self::Read { permit, .. } => {
                let permit = permit.clone();
                Some(crate::provider_reads::ReadGuard::new(move || {
                    permit.current()
                }))
            }
            _ => None,
        }
    }
}
