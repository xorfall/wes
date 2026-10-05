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
    };
    json!({"version":manifest["version"],"sdk":manifest["sdk"],"topics":["overview","sdk","theme","layout","examples"],"content":content})
}
