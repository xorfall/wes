//! Import planning is inert owner work; content capture belongs to explicit apply admission.
use crate::{
    graph::OutputRef,
    plan::{Input, MetaTask},
    runtime::{Outcome, RuntimeCode},
};
use wes_core::{Shape, capability::Parameter};
#[derive(Clone, Debug)]
pub struct BoundImportPlan {
    pub(crate) task: MetaTask,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) environment: Option<wes_core::environments::EnvironmentContext>,
    pub(crate) captured: Option<Outcome>,
}
impl BoundImportPlan {
    pub(crate) fn bind(
        task: MetaTask,
        parameters: Vec<Parameter>,
        environment: Option<wes_core::environments::EnvironmentContext>,
    ) -> Result<Self, &'static str> {
        if task.tail.len() != 1
            || !task.subjects.is_empty()
            || task
                .inputs
                .values()
                .any(|v| matches!(v, Input::Record(_) | Input::List { .. }))
        {
            return Err(
                "Import plan requires one importer and scalar arguments; referenced scalar fields are supported.",
            );
        }
        if task
            .inputs
            .keys()
            .any(|k| k != "as" && !parameters.iter().any(|p| &p.name == k))
        {
            return Err("Import plan contains an unsupported argument.");
        }
        Ok(Self {
            task,
            parameters,
            environment,
            captured: None,
        })
    }
    pub(crate) fn dependencies(&self) -> impl Iterator<Item = OutputRef> + '_ {
        self.task
            .inputs
            .values()
            .flat_map(Input::dependencies)
            .cloned()
    }
    pub(crate) fn expected(&self, key: &str) -> Shape {
        if key == "as" {
            Shape::Primitive(wes_core::Primitive::Text)
        } else {
            self.parameters
                .iter()
                .find(|p| p.name == key)
                .map_or(Shape::Unknown, |p| p.shape.clone())
        }
    }
    pub(crate) fn outcome(self) -> Outcome {
        self.captured.unwrap_or_else(|| {
            Outcome::Failed(
                RuntimeCode::ExecutionFailed
                    .error("Import planning requires a live authorized session.", None),
            )
        })
    }
}
