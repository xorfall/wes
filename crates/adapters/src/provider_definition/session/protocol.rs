//! Version 1: a four-byte big-endian length followed by one strict UTF-8 JSON frame.
use super::*;
use serde::{Deserialize, Serialize, ser::SerializeMap};
use serde_json::value::RawValue;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    task::JoinHandle,
};
pub(super) fn max_frame() -> usize {
    wes_budgets::get("provider.session.frame.bytes") as usize
}
pub(super) fn limits() -> codec::Limits {
    codec::Limits {
        bytes: max_frame(),
        nodes: 100_000,
    }
}
#[derive(Serialize)]
pub(super) struct Contracts<'a> {
    pub result: &'a str,
    pub parameters: BTreeMap<&'a str, &'a str>,
    pub control: Option<[&'a str; 2]>,
}
pub(super) struct Secrets<'a>(pub &'a BTreeMap<String, Option<wes_engine::credentials::Secret>>);
impl Serialize for Secrets<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            map.serialize_entry(name, &value.as_ref().map(|s| s.expose_secret()))?;
        }
        map.end()
    }
}
#[derive(Serialize)]
pub(super) struct Start<'a> {
    pub version: u32,
    pub session: &'a str,
    pub run: &'a str,
    pub operation: &'a [String],
    pub arguments: &'a RawValue,
    pub endpoint: Option<&'a str>,
    pub credentials: Secrets<'a>,
    pub contracts: Contracts<'a>,
    pub event_credit: u8,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Frame {
    pub version: u32,
    pub session: String,
    #[serde(deserialize_with = "message")]
    pub message: Message,
}
pub(super) enum Message {
    Ready,
    Event { sequence: u64, data: Box<RawValue> },
    Result { data: Box<RawValue> },
    Control { id: String, outcome: Receipt },
    End,
    Failed,
}
pub(super) enum Receipt {
    Confirmed { data: Box<RawValue> },
    Refused,
    Unknown,
}
// Internally tagged serde enums buffer their payload as generic Content, which
// cannot preserve RawValue tokens. Select the tag, then strictly deserialize
// that exact raw document; no JSON value/float round trip or duplicate-key loss.
#[derive(Deserialize)]
struct Tag {
    kind: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {
    kind: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    kind: String,
    data: Box<RawValue>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    kind: String,
    sequence: u64,
    data: Box<RawValue>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    kind: String,
    id: String,
    outcome: Box<RawValue>,
}
fn decode_message(raw: &str) -> Result<Message, serde_json::Error> {
    let Tag { kind } = serde_json::from_str(raw)?;
    Ok(match kind.as_str() {
        "ready" | "end" | "failed" => {
            let Empty { kind } = serde_json::from_str(raw)?;
            match kind.as_str() {
                "ready" => Message::Ready,
                "end" => Message::End,
                _ => Message::Failed,
            }
        }
        "event" => {
            let Event {
                kind,
                sequence,
                data,
            } = serde_json::from_str(raw)?;
            debug_assert_eq!(kind, "event");
            Message::Event { sequence, data }
        }
        "result" => {
            let Payload { kind, data } = serde_json::from_str(raw)?;
            debug_assert_eq!(kind, "result");
            Message::Result { data }
        }
        "control" => {
            let Control { kind, id, outcome } = serde_json::from_str(raw)?;
            debug_assert_eq!(kind, "control");
            let Tag { kind } = serde_json::from_str(outcome.get())?;
            let outcome = match kind.as_str() {
                "confirmed" => {
                    let Payload { kind, data } = serde_json::from_str(outcome.get())?;
                    debug_assert_eq!(kind, "confirmed");
                    Receipt::Confirmed { data }
                }
                "refused" | "unknown" => {
                    let Empty { kind } = serde_json::from_str(outcome.get())?;
                    if kind == "refused" {
                        Receipt::Refused
                    } else {
                        Receipt::Unknown
                    }
                }
                _ => return Err(serde::de::Error::custom("unknown control receipt kind")),
            };
            Message::Control { id, outcome }
        }
        _ => return Err(serde::de::Error::custom("unknown provider message kind")),
    })
}
fn message<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Message, D::Error> {
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    decode_message(raw.get()).map_err(serde::de::Error::custom)
}
pub(super) fn value(
    bytes: &[u8],
    contract: &Arc<Contract>,
    token: &CancellationToken,
) -> Result<Value, InvocationError> {
    let data = codec::decode_json_for_contract(bytes, limits(), contract)
        .map_err(|_| error("Session result is malformed or exceeds its decoding budget"))?;
    let value = Value::new(Shape::Unknown, data, Provenance::default())
        .map_err(|_| error("Invalid session result"))?;
    boundary::checked_result(contract, &value, &|| token.is_cancelled())
        .map_err(|_| error("Session result violates its captured contract"))
}
pub(super) fn action(
    session: &str,
    id: &str,
    value: &Value,
) -> Result<Zeroizing<Vec<u8>>, InvocationError> {
    let raw = codec::encode_request_data(value.data(), limits())
        .map_err(|_| error("Session control encoding exceeded its budget"))?;
    let raw = RawValue::from_string(String::from_utf8(raw).expect("JSON UTF-8"))
        .map_err(|_| error("Invalid session control"))?;
    #[derive(Serialize)]
    struct Action<'a> {
        kind: &'static str,
        id: &'a str,
        data: &'a RawValue,
    }
    #[derive(Serialize)]
    struct Envelope<'a> {
        version: u32,
        session: &'a str,
        message: Action<'a>,
    }
    let encoded = Zeroizing::new(
        serde_json::to_vec(&Envelope {
            version: 1,
            session,
            message: Action {
                kind: "control",
                id,
                data: &raw,
            },
        })
        .map_err(|_| error("Session control encoding failed"))?,
    );
    if encoded.len() > max_frame() {
        return Err(error("Session control exceeds its frame budget"));
    }
    Ok(encoded)
}
async fn write(
    output: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
    handed_off: Option<&mut bool>,
) -> Result<(), InvocationError> {
    let length = u32::try_from(bytes.len())
        .ok()
        .filter(|n| *n > 0 && *n as usize <= max_frame())
        .ok_or_else(|| error("Invalid session frame size"))?;
    output
        .write_all(&length.to_be_bytes())
        .await
        .map_err(|_| error("Session request write failed"))?;
    output
        .write_all(bytes)
        .await
        .map_err(|_| error("Session request write failed"))?;
    // A strict peer cannot start an incomplete frame. After all bytes are
    // written it may already act, even if flush, cancellation or EOF follows.
    if let Some(handed_off) = handed_off {
        *handed_off = true;
    }
    output
        .flush()
        .await
        .map_err(|_| error("Session request flush failed"))
}
pub(super) async fn write_until(
    output: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
    token: &CancellationToken,
    until: tokio::time::Instant,
    handed_off: Option<&mut bool>,
) -> Result<(), InvocationError> {
    tokio::select! {biased;
        ()=token.cancelled()=>Err(InvocationError::Cancelled),
        result=write(output,bytes,handed_off)=>result,
        _=tokio::time::sleep_until(until)=>Err(error("Session write deadline elapsed; no retry was performed")),
    }
}
pub(super) fn credit(session: &str) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"version":1,"session":session,"message":{"kind":"credit","count":1}}),
    )
    .expect("bounded credit frame")
}
pub(super) async fn read(input: &mut (impl AsyncRead + Unpin)) -> Result<Frame, InvocationError> {
    let length = input
        .read_u32()
        .await
        .map_err(|_| error("Session protocol was truncated or closed"))? as usize;
    if length == 0 || length > max_frame() {
        return Err(error("Session frame exceeds its byte budget"));
    }
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .await
        .map_err(|_| error("Session frame was truncated"))?;
    serde_json::from_slice(&bytes).map_err(|_| error("Malformed session frame"))
}
pub(super) fn reader(
    mut input: impl AsyncRead + Unpin + Send + 'static,
    send: mpsc::Sender<Result<Frame, InvocationError>>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame =
                tokio::select! {biased; ()=stop.cancelled()=>break, frame=read(&mut input)=>frame};
            let failed = frame.is_err();
            if tokio::select! {biased; ()=stop.cancelled()=>false, sent=send.send(frame)=>sent.is_ok()}
                == false
                || failed
            {
                break;
            }
        }
    })
}
pub(super) fn diagnostics(
    mut input: impl AsyncRead + Unpin + Send + 'static,
    stop: CancellationToken,
) -> JoinHandle<Result<(), ()>> {
    tokio::spawn(async move {
        let mut bytes = 0usize;
        let mut buffer = [0; 4096];
        loop {
            let read = tokio::select! {biased; ()=stop.cancelled()=>return Ok(()), read=input.read(&mut buffer)=>read.map_err(|_|())?};
            if read == 0 {
                return Ok(());
            }
            bytes = bytes.checked_add(read).ok_or(())?;
            if bytes > wes_budgets::get("provider.session.stderr.bytes") as usize {
                return Err(());
            }
        }
    })
}
