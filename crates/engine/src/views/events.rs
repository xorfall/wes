//! Ordered event capture is a bounded observation window, never an execution/replay queue.
use super::*;
use std::collections::VecDeque;
use wes_core::{Provenance, Shape};
#[derive(Clone, Debug)]
pub struct EventEmission {
    pub port: String,
    pub data: Data,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Capture {
    entries: VecDeque<(String, Value, u64)>,
    omitted: BTreeMap<String, u64>,
    bytes: u64,
    sequence: u64,
}
impl Capture {
    pub fn append(&self, package: &Package, emissions: &[EventEmission]) -> Result<Self, Error> {
        if emissions.len() > 16 {
            return Err(Error::Capacity);
        }
        let mut next = self.clone();
        for event in emissions {
            let port = package
                .manifest
                .outputs
                .get(&event.port)
                .filter(|p| p.shared && p.mode == wes_views::Mode::Event)
                .ok_or(Error::Interaction)?;
            let contract = package
                .contracts
                .resolve(&port.r#type)
                .map_err(|_| Error::Interaction)?;
            if !event.data.is_materialized() || !contract.issues(&event.data).is_empty() {
                return Err(Error::Interaction);
            }
            let value = Value::new(contract.shape(), event.data.clone(), Provenance::default())
                .map_err(|_| Error::Interaction)?;
            let charge = crate::value_size::value_charge(&value, 8192).ok_or(Error::Capacity)?
                + event.port.len() as u64
                + 64;
            next.sequence = next.sequence.checked_add(1).ok_or(Error::Capacity)?;
            next.bytes += charge;
            next.entries.push_back((event.port.clone(), value, charge));
            while next.entries.len() > wes_budgets::get("view.events") as usize
                || next.bytes > wes_budgets::get("view.events.bytes")
            {
                let (port, _, charge) = next.entries.pop_front().ok_or(Error::Capacity)?;
                next.bytes -= charge;
                let count = next.omitted.entry(port).or_default();
                *count = count.checked_add(1).ok_or(Error::Capacity)?;
            }
        }
        Ok(next)
    }
    pub fn read(&self, port: &str, shape: Shape, provenance: Provenance) -> Result<Value, Error> {
        let count = self.omitted.get(port).copied().unwrap_or_default();
        let mut provenance =
            provenance.with_fact("view.events.sequence", self.sequence.to_string());
        if count > 0 {
            provenance = provenance.cautioned([format!(
                "View event window omitted {count} older {port} events."
            )]);
        }
        Value::new(
            Shape::List(Box::new(shape)),
            Data::List(
                self.entries
                    .iter()
                    .filter(|(p, _, _)| p == port)
                    .map(|(_, v, _)| v.data().clone())
                    .collect(),
            ),
            provenance,
        )
        .map_err(|_| Error::Interaction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_event_refuses_the_whole_commit_without_mutating_the_window() {
        let package=Package::parse(r#"{"name":"Notes","id":"notes","summary":"Typed notes","renderer":"View.tsx","input":"Input","outputs":{"note":{"type":"Text","mode":"event","shared":true}},"interaction":{"protocol":"Notes","state":"Input","event":"Input","sharedFields":["title"]}}"#,"types: {Input: {base: Record, fields: {title: Text}}}").unwrap();
        let capture = Capture::default();
        assert!(matches!(
            capture.append(
                &package,
                &[
                    EventEmission {
                        port: "note".into(),
                        data: Data::Text("small".into())
                    },
                    EventEmission {
                        port: "note".into(),
                        data: Data::Text("x".repeat(4096).into())
                    }
                ]
            ),
            Err(Error::Capacity)
        ));
        assert_eq!(capture.sequence, 0);
        assert_eq!(capture.bytes, 0);
        assert!(capture.entries.is_empty());
        let next = capture
            .append(
                &package,
                &[EventEmission {
                    port: "note".into(),
                    data: Data::Text("ok".into()),
                }],
            )
            .unwrap();
        assert_eq!(next.sequence, 1);
    }
}
