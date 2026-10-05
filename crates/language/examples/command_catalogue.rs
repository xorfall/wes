//! Export the actual public language catalogue for documentation and tooling checks.
use wes_language::{
    targets::OBJECT_KINDS,
    vocabulary::{ENVIRONMENT_COMMANDS, ListRegistry, commands},
};
fn main() {
    let operations: Vec<_> = commands::COMMAND_PATHS.iter().map(|entry| {
        let path: Vec<_> = entry.path.iter().map(|s|s.to_string()).collect();
        let spec = commands::signature(&path).expect("public signature");
        serde_json::json!({"path":path,"short":entry.short,"summary":entry.summary,
            "operands":{"min":spec.operands.min,"max":spec.operands.max},
            "parameters":spec.parameters.iter().map(|p|serde_json::json!({"name":p.name,"required":p.required,"type":p.shape.to_string()})).collect::<Vec<_>>()})
    }).collect();
    println!(
        "{}",
        serde_json::json!({"operations":operations,
        "environment":ENVIRONMENT_COMMANDS.iter().map(|c|serde_json::json!({"name":c.name,"name_operand":c.name_operand,"plan_operand":c.plan_operand})).collect::<Vec<_>>(),
        "registries":ListRegistry::ALL.iter().map(|r|serde_json::json!({"name":r.name(),"scope":r.scope().name(),"summary":r.summary()})).collect::<Vec<_>>(),
        "object_selectors":OBJECT_KINDS.iter().map(|(_,name)|name).collect::<Vec<_>>() })
    );
}
