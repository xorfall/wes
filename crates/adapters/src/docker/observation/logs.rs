//! Shared log selection, transport and row decoding; finite and streaming owners stay separate.
mod decoder;
mod follow;
use super::*;
use decoder::{Decoder, Line};
pub(super) use follow::capability as follow_capability;
use std::borrow::Cow;
use wes_core::Timestamp;

fn wire_limit() -> usize {
    wes_budgets::get("docker.log.wire.bytes") as usize
}

pub(super) fn capability() -> Capability {
    let mut cap = Capability::new(["logs"], shape(), Safety::Safe);
    cap.summary = "Read a finite retained log tail for an exact container ID; tail defaults to 200 (1..5000), since/until are Unix seconds; truncation is explicit".into();
    cap.parameters = vec![
        Parameter::new("container", primitive(Primitive::Text), true).suggesting("container"),
        Parameter::new("tail", primitive(Primitive::Int), false),
        Parameter::new("since", primitive(Primitive::Int), false),
        Parameter::new("until", primitive(Primitive::Int), false),
    ];
    cap
}
fn shape() -> Shape {
    record(
        "DockerLogTail",
        vec![
            ("container", primitive(Primitive::Text)),
            ("received_at_ns", primitive(Primitive::Int)),
            ("requested_tail", primitive(Primitive::Int)),
            ("since", primitive(Primitive::Int)),
            ("until", primitive(Primitive::Int)),
            (
                "rows",
                Shape::List(Box::new(record(
                    "DockerLogLine",
                    vec![
                        ("stream", primitive(Primitive::Text)),
                        ("timestamp_ns", option(primitive(Primitive::Int))),
                        ("text", primitive(Primitive::Text)),
                        ("partial", primitive(Primitive::Bool)),
                        ("lossy", primitive(Primitive::Bool)),
                    ],
                ))),
            ),
            ("returned", primitive(Primitive::Int)),
            ("truncated", primitive(Primitive::Bool)),
            ("truncation", option(primitive(Primitive::Text))),
        ],
    )
}
struct Selection {
    container: String,
    tail: usize,
    since: i64,
    until: i64,
}
impl Selection {
    fn from_call(call: &Call) -> Result<Self, InvocationError> {
        let container = match call.arguments.get("container").map(Value::data) {
            Some(Data::Text(id)) if digest(id) => id.clone(),
            _ => {
                return Err(failure(
                    "DOCKER_ARGUMENT",
                    "container requires a full 64-character Docker ID; run containers to select one",
                ));
            }
        };
        let tail = match call.arguments.get("tail").map(Value::data) {
            None => 200,
            Some(Data::Int(n)) if (1..=5000).contains(n) => *n as usize,
            _ => {
                return Err(failure(
                    "DOCKER_ARGUMENT",
                    "tail must be between 1 and 5000",
                ));
            }
        };
        let bound = |name| match call.arguments.get(name).map(Value::data) {
            None => Ok(0),
            Some(Data::Int(n)) if *n >= 0 => Ok(*n),
            _ => Err(failure(
                "DOCKER_ARGUMENT",
                format!("{name} must be nonnegative Unix seconds; zero means no bound"),
            )),
        };
        let (since, until) = (bound("since")?, bound("until")?);
        if until != 0 && since > until {
            return Err(failure("DOCKER_ARGUMENT", "since must not exceed until"));
        }
        Ok(Self {
            container: container.to_string(),
            tail,
            since,
            until,
        })
    }
    fn error(&self, error: ClientError) -> InvocationError {
        if matches!(error, ClientError::Status(404)) {
            failure(
                "DOCKER_MISSING",
                format!(
                    "Container {} no longer exists; refresh inventory and explicitly select its replacement",
                    self.container
                ),
            )
        } else {
            transport(error)
        }
    }
}
impl Observation {
    pub(super) async fn logs(&self, call: Call) -> Result<Value, InvocationError> {
        let selected = Selection::from_call(&call)?;
        let (mut response, tty) = self.open_logs(&selected, false).await?;
        let mut wire = Vec::new();
        let mut cut = false;
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            self.client.invalidate();
            failure(
                "DOCKER_TRANSPORT",
                "Docker log response was interrupted; no partial result was published",
            )
        })? {
            self.permitted()?;
            let remaining = wire_limit() - wire.len();
            wire.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if chunk.len() > remaining {
                cut = true;
                break;
            }
        }
        // Owned by this invocation: deadline/cancellation also drops the HTTP body.
        drop(response);
        self.permitted()?;
        let decoded = decode(&wire, tty, selected.tail, cut)?;
        let reason = match (cut, decoded.line_cut) {
            (false, false) => None,
            (true, false) => Some("byte_limit"),
            (false, true) => Some("line_limit"),
            (true, true) => Some("byte_and_line_limit"),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| malformed())?
            .as_nanos()
            .try_into()
            .map_err(|_| malformed())?;
        let data = Data::Record(indexmap::IndexMap::from_iter([
            ("container".into(), Data::Text(selected.container.into())),
            ("received_at_ns".into(), Data::Int(now)),
            ("requested_tail".into(), Data::Int(selected.tail as i64)),
            ("since".into(), Data::Int(selected.since)),
            ("until".into(), Data::Int(selected.until)),
            ("returned".into(), Data::Int(decoded.rows.len() as i64)),
            ("rows".into(), Data::List(decoded.rows)),
            ("truncated".into(), Data::Bool(reason.is_some())),
            (
                "truncation".into(),
                Data::Option(reason.map(|s| Box::new(Data::Text(s.into())))),
            ),
        ]));
        Value::new(
            shape(),
            data,
            Provenance::default().with_fact("docker.api", "1.45"),
        )
        .map_err(|_| malformed())
    }
    async fn open_logs(
        &self,
        selected: &Selection,
        follow: bool,
    ) -> Result<(reqwest::Response, bool), InvocationError> {
        let api = self.client.endpoint().await.map_err(transport)?;
        self.permitted()?;
        let base = format!("{api}/containers/{}", selected.container);
        // The logs endpoint does not reliably send Content-Type. TTY is immutable
        // container configuration; the ID check also prevents accidental replacement.
        let metadata = self
            .client
            .json(self.client.http.get(format!("{base}/json")))
            .await
            .map_err(|e| selected.error(e))?;
        if metadata.get("Id").and_then(Json::as_str) != Some(selected.container.as_str()) {
            return Err(failure(
                "DOCKER_IDENTITY",
                "Docker returned a different container identity",
            ));
        }
        let tty = metadata
            .pointer("/Config/Tty")
            .and_then(Json::as_bool)
            .ok_or_else(malformed)?;
        self.permitted()?;
        let mut url = url::Url::parse(&format!("{base}/logs")).expect("constant URL and hex ID");
        url.query_pairs_mut().extend_pairs([
            ("stdout", "true".to_owned()),
            ("stderr", "true".to_owned()),
            ("timestamps", "true".to_owned()),
            ("follow", follow.to_string()),
            ("tail", selected.tail.to_string()),
            ("since", selected.since.to_string()),
            ("until", selected.until.to_string()),
        ]);
        let request = self.client.http.get(url);
        let response = self
            .client
            .response(request)
            .await
            .map_err(|e| selected.error(e))?;
        Ok((response, tty))
    }
}

