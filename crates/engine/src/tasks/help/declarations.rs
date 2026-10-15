use wes_core::Data;

pub(super) fn environment() -> Data {
    let schema = wes_core::package_schema::declarations()
        .describe_root("env")
        .expect("registered environment declaration family");
    data(schema)
}

// Only static schema metadata enters this projection, never declaration input or a user value.
fn data(value: serde_json::Value) -> Data {
    use serde_json::Value;
    match value {
        Value::Null => Data::Option(None),
        Value::Bool(value) => Data::Bool(value),
        Value::Number(value) => Data::Int(value.as_i64().expect("schema metadata integer").into()),
        Value::String(value) => Data::Text(value.into()),
        Value::Array(items) => Data::List(items.into_iter().map(data).collect()),
        Value::Object(fields) => {
            Data::Record(fields.into_iter().map(|(k, v)| (k, data(v))).collect())
        }
    }
}
