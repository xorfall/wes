use super::*;

impl Input {
    pub fn selection_sources(&self) -> Vec<&OutputRef> {
        match self {
            Self::Selection { source, .. } => vec![source],
            Self::Record(fields) => fields.values().flat_map(Self::selection_sources).collect(),
            Self::List { items, .. } => items.iter().flat_map(Self::selection_sources).collect(),
            _ => vec![],
        }
    }

    /// Capture all arguments at admission. A later selection or graph publication cannot
    /// substitute another value while the invocation waits for its predecessor to exit.
    pub(crate) fn capture_selection(
        &self,
        source: &OutputRef,
        selected: &Value,
        inputs: &IndexMap<NodeId, Value>,
    ) -> Result<Self, InputResolutionError> {
        match self {
            Self::Selection {
                source: expected,
                fields,
                ..
            } => {
                if source != expected {
                    return Err(InputResolutionError::unavailable(expected, fields));
                }
                project_value(selected, fields).map(Self::Literal)
            }
            Self::Record(fields) => Ok(Self::Record(
                fields
                    .iter()
                    .map(|(key, input)| {
                        Ok((
                            key.clone(),
                            input.capture_selection(source, selected, inputs)?,
                        ))
                    })
                    .collect::<Result<_, InputResolutionError>>()?,
            )),
            Self::List { items, item_shape } => Ok(Self::List {
                items: items
                    .iter()
                    .map(|input| input.capture_selection(source, selected, inputs))
                    .collect::<Result<_, _>>()?,
                item_shape: item_shape.clone(),
            }),
            _ => self
                .resolve(inputs)
                .map(|value| Self::Literal(value.into_owned())),
        }
    }
}
