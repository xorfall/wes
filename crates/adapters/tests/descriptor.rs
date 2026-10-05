use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use wes_adapters::{
    descriptor::{self, DescriptorError, Reading},
    http::HttpConfig,
};
use wes_core::{Data, Primitive, Shape, capability::Safety};
use wes_engine::{
    credentials::{CredentialError, Credentials, Secret, SecretString},
    driver::CancellationToken,
    providers::{Call, Invoker},
};

const ENDPOINT: &str = "https://example.invalid/api";
#[derive(Default)]
struct Secrets {
    lookups: Mutex<Vec<String>>,
}
impl Credentials for Secrets {
    fn lookup(&self, name: &str) -> Result<Option<Secret>, CredentialError> {
        self.lookups.lock().unwrap().push(name.into());
        match name {
            "available" => Ok(Some(Arc::new(SecretString::from("not-for-metadata")))),
            "unavailable" => Err(CredentialError::Unavailable),
            _ => Ok(None),
        }
    }
}
fn document() -> Value {
    json!({"version":1,"provider":"library","types":{},"operations":[{
        "path":["items","list"],"method":"GET","route":"/items","auth":[],
        "parameters":[],"responses":{"200":"Unknown"}
    }]})
}
fn read(value: Value) -> Result<Reading, DescriptorError> {
    descriptor::read(
        &serde_json::to_vec(&value).unwrap(),
        None,
        Arc::new(Secrets::default()),
        HttpConfig::default(),
        ENDPOINT,
    )
}
fn capability(reading: &Reading) -> &wes_core::capability::Capability {
    reading.description.capabilities().next().unwrap()
}
fn shape(value: &Shape) -> String {
    match value {
        Shape::Record(record) => record.name().into(),
        _ => value.to_string(),
    }
}

