//! Capture statically selected workspace fields before VM entry. No storage port enters the VM.
use crate::plan::Input;
use indexmap::IndexMap;
use std::{collections::BTreeSet, sync::Arc};
use wes_language::calc::{Binary, Compiled, ExprKind, Program, Resolution, StmtKind, Unary};

pub(super) fn capture(
    compiled: Arc<Compiled>,
    mut inputs: IndexMap<String, Input>,
) -> (Arc<Compiled>, IndexMap<String, Input>) {
    let mut lowered = (*compiled).clone();
    let mut program = (*compiled.program).clone();
    let live = reachable(&compiled, &program);
    let callees = program
        .expressions
        .iter()
        .filter_map(|e| match e.kind {
            ExprKind::Call(id, _) => Some(id),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for (id, expr) in compiled.program.expressions.iter().enumerate() {
        if !live.contains(&id) || !matches!(expr.kind, ExprKind::Field(..)) || callees.contains(&id)
        {
            continue;
        }
        let mut base = id;
        let mut path = Vec::new();
        while let ExprKind::Field(parent, name) = &compiled.program.expressions[base].kind {
            path.push(name.clone());
            base = *parent;
        }
        let ExprKind::Workspace(name) = &compiled.program.expressions[base].kind else {
            continue;
        };
        let Some(input) = inputs.get(name).cloned() else {
            continue;
        };
        let Input::FromNode(output) = input else {
            continue;
        };
        path.reverse();
        let mut slot = format!("@captured-field:{id}");
        while inputs.contains_key(&slot) {
            slot.push('@');
        }
        inputs.insert(
            slot.clone(),
            Input::FieldPath {
                output,
                fields: path,
            },
        );
        lowered
            .workspace
            .insert(slot.clone(), compiled.shapes[id].clone());
        program.expressions[id].kind = ExprKind::Workspace(slot);
    }
    // Arena identities/spans remain stable. An unreachable node is not a payload request.
    let used = reachable(&compiled, &program)
        .into_iter()
        .filter_map(|id| {
            if let ExprKind::Workspace(name) = &program.expressions[id].kind {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect::<BTreeSet<_>>();
    inputs.retain(|name, _| used.contains(name));
    lowered.workspace.retain(|name, _| used.contains(name));
    lowered.program = Arc::new(program);
    (Arc::new(lowered), inputs)
}

/// Only syntactically proven dead execution is omitted. Unknown branches and escaped functions
/// are conservative. Source identity/policy dependencies are retained separately by the host.
fn reachable(compiled: &Compiled, program: &Program) -> BTreeSet<usize> {
    // Pipeline construction can place parents before children in the arena. Use iterative
    // postorder rather than relying on parser insertion order or the native call stack.
    let mut booleans: Vec<Option<bool>> = vec![None; program.expressions.len()];
    let mut seen = vec![false; program.expressions.len()];
    for root in 0..program.expressions.len() {
        let mut pending = vec![(root, false)];
        while let Some((id, finish)) = pending.pop() {
            if finish {
                booleans[id] = match &program.expressions[id].kind {
                    ExprKind::Literal(wes_core::Data::Bool(value)) => Some(*value),
                    ExprKind::Unary(Unary::Not, value) => booleans[*value].map(|v| !v),
                    ExprKind::Binary(Binary::And, a, b) => match booleans[*a] {
                        Some(false) => Some(false),
                        Some(true) => booleans[*b],
                        None => None,
                    },
                    ExprKind::Binary(Binary::Or, a, b) => match booleans[*a] {
                        Some(true) => Some(true),
                        Some(false) => booleans[*b],
                        None => None,
                    },
                    _ => None,
                };
                continue;
            }
            if std::mem::replace(&mut seen[id], true) {
                continue;
            }
            pending.push((id, true));
            match &program.expressions[id].kind {
                ExprKind::Unary(Unary::Not, value) => pending.push((*value, false)),
                ExprKind::Binary(Binary::And | Binary::Or, a, b) => {
                    pending.push((*a, false));
                    pending.push((*b, false));
                }
                _ => {}
            }
        }
    }
    let mut falls = vec![true; program.statements.len()];
    let mut seen = vec![false; program.statements.len()];
    for root in 0..program.statements.len() {
        let mut pending = vec![(root, false)];
        while let Some((id, finish)) = pending.pop() {
            if finish {
                falls[id] = match &program.statements[id].kind {
                    StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue => false,
                    StmtKind::Block(body) => body.iter().all(|id| falls[*id]),
                    StmtKind::If { condition, yes, no } => match booleans[*condition] {
                        Some(true) => falls[*yes],
                        Some(false) => no.is_none_or(|id| falls[id]),
                        None => falls[*yes] || no.is_none_or(|id| falls[id]),
                    },
                    _ => true,
                };
                continue;
            }
            if std::mem::replace(&mut seen[id], true) {
                continue;
            }
            pending.push((id, true));
            match &program.statements[id].kind {
                StmtKind::Block(body) => pending.extend(body.iter().map(|id| (*id, false))),
                StmtKind::If { yes, no, .. } => {
                    pending.push((*yes, false));
                    pending.extend(no.iter().map(|id| (*id, false)));
                }
                _ => {}
            }
        }
    }
    let mut functions = std::collections::BTreeMap::new();
    let mut bound_functions = BTreeSet::new();
    for (id, statement) in program.statements.iter().enumerate() {
        if let StmtKind::Binding { value, .. } = &statement.kind
            && let ExprKind::Function(function) = program.expressions[*value].kind
        {
            functions.insert(compiled.declarations[&id], program.functions[function].body);
            bound_functions.insert(*value);
        }
    }
    enum Work {
        Statement(usize),
        Expression(usize),
    }
    let mut pending = vec![Work::Statement(program.root)];
    let mut expressions = BTreeSet::new();
    let mut statements = BTreeSet::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Statement(id) => {
                if !statements.insert(id) {
                    continue;
                }
                match &program.statements[id].kind {
                    StmtKind::Block(body) => {
                        for id in body {
                            pending.push(Work::Statement(*id));
                            if !falls[*id] {
                                break;
                            }
                        }
                    }
                    StmtKind::Binding { value, .. }
                    | StmtKind::Expression(value)
                    | StmtKind::Return(value) => pending.push(Work::Expression(*value)),
                    StmtKind::If { condition, yes, no } => {
                        pending.push(Work::Expression(*condition));
                        if booleans[*condition] != Some(false) {
                            pending.push(Work::Statement(*yes));
                        }
                        if booleans[*condition] != Some(true)
                            && let Some(no) = no
                        {
                            pending.push(Work::Statement(*no));
                        }
                    }
                    StmtKind::While { condition, body } => {
                        pending.push(Work::Expression(*condition));
                        if booleans[*condition] != Some(false) {
                            pending.push(Work::Statement(*body));
                        }
                    }
                    StmtKind::For { values, body, .. } => {
                        pending.push(Work::Expression(*values));
                        pending.push(Work::Statement(*body));
                    }
                    _ => {}
                }
            }
            Work::Expression(id) => {
                if !expressions.insert(id) {
                    continue;
                }
                match &program.expressions[id].kind {
                    ExprKind::Name(_) => {
                        if let Some(Resolution::Local(symbol)) = compiled.resolutions.get(&id)
                            && let Some(body) = functions.get(symbol)
                        {
                            pending.push(Work::Statement(*body));
                        }
                    }
                    ExprKind::Function(function) if !bound_functions.contains(&id) => {
                        pending.push(Work::Statement(program.functions[*function].body))
                    }
                    ExprKind::List(xs) => pending.extend(xs.iter().map(|id| Work::Expression(*id))),
                    ExprKind::Record(xs) => {
                        pending.extend(xs.iter().map(|(_, id)| Work::Expression(*id)))
                    }
                    ExprKind::Unary(_, v) | ExprKind::Assign(_, v) | ExprKind::Field(v, _) => {
                        pending.push(Work::Expression(*v))
                    }
                    ExprKind::Binary(op, a, b) => {
                        pending.push(Work::Expression(*a));
                        if !matches!(
                            (op, booleans[*a]),
                            (Binary::And, Some(false)) | (Binary::Or, Some(true))
                        ) {
                            pending.push(Work::Expression(*b));
                        }
                    }
                    ExprKind::Index(a, b) => {
                        pending.push(Work::Expression(*a));
                        pending.push(Work::Expression(*b));
                    }
                    ExprKind::Call(callee, args) => {
                        pending.push(Work::Expression(*callee));
                        pending.extend(args.iter().map(|id| Work::Expression(*id)));
                    }
                    _ => {}
                }
            }
        }
    }
    expressions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        graph::OutputRef,
        runtime::{ExecutionTraits, Runtime},
    };
    use wes_core::{Shape, capability::Catalogue, contracts::ContractRegistry};
    use wes_language::calc::{self, Package};

    fn selected(body: &str) -> Vec<Vec<String>> {
        let compiled = calc::analyze(
            Arc::new(calc::parse_body(body, 0, Package::standard()).unwrap()),
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| Some(Shape::Unknown),
            },
        )
        .unwrap();
        let node = Runtime::new()
            .add(
                (),
                [],
                ExecutionTraits {
                    pure: true,
                    repeatable: true,
                    bounded: true,
                },
            )
            .unwrap();
        let (_, inputs) = capture(
            Arc::new(compiled),
            IndexMap::from([("p".into(), Input::FromNode(OutputRef::data(node)))]),
        );
        let mut paths = BTreeSet::new();
        for input in inputs.values() {
            if let Input::FieldPath { fields, .. } = input {
                paths.insert(fields.clone());
            }
            if matches!(input, Input::FromNode(_)) {
                paths.insert(vec![]);
            }
        }
        paths.into_iter().collect()
    }

    #[test]
    fn unreachable_branches_short_circuits_and_unused_functions_do_not_capture_payloads() {
        for body in [
            "if(false) { return $p.stdout; } return $p.exitCode;",
            "while(false) { const x=$p.stdout; } return $p.exitCode;",
            "const x=false && $p.stdout==$p.stdout; return $p.exitCode;",
            "const x=true || $p.stdout==$p.stdout; return $p.exitCode;",
            "const f=()=> $p.stdout; return $p.exitCode;",
            "function f(){ return $p.stdout; } return $p.exitCode;",
            "return $p.exitCode; const x=$p.stdout;",
        ] {
            assert_eq!(selected(body), vec![vec!["exitCode".to_owned()]], "{body}");
        }
    }

    #[test]
    fn called_or_escaped_functions_and_unknown_branches_keep_their_captures() {
        for body in [
            "const f=()=> $p.stdout; f(); return $p.exitCode;",
            "function f(){ return $p.stdout; } f(); return $p.exitCode;",
            "const f=()=> $p.stdout; const g=()=>f(); g(); return $p.exitCode;",
            "if($p.exitCode==0) { return $p.stdout; } return $p.exitCode;",
        ] {
            assert_eq!(
                selected(body),
                vec![vec!["exitCode".to_owned()], vec!["stdout".to_owned()]],
                "{body}"
            );
        }
        assert_eq!(
            selected("return $p.inner.exitCode;"),
            vec![vec!["inner".to_owned(), "exitCode".to_owned()]]
        );
        assert_eq!(
            selected("const x=$p; return $p.exitCode;"),
            vec![vec![], vec!["exitCode".to_owned()]]
        );
    }
}
