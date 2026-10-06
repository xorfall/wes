//! Bounded UI observation schema. No renderer data, commands or execution authority.
use super::RenderScope;
use serde_json::Value;
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct HostId(String);
impl<'de> serde::Deserialize<'de> for HostId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        let id = uuid::Uuid::parse_str(&text)
            .map_err(|_| serde::de::Error::custom("Invalid render host identity"))?;
        if text.len() != 36 || id.to_string() != text {
            return Err(serde::de::Error::custom("Invalid render host identity"));
        }
        Ok(Self(text))
    }
}
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenderReceipt {
    pub host: HostId,
    pub digest: String,
    pub mode: RenderMode,
    pub status: RenderState,
    #[serde(rename = "requestedInputRevision")]
    pub requested_input_revision: Option<String>,
    #[serde(rename = "drawnInputRevision")]
    pub drawn_input_revision: Option<String>,
    #[serde(rename = "sentSequence")]
    pub sent_sequence: u64,
    #[serde(rename = "ackSequence")]
    pub ack_sequence: u64,
    pub error: Option<RenderFailure>,
}
#[derive(serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    Preview,
    Expanded,
    Window,
}
#[derive(serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RenderState {
    Loading,
    Ready,
    Drawn,
    Failed,
}
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderFailure {
    AssetsUnavailable,
    RendererFailed,
    CommunicationRejected,
    DrawTimeout,
    NavigationRejected,
    DeliveryFailed,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Packet {
    ok: bool,
    workspace: String,
    generation: String,
    node: String,
    instance: String,
    hosts: Vec<RenderReceipt>,
}
pub fn decode(response: Value, scope: &RenderScope, digest: &str) -> Option<Vec<RenderReceipt>> {
    if response.to_string().len() > 32 * 1024 {
        return None;
    }
    let packet: Packet = serde_json::from_value(response).ok()?;
    let actual = RenderScope {
        workspace: packet.workspace,
        generation: packet.generation,
        node: packet.node,
        instance: packet.instance,
    };
    if !packet.ok || actual != *scope || packet.hosts.len() > 32 {
        return None;
    }
    let mut ids = std::collections::BTreeSet::new();
    for receipt in &packet.hosts {
        if !ids.insert(receipt.host.clone())
            || receipt.digest != digest
            || receipt.sent_sequence >= (1u64 << 53)
            || receipt.ack_sequence > receipt.sent_sequence
            || [
                &receipt.requested_input_revision,
                &receipt.drawn_input_revision,
            ]
            .iter()
            .any(|revision| {
                revision
                    .as_ref()
                    .is_some_and(|v| v.len() > 20 || v.parse::<u64>().is_err())
            })
        {
            return None;
        }
    }
    Some(packet.hosts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn foreign_scopes_private_fields_and_malformed_receipts_fail_closed() {
        let scope = RenderScope {
            workspace: "lab".into(),
            generation: "g".into(),
            node: "id1".into(),
            instance: "instance".into(),
        };
        let digest = "a".repeat(64);
        let receipt = json!({"host":uuid::Uuid::new_v4().to_string(),"digest":digest,"mode":"preview","status":"drawn","requestedInputRevision":"1","drawnInputRevision":"1","sentSequence":1,"ackSequence":1,"error":null});
        let valid = json!({"ok":true,"workspace":"lab","generation":"g","node":"id1","instance":"instance","hosts":[receipt]});
        assert_eq!(decode(valid.clone(), &scope, &digest).unwrap().len(), 1);
        for key in ["workspace", "generation", "node", "instance"] {
            let mut bad = valid.clone();
            bad[key] = json!("foreign");
            assert!(decode(bad, &scope, &digest).is_none());
        }
        for (key, value) in [
            ("error", json!("PRIVATE_ERROR")),
            ("status", json!("unknown")),
            ("ackSequence", json!(2)),
            ("input", json!("PRIVATE_INPUT")),
            ("host", json!("PRIVATE_HOST")),
        ] {
            let mut bad = valid.clone();
            bad["hosts"][0][key] = value;
            assert!(decode(bad, &scope, &digest).is_none());
        }
        let mut bad = valid.clone();
        bad["hosts"] = json!(vec![valid["hosts"][0].clone(); 33]);
        assert!(decode(bad, &scope, &digest).is_none());
    }
}
