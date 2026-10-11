//! Explicit interaction inputs. Selection state remains in the presentation; an
//! activation identifies the exact source version from which arguments are captured.
use crate::{
    graph::{NodeId, OutputRef},
    runtime::RunId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Rendered {
        source: NodeId,
        run: RunId,
        view: String,
        artifact: String,
        value: wes_core::Data,
    },
    Row {
        source: NodeId,
        run: RunId,
        row: usize,
    },
    View {
        source: NodeId,
        identity: String,
        revision: u64,
        port: String,
    },
}
impl Selection {
    pub fn source(&self) -> &NodeId {
        match self {
            Self::Row { source, .. }
            | Self::View { source, .. }
            | Self::Rendered { source, .. } => source,
        }
    }
    pub fn output(&self) -> OutputRef {
        OutputRef::data(self.source().clone())
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Rendered {
                view,
                artifact,
                value,
                ..
            } if view.len() > 256
                || artifact.len() > 256
                || !value.is_inline()
                || wes_core::Value::new(
                    wes_core::Shape::Unknown,
                    value.clone(),
                    wes_core::Provenance::default(),
                )
                .ok()
                .and_then(|v| crate::value_size::value_charge(&v, 128 * 1024))
                .is_none() =>
            {
                Err("Invalid rendered selection")
            }
            Self::Row { row, .. } if *row > 1_000_000 => Err("Selection row exceeds its bound"),
            Self::View { identity, port, .. }
                if identity.is_empty()
                    || identity.len() > 256
                    || identity.chars().any(char::is_control)
                    || !matches!(port.as_str(), "selected" | "selection") =>
            {
                Err("Invalid selection output")
            }
            _ => Ok(()),
        }
    }
}

/// Run evidence, never a command edit or an instruction to resume work. Private
/// inputs are deliberately absent from the ordinary public history journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationRecord {
    pub node: NodeId,
    pub run: RunId,
    pub cell: String,
    pub request: String,
    pub arguments: Option<wes_core::Value>,
}
impl InvocationRecord {
    pub fn validate(&self) -> bool {
        self.node.as_str().len() <= 256
            && self.run.as_str().len() <= 256
            && [&self.cell, &self.request]
                .iter()
                .all(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            && self.arguments.as_ref().is_none_or(|v| {
                matches!(v.data(), wes_core::Data::Record(_))
                    && v.data().is_inline()
                    && !v.provenance().policy().is_private()
                    && !v.provenance().policy().is_unknown()
                    && crate::value_size::value_charge(v, 128 * 1024).is_some()
            })
    }
}

/// Inline renderers belong to a nominal input contract, never to an arbitrary record.
pub(crate) fn rendered_package<'a>(
    shape: &wes_core::Shape,
    views: &'a wes_views::Catalogue,
) -> Option<&'a wes_views::Package> {
    let wes_core::Shape::Record(record) = shape else {
        return None;
    };
    if record.fields().any(|(name, _)| name == "selected") {
        return None;
    }
    let mut candidates = views
        .values()
        .filter(|package| package.manifest.input == record.name());
    let package = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    selection_contract(package)?;
    Some(package)
}
pub(crate) fn selection_contract(
    package: &wes_views::Package,
) -> Option<std::sync::Arc<wes_core::contracts::Contract>> {
    let port = package
        .manifest
        .outputs
        .get("selected")
        .or_else(|| package.manifest.outputs.get("selection"))?;
    if !port.shared || port.mode != wes_views::Mode::State {
        return None;
    }
    package.contracts.resolve(&port.r#type).ok()
}
pub(crate) fn selected_shape(package: &wes_views::Package) -> Option<wes_core::Shape> {
    Some(match selection_contract(package)?.shape() {
        wes_core::Shape::Option(inner) => *inner,
        shape => shape,
    })
}
