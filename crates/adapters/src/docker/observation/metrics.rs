//! Bounded JSON observations. Upstream payloads never cross the explicit field projection.
use super::streaming::sink_error;
use super::*;
use wes_core::Timestamp;
use wes_engine::streams::StreamSink;
mod fields;
use fields::{event, stats};
fn record_limit() -> usize {
    wes_budgets::get("docker.metrics.bytes") as usize
}
fn base(name: &str, shape: Shape) -> Capability {
    let mut cap = Capability::new([name], shape, Safety::Safe);
    cap.streaming = true;
    cap.parameters =
        vec![Parameter::new("container", primitive(Primitive::Text), true).suggesting("container")];
    cap
}
pub(super) fn stats_capability() -> Capability {
    let mut cap = base("stats", fields::stats_shape());
    cap.summary = "Follow exact-container resource observations; missing CPU/memory metrics remain optional with reasons, no reconnect".into();
    cap
}
pub(super) fn events_capability() -> Capability {
    let mut cap = base("events", fields::event_shape());
    cap.summary = "Follow future events for one exact container; type/action stay distinct, unknown actions retained, no replay or reconnect".into();
    cap
}
fn container(call: &Call) -> Result<String, InvocationError> {
    match call.arguments.get("container").map(Value::data) {
        Some(Data::Text(id)) if digest(id) => Ok(id.to_string()),
        _ => Err(failure(
            "DOCKER_ARGUMENT",
            "container requires a full 64-character Docker ID; run containers to select one",
        )),
    }
}
fn timestamp(value: &Json) -> Option<i64> {
    let s = value.as_str()?;
    if s.len() > 64 {
        return None;
    }
    let parts = s.parse::<Timestamp>().ok()?.parts();
    i64::try_from(i128::from(parts.seconds()) * 1_000_000_000 + i128::from(parts.nanos())).ok()
}
fn received() -> Result<i64, InvocationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| malformed())?
        .as_nanos()
        .try_into()
        .map_err(|_| malformed())
}
impl Observation {
    pub(super) async fn follow_metrics(
        &self,
        call: Call,
        sink: StreamSink,
    ) -> Result<(), InvocationError> {
        let id = container(&call)?;
        let is_stats = call.capability.path == ["stats"];
        let timeout =
            Duration::from_millis(self.binding.import().timeout_ms().unwrap_or(15_000) as u64);
        let mut response = tokio::time::timeout(timeout, async {
            let api = self.client.endpoint().await.map_err(transport)?;
            self.permitted()?;
            // Events endpoint accepts missing IDs without error. Validate identity within this
            // admitted call so a typo cannot silently create an empty forever subscription.
            if !is_stats {
                let info = self
                    .client
                    .json(self.client.http.get(format!("{api}/containers/{id}/json")))
                    .await
                    .map_err(|e| selected_error(e, &id))?;
                if info.get("Id").and_then(Json::as_str) != Some(&id) {
                    return Err(failure(
                        "DOCKER_IDENTITY",
                        "Docker returned a different container identity",
                    ));
                }
            }
            let mut url = url::Url::parse(&if is_stats {
                format!("{api}/containers/{id}/stats")
            } else {
                format!("{api}/events")
            })
            .expect("constant Docker URL");
            if is_stats {
                url.query_pairs_mut().append_pair("stream", "true");
            } else {
                url.query_pairs_mut().append_pair(
                    "filters",
                    &serde_json::json!({"type":["container"],"container":[id]}).to_string(),
                );
            }
            self.client
                .response(self.client.http.get(url))
                .await
                .map_err(|e| selected_error(e, &id))
        })
        .await
        .map_err(|_| {
            failure(
                "DOCKER_TIMEOUT",
                "Docker observation stream did not open before its deadline",
            )
        })??;
        self.permitted()?;
        sink.opened().map_err(sink_error)?;
        let mut decoder = Records::default();
        let mut sequence = 0i64;
        let mut emit = |json: Json| {
            sequence = sequence.checked_add(1).ok_or_else(|| {
                failure(
                    "DOCKER_STREAM_SEQUENCE",
                    "Docker stream sequence exhausted; start a new run",
                )
            })?;
            let fields = if is_stats {
                stats(&json, &id, sequence, received()?)?
            } else {
                event(&json, &id, sequence, received()?)?
            };
            let value = Value::new(
                call.capability.result.clone(),
                Data::Record(fields),
                Provenance::default().with_fact("docker.api", "1.45"),
            )
            .map_err(|_| malformed())?;
            Ok::<_, InvocationError>(value)
        };
        loop {
            let chunk = response.chunk().await.map_err(|_| {
                self.client.invalidate();
                failure(
                    "DOCKER_TRANSPORT",
                    "Docker observation stream was interrupted; no reconnect was attempted",
                )
            })?;
            let Some(chunk) = chunk else {
                let mut records = Vec::new();
                decoder.finish(&mut |json| {
                    records.push(json);
                    Ok(())
                })?;
                for json in records {
                    sink.send(emit(json)?).await.map_err(sink_error)?;
                }
                return Ok(());
            };
            for part in chunk.chunks(1024) {
                let mut records = Vec::new();
                decoder.feed(part, &mut |json| {
                    records.push(json);
                    Ok(())
                })?;
                for json in records {
                    sink.send(emit(json)?).await.map_err(sink_error)?;
                }
                tokio::task::yield_now().await;
            }
        }
    }
}
fn selected_error(error: ClientError, id: &str) -> InvocationError {
    if matches!(error, ClientError::Status(404)) {
        failure(
            "DOCKER_MISSING",
            format!(
                "Container {id} no longer exists; refresh inventory and explicitly select its replacement"
            ),
        )
    } else {
        transport(error)
    }
}
#[derive(Default)]
struct Records {
    bytes: Vec<u8>,
}
impl Records {
    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(Json) -> Result<(), InvocationError>,
    ) -> Result<(), InvocationError> {
        for &byte in bytes {
            if byte == b'\n' {
                self.finish(emit)?;
            } else {
                if self.bytes.len() == record_limit() {
                    return Err(failure(
                        "DOCKER_STREAM_RECORD",
                        "Docker JSON record exceeds 1 MiB; stream stopped without reconnect",
                    ));
                }
                self.bytes.push(byte);
            }
        }
        Ok(())
    }
    fn finish(
        &mut self,
        emit: &mut impl FnMut(Json) -> Result<(), InvocationError>,
    ) -> Result<(), InvocationError> {
        if self.bytes.iter().all(u8::is_ascii_whitespace) {
            self.bytes.clear();
            return Ok(());
        }
        let json = serde_json::from_slice(&self.bytes).map_err(|_| failure("DOCKER_STREAM_RECORD", "Docker stream contains malformed or incomplete JSON; stream stopped without reconnect"))?;
        self.bytes.clear();
        emit(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn sample() -> Json {
        json!({"id":"a".repeat(64),"read":"2026-09-24T00:00:01Z","preread":"2026-09-24T00:00:00Z",
        "cpu_stats":{"cpu_usage":{"total_usage":200},"system_cpu_usage":2000,"online_cpus":4},
        "precpu_stats":{"cpu_usage":{"total_usage":100},"system_cpu_usage":1000},
        "memory_stats":{"usage":1000,"limit":2000,"stats":{"inactive_file":200}},"secret":"do-not-export"})
    }
    #[test]
    fn stats_ratios_optional_counters_and_memory_sources_are_explicit() {
        let mut raw = sample();
        let id = "a".repeat(64);
        let result = stats(&raw, &id, 1, 2).unwrap();
        let decimal = |s: &str| {
            Data::Option(Some(Box::new(Data::Decimal(
                format!("{s}.000000").parse().unwrap(),
            ))))
        };
        assert_eq!(result["cpu_percent"], decimal("40"));
        assert_eq!(result["memory_percent"], decimal("40"));
        assert_eq!(
            result["memory_working_set_bytes"],
            Data::Option(Some(Box::new(Data::Int(800))))
        );
        assert!(!format!("{result:?}").contains("do-not-export"));
        Value::new(
            fields::stats_shape(),
            Data::Record(result),
            Provenance::default(),
        )
        .unwrap();
        raw["cpu_stats"]["cpu_usage"]["total_usage"] = json!(100);
        assert_eq!(stats(&raw, &id, 1, 2).unwrap()["cpu_percent"], decimal("0"));
        for (path, value) in [
            ("/precpu_stats/system_cpu_usage", json!(2000)),
            ("/cpu_stats/cpu_usage/total_usage", json!(99)),
            ("/cpu_stats/online_cpus", json!(0)),
            ("/preread", json!("0001-01-01T00:00:00Z")),
            ("/cpu_stats/system_cpu_usage", Json::Null),
        ] {
            let mut raw = sample();
            *raw.pointer_mut(path).unwrap() = value;
            let result = stats(&raw, &id, 1, 2).unwrap();
            assert_eq!(result["cpu_percent"], Data::Option(None));
            assert_ne!(result["cpu_unavailable"], Data::Option(None));
        }
        for name in ["total_inactive_file", "inactive_file", "cache"] {
            let mut raw = sample();
            raw["memory_stats"]["stats"] = json!({name:200});
            assert_eq!(
                stats(&raw, &id, 1, 2).unwrap()["memory_percent"],
                decimal("40")
            );
        }
        for memory in [
            json!({}),
            json!({"usage":1000,"limit":0,"stats":{"cache":200}}),
            json!({"usage":100,"limit":2000,"stats":{"cache":200}}),
        ] {
            raw["memory_stats"] = memory;
            let result = stats(&raw, &id, 1, 2).unwrap();
            assert_eq!(result["memory_percent"], Data::Option(None));
            assert_ne!(result["memory_unavailable"], Data::Option(None));
        }
        raw = sample();
        raw["cpu_stats"]["online_cpus"] = Json::Null;
        raw["cpu_stats"]["cpu_usage"]["percpu_usage"] = json!([1, 2]);
        assert_eq!(
            stats(&raw, &id, 1, 2).unwrap()["cpu_percent"],
            decimal("20")
        );
        raw["cpu_stats"]["system_cpu_usage"] = json!(-1);
        assert!(stats(&raw, &id, 1, 2).is_err());
        assert!(stats(&sample(), &"b".repeat(64), 1, 2).is_err());
    }
    #[test]
    fn future_event_actions_survive_but_attributes_and_wrong_identities_do_not() {
        let id = "a".repeat(64);
        let mut raw = json!({"Actor":{"ID":id,"Attributes":{"secret":"do-not-export"}},"Type":"container","Action":"future_action","timeNano":123});
        let result = event(&raw, &id, 1, 2).unwrap();
        assert_eq!(result["action"], Data::Text("future_action".into()));
        assert_eq!(
            result["timestamp_ns"],
            Data::Option(Some(Box::new(Data::Int(123))))
        );
        assert!(!format!("{result:?}").contains("do-not-export"));
        Value::new(
            fields::event_shape(),
            Data::Record(result),
            Provenance::default(),
        )
        .unwrap();
        raw["timeNano"] = Json::Null;
        raw["time"] = json!(2);
        assert_eq!(
            event(&raw, &id, 1, 2).unwrap()["timestamp_ns"],
            Data::Option(Some(Box::new(Data::Int(2_000_000_000))))
        );
        raw["time"] = Json::Null;
        assert_eq!(
            event(&raw, &id, 1, 2).unwrap()["timestamp_ns"],
            Data::Option(None)
        );
        assert!(event(&raw, &"b".repeat(64), 1, 2).is_err());
        raw["Type"] = json!("image");
        assert!(event(&raw, &id, 1, 2).is_err());
        raw["Type"] = json!("container");
        raw["Action"] = json!("x".repeat(4097));
        assert!(event(&raw, &id, 1, 2).is_err());
    }
    #[test]
    fn json_frames_are_incremental_bounded_and_never_publish_a_truncated_record() {
        let wire = "{\"text\":\"ş😀\"}\n\r\n{\"last\":true}";
        let mut decoder = Records::default();
        let mut values = vec![];
        for byte in wire.as_bytes() {
            decoder
                .feed(&[*byte], &mut |v| {
                    values.push(v);
                    Ok(())
                })
                .unwrap();
        }
        decoder
            .finish(&mut |v| {
                values.push(v);
                Ok(())
            })
            .unwrap();
        assert_eq!(values, vec![json!({"text":"ş😀"}), json!({"last":true})]);
        let mut bad = Records::default();
        bad.feed(b"{\"partial\":", &mut |_| panic!()).unwrap();
        assert!(bad.finish(&mut |_| panic!()).is_err());
        let mut big = Records::default();
        assert!(
            big.feed(&vec![b' '; record_limit() + 1], &mut |_| panic!())
                .is_err()
        );
    }
}