struct Decoded {
    rows: Vec<Data>,
    line_cut: bool,
}
fn line_data(line: &Line) -> indexmap::IndexMap<String, Data> {
    let decoded = String::from_utf8_lossy(&line.bytes);
    let lossy = matches!(decoded, Cow::Owned(_));
    let stamp = decoded.split_once(' ').and_then(|(prefix, text)| {
        if prefix.len() > 64 {
            return None;
        }
        let time = prefix.parse::<Timestamp>().ok()?.parts();
        let ns = i128::from(time.seconds()) * 1_000_000_000 + i128::from(time.nanos());
        Some((i64::try_from(ns).ok()?, text))
    });
    let text = stamp.map_or(decoded.as_ref(), |(_, text)| text);
    indexmap::IndexMap::from_iter([
        (
            "stream".into(),
            Data::Text(["stdout", "stderr", "tty"][line.channel].into()),
        ),
        (
            "timestamp_ns".into(),
            Data::Option(stamp.map(|(ns, _)| Box::new(Data::Int(ns)))),
        ),
        ("text".into(), Data::Text(text.into())),
        ("partial".into(), Data::Bool(line.partial)),
        ("lossy".into(), Data::Bool(lossy)),
    ])
}
fn framing() -> InvocationError {
    failure(
        "DOCKER_LOG_FRAME",
        "Docker log framing is malformed or ended inside a frame; no partial result was published",
    )
}
fn decode(wire: &[u8], tty: bool, limit: usize, cut: bool) -> Result<Decoded, InvocationError> {
    let mut result = Decoded {
        rows: vec![],
        line_cut: false,
    };
    let mut emit = |line: Line| {
        if result.rows.len() == limit {
            result.line_cut = true;
        } else {
            result.rows.push(Data::Record(line_data(&line)));
        }
        Ok(())
    };
    let mut decoder = Decoder::new(tty, wire_limit());
    decoder.feed(wire, &mut emit)?;
    decoder.finish(cut, &mut emit)?;
    Ok(result)
}

#[cfg(test)]
mod tests;
