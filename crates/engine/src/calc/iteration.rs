use super::{Failure, value::Item};
use crate::iteration::SourceCursor;
use std::sync::Arc;
use wes_core::{IterStage, IterValue, contracts::Contract};
use wes_language::Span;

#[derive(Clone, Debug)]
pub(super) enum Stage {
    Data(IterStage),
    Map(Item),
    Filter(Item),
}
#[derive(Clone, Debug)]
pub(super) struct Pipeline {
    pub root: Arc<IterValue>,
    pub stages: Vec<Stage>,
}
impl Pipeline {
    pub fn from_value(value: Arc<IterValue>) -> Self {
        let stages = value.stages().iter().cloned().map(Stage::Data).collect();
        Self {
            root: value,
            stages,
        }
    }
    pub fn push(&self, stage: Stage, span: Span) -> Result<Self, Failure> {
        if self.stages.len() >= 64 {
            return Err(Failure::new(
                "CAL006",
                span,
                "Iter pipeline exceeds 64 stages",
            ));
        }
        let mut next = self.clone();
        next.stages.push(stage);
        Ok(next)
    }
    pub fn stored(&self, span: Span) -> Result<Arc<IterValue>, Failure> {
        let stages = self
            .stages
            .iter()
            .map(|stage| match stage {
                Stage::Data(s) => Ok(s.clone()),
                _ => Err(Failure::new(
                    "CAL004",
                    span,
                    "lazy callback pipelines must be collected before a value boundary",
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        IterValue::new(
            self.root.source().clone(),
            self.root.mode(),
            self.root.argument().map(str::to_owned),
            stages,
        )
        .map(Arc::new)
        .map_err(|e| Failure::iteration(e, span))
    }
}
pub(super) struct Cursor {
    pub source: SourceCursor,
    pub source_shape: Arc<wes_core::Shape>,
    pub pipeline: Arc<Pipeline>,
    pub counts: Vec<u64>,
    pub checks: Vec<Option<Arc<Contract>>>,
    pub index: u64,
    pub offset: Option<usize>,
}
impl Cursor {
    pub fn new(
        pipeline: Arc<Pipeline>,
        span: Span,
        cache: &mut wes_core::IterRegexCache,
    ) -> Result<Self, Failure> {
        let checks = pipeline
            .stages
            .iter()
            .map(|stage| match stage {
                Stage::Data(IterStage::Check(c)) => c.resolve().map(Some).map_err(|e| {
                    Failure::categorized(
                        wes_language::calc::diagnostics::Category::Contract,
                        span,
                        e,
                    )
                }),
                _ => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            source: SourceCursor::new_cached(pipeline.root.clone(), cache)?,
            source_shape: Arc::new(pipeline.root.source_item_shape().clone()),
            counts: vec![0; pipeline.stages.len()],
            pipeline,
            checks,
            index: 0,
            offset: None,
        })
    }
    pub fn output_shape(&self) -> wes_core::Shape {
        use wes_core::Shape;
        self.pipeline.stages.iter().enumerate().fold(
            self.pipeline.root.source_item_shape().clone(),
            |shape, (index, stage)| match stage {
                Stage::Data(IterStage::Check(_)) => {
                    self.checks[index].as_ref().expect("resolved check").shape()
                }
                Stage::Data(IterStage::Field(name)) => match shape {
                    Shape::Record(record) => record.field(name).cloned().unwrap_or(Shape::Unknown),
                    _ => Shape::Unknown,
                },
                Stage::Map(_) => Shape::Unknown,
                _ => shape,
            },
        )
    }
    pub fn finished(&self) -> bool {
        self.pipeline
            .stages
            .iter()
            .enumerate()
            .any(|(i, s)| matches!(s,Stage::Data(IterStage::Take(n)) if self.counts[i]>=*n))
    }
}
