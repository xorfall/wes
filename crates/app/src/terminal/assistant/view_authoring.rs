//! Readonly product context. No workspace values, file access or compiler execution.
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Topic {
    #[default]
    Overview,
    Sdk,
    Theme,
    Layout,
    Examples,
    Types,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Read {
    #[serde(default)]
    topic: Topic,
}

pub(super) fn read(input: Read) -> Value {
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../packages/view-sdk/authoring.json"
    ))
    .expect("authoring manifest");
    let content = match input.topic {
        Topic::Overview => manifest.clone(),
        Topic::Theme => {
            serde_json::from_str(include_str!("../../../../../packages/view-sdk/theme.json"))
                .expect("theme registry")
        }
        Topic::Sdk => json!({
            "declarations": include_str!("../../../../../packages/view-sdk/index.ts"),
            "time": include_str!("../../../../../packages/view-sdk/time.ts"),
            "numbers": include_str!("../../../../../packages/view-sdk/values.ts"),
            "contracts": include_str!("../../../../../packages/view-sdk/contract.ts"),
            "commands": include_str!("../../../../../packages/view-sdk/commands.ts"),
            "evidence": include_str!("../../../../../packages/view-sdk/evidence.ts"),
            "datasets": include_str!("../../../../../packages/view-sdk/datasets.ts"),
            "generatedTypes": "Import definition and Input/Outputs/State/Event/EventOutputs from ./contract. The compiler generates them from validated view.json and types.yaml.",
            "runtimeLimits": manifest["runtimeLimits"],
            "layout": manifest["layout"],
        }),
        Topic::Layout => json!({
            "rules": manifest["layout"],
            "declarations": include_str!("../../../../../packages/view-sdk/contract.ts"),
            "builtinLayouts": wes_views::catalogue().iter().map(|package| json!({
                "name": package.manifest.name,
                "layout": package.description()["layout"],
            })).collect::<Vec<_>>(),
        }),
        Topic::Examples => {
            json!({"name":"TaskBoard","purpose":"Minimal keyboard-accessible selection with explicit state/output contracts. Use domain-specific input constraints for your View.","files":{
                "view.json": include_str!("../../../../../tools/view-package/template/view.json"),
                "types.yaml": include_str!("../../../../../tools/view-package/template/types.yaml"),
                "View.tsx": include_str!("../../../../../tools/view-package/template/View.tsx"),
                "view.css": include_str!("../../../../../tools/view-package/template/view.css"),
            }})
        }
        Topic::Types => json!({
            "inputRule":"view.json.input must resolve to a named Record contract. Wrap a list/scalar in a Record field; field links address these named fields. State is also a Record. View boundary contracts must be finite; unsupported lazy/management shapes need an explicit adapter.",
            "builtins":wes_core::contracts::ContractRegistry::new().snapshot().keys().collect::<Vec<_>>(),
            "schema":crate::web::language::published_yaml(),
            "viewBoundary":"Supported View port shapes: finite scalar, Record, List, Option and Union. Map, Iter, Unknown, management values and pattern-constrained contracts require an explicit adapter; the general type-package schema includes shapes not permitted at a View boundary.",
            "syntax":"types.yaml is a Wes type package, not JSON Schema. Define names under types; a definition uses base plus fields/constraints. Field types are type expressions; names and constructors are case-sensitive. List<T>, Option<T>, Map<Text,T>, Union<A,B> are constructors, not YAML collections.",
            "example":"types:\n  CityReading:\n    base: Record\n    fields:\n      time: Instant\n      temperature: Decimal\n  ForecastInput:\n    base: Record\n    fields:\n      readings: List<CityReading>\n",
            "manifestInput":"ForecastInput",
            "valueAdapter":":calc pure { return {readings:$readings}; } > forecastInput",
            "time":"Instant requires a date, T, hour:minute:second and an explicit Z or numeric UTC offset; it never guesses the machine timezone. Example: 2026-10-06T12:00:00+03:00. A timezone-less API string remains Text until explicitly adapted with known timezone information. Epoch inputs use fromEpochSeconds/Millis/Nanos with explicit units.",
        }),
    };
    json!({"version":manifest["version"],"sdk":manifest["sdk"],"topics":["overview","sdk","theme","layout","examples","types"],"content":content})
}