#[test]
fn acceptance_descriptor_corpus_preserves_current_contracts() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/descriptors-acceptance.json"
    ))
    .unwrap();
    for case in cases {
        let secrets = Arc::new(Secrets::default());
        let reading = descriptor::read(
            case["input"].as_str().unwrap().as_bytes(),
            None,
            secrets.clone(),
            HttpConfig::default(),
            ENDPOINT,
        );
        assert_eq!(
            reading.is_ok(),
            case["ok"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if let Ok(reading) = reading {
            let cap = capability(&reading);
            assert_eq!(
                json!({"provider":reading.description.name(),"path":cap.path,"summary":cap.summary,
                "safe":cap.safety==Safety::Safe,"stream":cap.streaming,"result":shape(&cap.result),
                "parameters":cap.parameters.iter().map(|p|json!({"name":p.name,"type":shape(&p.shape),"required":p.required})).collect::<Vec<_>>(),
                "secrets":reading.description.secrets()}),
                case["expected"],
                "{}",
                case["name"]
            );
        } else {
            assert!(secrets.lookups.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn auth_missing_credentials_and_hazards_do_not_expose_values() {
    let secrets = Arc::new(Secrets::default());
    let mut doc = document();
    doc["operations"][0]["auth"] = json!([
        {"secret":"available","header":"X-Key","scheme":""},
        {"secret":"missing","query":"api_key"},
        {"secret":"unavailable","user":"reader"}
    ]);
    let reading = descriptor::read(
        &serde_json::to_vec(&doc).unwrap(),
        Some("alternate"),
        secrets.clone(),
        HttpConfig::default(),
        ENDPOINT,
    )
    .unwrap();
    assert_eq!(reading.description.name(), "alternate");
    assert_eq!(
        reading.description.secrets(),
        ["available", "missing", "unavailable"]
    );
    assert_eq!(
        *secrets.lookups.lock().unwrap(),
        ["available", "missing", "unavailable"]
    );
    assert!(reading.warnings.iter().any(|s| s.contains("URL")));
    for name in ["missing", "unavailable"] {
        assert!(
            reading
                .warnings
                .iter()
                .any(|s| s.contains(name) && s.contains("not available"))
        );
    }
    assert!(
        !format!("{:?} {:?}", reading.description, reading.warnings).contains("not-for-metadata")
    );
}

#[test]
fn malformed_documents_fail_atomically_before_credential_lookup() {
    let mut invalid = Vec::new();
    for (key, value) in [
        ("provider", json!(" ")),
        ("operations", json!([])),
        ("notes", json!(42)),
        ("base", json!("https://user:secret@example.invalid")),
        ("unexpected", json!(true)),
    ] {
        let mut doc = document();
        doc[key] = value;
        invalid.push(doc);
    }
    for (key, value) in [
        ("path", json!([])),
        ("path", json!([""])),
        ("responses", json!({"200":"Missing"})),
        ("safety", json!("idempotent")),
        ("stream", json!("true")),
        ("unexpected", json!(true)),
        ("parameters", json!([{"name":"a"},{"name":"a"}])),
        ("parameters", json!({"name":"a","required":true})),
        ("auth", json!({"secret":"available"})),
        ("auth", json!([{"secret":"available","header":null}])),
        (
            "auth",
            json!([{"secret":"available","query":"key","header":"X-Key"}]),
        ),
        (
            "auth",
            json!([{"secret":"available","user":"u","scheme":"Bearer"}]),
        ),
        ("auth", json!([{"secret":"available","header":"Host"}])),
        ("authOptions", Value::Null),
        ("route", json!("/../private")),
        ("method", json!("CONNECT")),
    ] {
        let mut doc = document();
        doc["operations"][0][key] = value;
        invalid.push(doc);
    }
    let mut duplicate = document();
    duplicate["operations"]
        .as_array_mut()
        .unwrap()
        .push(document()["operations"][0].clone());
    invalid.push(duplicate);
    for doc in invalid {
        let secrets = Arc::new(Secrets::default());
        assert!(
            descriptor::read(
                &serde_json::to_vec(&doc).unwrap(),
                None,
                secrets.clone(),
                HttpConfig::default(),
                ENDPOINT
            )
            .is_err(),
            "{doc}"
        );
        assert!(secrets.lookups.lock().unwrap().is_empty());
    }
    for bytes in [
        b"not json".as_slice(),
        br#"{"version":1,"version":1}"#,
        br#"{"a":{"x":1,"x":2}}"#,
    ] {
        assert!(
            descriptor::read(
                bytes,
                None,
                Arc::new(Secrets::default()),
                HttpConfig::default(),
                ENDPOINT
            )
            .is_err()
        );
    }
}

#[test]
fn native_types_validate_unused_definitions_cycles_and_resource_limits() {
    for types in [
        json!({"A":{"base":"Record","fields":{"next":"A"}}}),
        json!({"A":{"base":"B"},"B":{"base":"A"}}),
        json!({"Unused":{"base":"Record","fields":{"x":"Absent"}}}),
        json!({"Invalid":{"base":"Record","fields":{"x":42}}}),
    ] {
        let mut doc = document();
        doc["types"] = types;
        assert!(read(doc).is_err());
    }
    let mut doc = document();
    doc["operations"][0]["responses"]["200"] =
        json!(format!("{}Text{}", "List<".repeat(65), ">".repeat(65)));
    assert!(read(doc).is_err());
    let mut doc = document();
    doc["source"] = json!((0..20_001).map(|_| 0).collect::<Vec<_>>());
    assert!(read(doc).is_err());
    assert!(
        descriptor::read(
            &vec![b' '; 1024 * 1024 + 1],
            None,
            Arc::new(Secrets::default()),
            HttpConfig::default(),
            ENDPOINT
        )
        .is_err()
    );
    // Native contracts share a DAG. Finite response metadata stays an envelope instead
    // of recursively copying every path in a named type graph at import time.
    let mut types = serde_json::Map::new();
    types.insert(
        "T0".into(),
        json!({"base":"Record","fields":{"value":"Text"}}),
    );
    for i in 1..25 {
        types.insert(
            format!("T{i}"),
            json!({"base":"Record","fields":{"a":format!("T{}",i-1),"b":format!("T{}",i-1)}}),
        );
    }
    let mut doc = document();
    doc["types"] = Value::Object(types);
    doc["operations"][0]["responses"]["200"] = json!("T24");
    assert_eq!(
        shape(&capability(&read(doc).unwrap()).result),
        "HttpResponse"
    );
}

#[test]
fn bom_and_inert_nullable_evidence_are_preserved_without_default_coercions() {
    let mut doc = document();
    doc["source"] = json!({"unknown":null});
    let bytes = format!("\u{feff}{doc}\r\n");
    let reading = descriptor::read(
        bytes.as_bytes(),
        None,
        Arc::new(Secrets::default()),
        HttpConfig::default(),
        ENDPOINT,
    )
    .unwrap();
    let Data::Record(info) = reading.description.information().unwrap() else {
        panic!("information")
    };
    let Data::Record(evidence) = &info["evidence"] else {
        panic!("evidence")
    };
    assert_eq!(evidence["unknown"], Data::Option(None));
}

#[tokio::test]
async fn descriptor_import_is_inert_and_explicit_invocation_uses_existing_http_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let secrets = Arc::new(Secrets::default());
    let mut doc = document();
    let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
    doc["operations"][0]["auth"] = json!([{"secret":"available"}]);
    doc["types"] = json!({"Item":{"base":"Record","fields":{"id":"Int","title":"Text"}}});
    doc["operations"][0]["responses"]["200"] = json!("List<Item>");
    doc["operations"][0]["parameters"] = json!([{"name":"category","wire":"category","location":"query","encoding":"scalar","type":"Text","required":false}]);
    let reading = descriptor::read(
        &serde_json::to_vec(&doc).unwrap(),
        None,
        secrets.clone(),
        HttpConfig::default(),
        &endpoint,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).await.unwrap();
            assert!(n > 0 && request.len() < 65536);
            request.extend_from_slice(&bytes[..n]);
        }
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("GET /api/items?category=books HTTP/1.1"));
        assert!(
            text.to_ascii_lowercase()
                .contains("authorization: bearer not-for-metadata")
        );
        let body = r#"[{"id":7,"title":"Example"}]"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let mut runtime = wes_engine::runtime::Runtime::new();
    runtime
        .add(
            (),
            [],
            wes_engine::runtime::ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(std::time::Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            wes_engine::runtime::Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    let call = Call {
        authority: Default::default(),
        run,
        capability: reading.description.capabilities().next().unwrap().clone(),
        arguments: [(
            "category".into(),
            wes_core::Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text("books".into()),
                Default::default(),
            )
            .unwrap(),
        )]
        .into_iter()
        .collect(),
    };
    let value = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reading.invoker.invoke(call, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    let Data::Record(fields) = value.data() else {
        panic!()
    };
    assert!(matches!(&fields["body"], Data::List(items) if items.len() == 1));
    assert_eq!(*secrets.lookups.lock().unwrap(), ["available", "available"]);
    server.await.unwrap();
}

#[test]
fn provider_parameters_project_resolved_bounds_and_enum_into_help_metadata() {
    let mut doc = document();
    doc["types"] = json!({"ApiType3":{"base":"Int","min":1,"max":50},"ApiType4":{"base":"Text","enum":["queued","done"]}});
    doc["operations"][0]["parameters"] = json!([
        {"name":"limit","wire":"limit","location":"query","type":"ApiType3","required":false,"encoding":"scalar"},
        {"name":"status","wire":"status","location":"query","type":"ApiType4","required":false,"encoding":"scalar"}
    ]);
    let reading = read(doc).unwrap();
    let cap = capability(&reading);
    assert_eq!(cap.parameters[0].constraints, ["number: 1..50 (inclusive)"]);
    assert!(cap.parameters[1].constraints[0].contains("\"queued\", \"done\""));
}
