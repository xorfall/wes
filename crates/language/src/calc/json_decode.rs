use wes_core::{Primitive, RecordShape, Shape};

/// The diagnostic decoder's declaration and runtime share this structural contract.
/// The outer Option distinguishes a successfully decoded JSON null from failure.
pub fn json_decode_shape(value: Shape) -> Shape {
    let text = Shape::Primitive(Primitive::Text);
    let position = Shape::Option(Box::new(Shape::Primitive(Primitive::Int)));
    let error = Shape::Record(
        RecordShape::new(
            "",
            [
                ("code".into(), text.clone()),
                ("message".into(), text),
                ("line".into(), position.clone()),
                ("column".into(), position),
            ],
        )
        .expect("distinct diagnostic fields"),
    );
    Shape::Record(
        RecordShape::new(
            "",
            [
                ("ok".into(), Shape::Primitive(Primitive::Bool)),
                ("value".into(), Shape::Option(Box::new(value))),
                ("error".into(), Shape::Option(Box::new(error))),
            ],
        )
        .expect("distinct decode result fields"),
    )
}
