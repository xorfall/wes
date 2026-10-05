use super::{
    Controller,
    schema::{Notice, Operation, Outcome, Record},
};
use std::{sync::Arc, time::Instant};
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id},
};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

pub struct TelemetryLayer(pub Arc<Controller>);
struct Timing {
    at: Instant,
    kind: Operation,
    outcome: Outcome,
    id: u64,
    parent: Option<u64>,
    epoch: u64,
}
#[derive(Default)]
struct Fields {
    operation: Option<Operation>,
    outcome: Option<Outcome>,
    notice: Option<Notice>,
}
impl Visit for Fields {
    // Never format Debug/Display values. Only literal enum strings on named fields are accepted.
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, f: &Field, value: &str) {
        match f.name() {
            "operation" => self.operation = Operation::parse(value),
            "outcome" => self.outcome = Outcome::parse(value),
            "notice" => self.notice = Notice::parse(value),
            _ => {}
        }
    }
}
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for TelemetryLayer {
    fn register_callsite(
        &self,
        metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if metadata.target() == "wes.telemetry" {
            tracing::subscriber::Interest::sometimes()
        } else {
            tracing::subscriber::Interest::never()
        }
    }
    fn enabled(&self, metadata: &Metadata<'_>, _: Context<'_, S>) -> bool {
        metadata.target() == "wes.telemetry" && self.0.enabled()
    }
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let Some(kind) = fields.operation else { return };
        let Some((epoch, operation_id)) = self.0.begin(kind) else {
            return;
        };
        let parent = attrs
            .parent()
            .and_then(|p| ctx.span(p))
            .or_else(|| {
                if attrs.is_contextual() {
                    ctx.lookup_current()
                } else {
                    None
                }
            })
            .and_then(|s| s.extensions().get::<Timing>().map(|t| t.id));
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Timing {
                at: Instant::now(),
                kind,
                outcome: fields.outcome.unwrap_or(Outcome::Abandoned),
                id: operation_id,
                parent,
                epoch,
            });
        }
    }
    fn on_record(&self, id: &Id, record: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        record.record(&mut fields);
        if let Some(span) = ctx.span(id)
            && let Some(t) = span.extensions_mut().get_mut::<Timing>()
            && let Some(outcome) = fields.outcome
        {
            t.outcome = outcome;
        }
    }
    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id)
            && let Some(t) = span.extensions().get::<Timing>()
        {
            self.0.complete(
                t.epoch,
                Record::Operation {
                    at_ms: self.0.elapsed_ms(),
                    id: t.id,
                    parent: t.parent,
                    kind: t.kind,
                    outcome: t.outcome,
                    elapsed_us: t.at.elapsed().as_micros().min(u64::MAX as u128) as u64,
                },
                true,
            );
        }
    }
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        if let Some(kind) = fields.notice {
            self.0.notice(kind);
        }
    }
}
