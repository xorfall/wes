//! Contract alternatives are inert; only an environment selects an AND requirement.
use super::*;
use std::collections::BTreeSet;

pub(super) struct Selection {
    pub chosen: Option<Auth>,
    pub alternatives: Vec<Option<Auth>>,
    pub information: serde_json::Value,
}
fn parts(data: &Data) -> Result<Option<Auth>> {
    match data {
        Data::List(v) if v.is_empty() => Ok(None),
        Data::List(_) => read_auth(data).map(Some),
        _ => Err(DescriptorError("auth must be an array")),
    }
}
fn schemes(data: &Data) -> Result<Vec<String>> {
    let Data::List(items) = data else {
        return Err(DescriptorError("auth schemes must be an array"));
    };
    if items.len() > 32 {
        return Err(DescriptorError("too many auth schemes"));
    }
    let mut result = BTreeSet::new();
    for item in items {
        let name = as_text(item)?;
        if name.is_empty()
            || name.len() > 256
            || name.chars().any(char::is_control)
            || !result.insert(name.to_owned())
        {
            return Err(DescriptorError(
                "invalid or duplicate authentication scheme",
            ));
        }
    }
    Ok(result.into_iter().collect())
}
fn metadata(data: &Data, names: &[String]) -> Result<serde_json::Value> {
    let Data::List(items) = data else {
        return Err(DescriptorError("auth must be an array"));
    };
    let mut slots = BTreeSet::new();
    let mut methods = Vec::new();
    for item in items {
        let fields = object(item)?;
        slots.insert(text(fields, "secret")?.to_owned());
        if let Some(name) = optional_text(fields, "userSecret")? {
            slots.insert(name.to_owned());
        }
        methods.push(
            if fields.contains_key("user") || fields.contains_key("userSecret") {
                "basic"
            } else if fields.contains_key("query") {
                "query"
            } else {
                "header"
            },
        );
    }
    Ok(serde_json::json!({"schemes":names,"credentialSlots":slots,"methods":methods}))
}
pub(super) fn select(
    raw: &Data,
    path: &[String],
    selected: Option<&Vec<String>>,
) -> Result<Selection> {
    let fields = object(raw)?;
    let fixed = required(fields, "auth")?;
    let Some(options) = fields.get("authOptions") else {
        if selected.is_some() {
            return Err(DescriptorError(
                "auth choice supplied for an operation without alternatives",
            ));
        }
        return Ok(Selection {
            chosen: parts(fixed)?,
            alternatives: vec![],
            information: serde_json::json!({"operation":path,"state":"fixed","options":[metadata(fixed,&[])?]}),
        });
    };
    if !matches!(fixed,Data::List(v) if v.is_empty()) {
        return Err(DescriptorError(
            "authOptions cannot coexist with fixed authentication",
        ));
    }
    let Data::List(options) = options else {
        return Err(DescriptorError("authOptions must be an array"));
    };
    if options.is_empty() || options.len() > 32 {
        return Err(DescriptorError("expected 1 to 32 auth options"));
    }
    let selected = selected
        .map(|names| {
            schemes(&Data::List(
                names.iter().map(|n| Data::Text(n.clone().into())).collect(),
            ))
        })
        .transpose()?;
    let mut seen = BTreeSet::new();
    let mut alternatives = vec![];
    let mut info = vec![];
    let mut chosen = None;
    let mut chosen_names = None;
    for option in options {
        let option = object(option)?;
        known(option, &["schemes", "auth"])?;
        let names = schemes(required(option, "schemes")?)?;
        if !seen.insert(names.clone()) {
            return Err(DescriptorError("duplicate auth option"));
        }
        let raw_auth = required(option, "auth")?;
        let auth = parts(raw_auth)?;
        if names.is_empty() != auth.is_none() {
            return Err(DescriptorError(
                "anonymous auth option must have no credentials",
            ));
        }
        if selected.as_ref() == Some(&names) || (selected.is_none() && options.len() == 1) {
            chosen = Some(auth.clone());
            chosen_names = Some(names.clone());
        }
        info.push(metadata(raw_auth, &names)?);
        alternatives.push(auth);
    }
    if selected.is_some() && chosen.is_none() {
        return Err(DescriptorError(
            "environment auth choice is not supported by this operation",
        ));
    }
    let state = if chosen.is_some() {
        "selected"
    } else {
        "selection-required"
    };
    Ok(Selection {
        chosen: chosen.unwrap_or(Some(Auth::Unselected)),
        alternatives,
        information: serde_json::json!({"operation":path,"state":state,"selected":chosen_names,"options":info}),
    })
}
