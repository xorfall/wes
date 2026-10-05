//! Canonical help crosses the real engine and codec boundary without a legacy help dialect.
use std::time::Duration;
use wes_adapters::codec::{Limits, decode_value, encode_value};
use wes_core::Data;
use wes_engine::{
    driver::{CancellationToken, Executor},
    runtime::{Effect, Outcome},
    tasks::TaskExecutor,
    workspace::{Preparation, Workspace},
};
use wes_language::{SourceText, parse, vocabulary::commands::COMMAND_PATHS};
#[tokio::test]
async fn all_public_operations_and_short_forms_produce_one_layer_of_composable_help() {
    for entry in COMMAND_PATHS {
        let mut forms = vec![entry.path.join(" ")];
        if let Some(short) = entry.short {
            forms.push(short.into());
        }
        let mut previous = None;
        for form in forms {
            let parsed = parse(&SourceText::new(
                "help",
                format!(":help command:\"{form}\""),
            ));
            assert!(parsed.diagnostics.is_empty());
            let mut workspace =
                Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
            let Preparation::Change(change) =
                workspace.prepare(&parsed.script.statements[0]).unwrap()
            else {
                panic!("declaration")
            };
            workspace.commit(change).unwrap();
            let work = workspace
                .start(Duration::ZERO)
                .into_iter()
                .find_map(|e| match e {
                    Effect::Spawn(t) => Some(t),
                    _ => None,
                })
                .unwrap();
            assert!(workspace.enter(&work.run));
            let Outcome::Produced(value) = TaskExecutor::ephemeral()
                .execute(work, CancellationToken::new())
                .await
                .outcome
            else {
                panic!("help")
            };
            let Data::Record(fields) = value.data() else {
                panic!("record")
            };
            assert_eq!(fields["path"], Data::Text(entry.path.join(" ").into()));
            assert!(matches!(fields["children"], Data::List(_)));
            if entry.path != ["env"] {
                let Data::Record(invocation) = &fields["invocation"] else {
                    panic!("invocation")
                };
                assert_eq!(
                    invocation["command"],
                    Data::Text(format!(":{}", entry.path.join(" ")).into())
                );
                assert!(matches!(invocation["parameters"], Data::List(_)));
                assert!(matches!(invocation["operands"], Data::Record(_)));
            }
            let encoded = encode_value(&value, Limits::default()).unwrap();
            assert_eq!(
                decode_value(&encoded, Limits::default()).unwrap().value,
                value
            );
            assert!(value.provenance().policy().is_empty());
            if let Some(previous) = previous {
                assert_eq!(value, previous);
            }
            previous = Some(value);
        }
    }
}
