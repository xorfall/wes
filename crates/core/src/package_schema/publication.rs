use serde_json::{Value, json};

impl super::Registry {
    /// The same schema descriptors consumed by authoring tools and command help.
    pub fn describe(&self) -> Value {
        let registry = self;
        use super::{Kind, ScalarType};
        let definitions = registry.definitions.iter().map(|(name, schema)| {
        let mut value = match &schema.kind {
            Kind::Any => json!({"kind":"any"}),
            Kind::Scalar { scalar, choices } => {
                let name = match scalar { ScalarType::Text => "Text", ScalarType::Int => "Int", ScalarType::Decimal => "Decimal", ScalarType::Bool => "Bool", ScalarType::TypeExpression => "TypeExpression" };
                let mut value = json!({"kind":"scalar", "type":name});
                if !choices.is_empty() { value["choices"] = json!(choices); }
                value
            }
            Kind::Object { fields, exclusive, conditions } => {
                let fields = fields.iter().map(|(name, field)| ((*name).to_owned(), json!({"schema":field.schema,"required":field.required}))).collect::<serde_json::Map<_, _>>();
                let mut value = json!({"kind":"object", "fields":fields});
                if !exclusive.is_empty() { value["exclusive"] = json!(exclusive.iter().map(|group| json!({"fields":group.fields,"min":group.min,"max":group.max})).collect::<Vec<_>>()); }
                if !conditions.is_empty() { value["conditions"] = json!(conditions.iter().map(|condition| {
                    let mut value = json!({"field":condition.field,"values":condition.values,"allowed":condition.allowed});
                    if let Some(prefix) = condition.prefix { value["prefix"] = json!(prefix); }
                    if !condition.required.is_empty() { value["required"] = json!(condition.required); }
                    value
                }).collect::<Vec<_>>()); }
                value
            }
            Kind::Map { values } => json!({"kind":"map", "values":values}),
            Kind::List { items } => json!({"kind":"list", "items":items}),
            Kind::Union { variants } => json!({"kind":"union", "variants":variants}),
            Kind::Discriminated { field, variants } => json!({"kind":"discriminated", "field":field,"variants":variants.iter().map(|(key,value)| ((*key).to_owned(),json!(value))).collect::<serde_json::Map<_,_>>()}),
        };
        if let Some(hint) = schema.hint { value["hint"] = json!(hint); }
        if !schema.constraints.is_empty() { value["constraints"] = json!(schema.constraints); }
        ((*name).to_owned(), value)
    }).collect::<serde_json::Map<_,_>>();
        json!({
            "language":"yaml", "version":1,
            "roots":registry.roots.iter().map(|(name,schema)| ((*name).to_owned(),json!(schema))).collect::<serde_json::Map<_,_>>(),
            "definitions":definitions,
            "typeConstructors":crate::contracts::TYPE_CONSTRUCTORS.iter().map(|spec| json!({"name":spec.name,"parameters":spec.parameters})).collect::<Vec<_>>(),
            "constraints":registry.constraints.iter().map(|(name,description)| ((*name).to_owned(),json!({"description":description,"validation":"semantic"}))).collect::<serde_json::Map<_,_>>(),
        })
    }
}
