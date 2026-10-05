//! The descriptor producer owns its inert documentation contract.
use super::*;
use wes_core::{Primitive, Provenance, RecordShape, Shape, Value};

fn record(name: &str, fields: impl IntoIterator<Item = (&'static str, Shape)>) -> Shape {
    Shape::Record(
        RecordShape::new(name, fields.into_iter().map(|(k, v)| (k.into(), v)))
            .expect("documentation fields are unique"),
    )
}
fn texts() -> Shape {
    Shape::List(Box::new(Shape::Primitive(Primitive::Text)))
}

pub(super) fn typed(mut data: Data) -> Result<Value> {
    // JSON null already decodes to none. Explicit selections must use the native Option
    // representation too, so empty (public) selection remains distinct from no selection.
    let Data::Record(fields) = &mut data else {
        return Err(DescriptorError("invalid information record"));
    };
    let Some(Data::List(operations)) = fields.get_mut("authentication") else {
        return Err(DescriptorError("invalid auth information"));
    };
    for operation in operations {
        let Data::Record(operation) = operation else {
            return Err(DescriptorError("invalid auth information"));
        };
        let selected = operation
            .entry("selected".into())
            .or_insert(Data::Option(None));
        if matches!(selected, Data::List(_)) {
            *selected = Data::Option(Some(Box::new(std::mem::replace(
                selected,
                Data::Option(None),
            ))));
        }
    }
    let text = Shape::Primitive(Primitive::Text);
    let option = record(
        "AuthenticationOption",
        [
            ("schemes", texts()),
            ("credentialSlots", texts()),
            ("methods", texts()),
        ],
    );
    let authentication = record(
        "OperationAuthentication",
        [
            ("operation", texts()),
            ("state", text.clone()),
            ("selected", Shape::Option(Box::new(texts()))),
            ("options", Shape::List(Box::new(option))),
        ],
    );
    let note = record(
        "ProviderNote",
        [
            ("kind", text.clone()),
            ("target", text.clone()),
            ("description", text.clone()),
            ("enforcement", text.clone()),
        ],
    );
    let shape = record(
        "ProviderInformation",
        [
            ("authentication", Shape::List(Box::new(authentication))),
            (
                "safety",
                Shape::List(Box::new(record(
                    "OperationSafety",
                    [
                        ("operation", text.clone()),
                        ("classification", text.clone()),
                        ("basis", text.clone()),
                    ],
                ))),
            ),
            ("notes", Shape::List(Box::new(note))),
            ("advisories", texts()),
            // Provenance is extensible source evidence, not a fixed executable contract.
            ("evidence", Shape::Unknown),
        ],
    );
    Value::new(shape, data, Provenance::default())
        .map_err(|_| DescriptorError("invalid provider information contract"))
}
