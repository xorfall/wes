//! Conservative pure-callback verification over resolved symbols, including helper functions.
use super::*;
use std::collections::BTreeSet;
impl Compiled {
    pub fn pure_callback(&self, root: FunctionId) -> bool {
        let mut functions = vec![root];
        let mut seen = BTreeSet::new();
        let mut expressions = BTreeSet::new();
        let mut locals = BTreeSet::new();
        while let Some(function) = functions.pop() {
            if !seen.insert(function) {
                continue;
            }
            if seen.len() > 128 {
                return false;
            }
            let Some(f) = self.program.functions.get(function) else {
                return false;
            };
            locals.extend(self.functions[function].parameters.iter().copied());
            locals.extend(self.functions[function].own_name);
            let mut statements = vec![f.body];
            let mut pending = vec![];
            while let Some(id) = statements.pop() {
                if let Some(symbol) = self.declarations.get(&id) {
                    locals.insert(*symbol);
                }
                match &self.program.statements[id].kind {
                    StmtKind::Block(xs) => statements.extend(xs),
                    StmtKind::Binding { value, .. }
                    | StmtKind::Expression(value)
                    | StmtKind::Return(value) => pending.push(*value),
                    StmtKind::If { condition, yes, no } => {
                        pending.push(*condition);
                        statements.push(*yes);
                        statements.extend(no);
                    }
                    StmtKind::While { condition, body } => {
                        pending.push(*condition);
                        statements.push(*body);
                    }
                    StmtKind::For { values, body, .. } => {
                        pending.push(*values);
                        statements.push(*body);
                    }
                    _ => {}
                }
            }
            while let Some(id) = pending.pop() {
                if !expressions.insert(id) {
                    continue;
                }
                match &self.program.expressions[id].kind {
                    ExprKind::Function(f) => functions.push(*f),
                    ExprKind::Call(callee, args) => {
                        let operation = match self.resolutions.get(&id) {
                            Some(Resolution::Operation(spec)) => Some(spec.operation),
                            _ => None,
                        };
                        if operation == Some(Operation::Call) {
                            return false;
                        }
                        if matches!(
                            operation,
                            Some(
                                Operation::Map
                                    | Operation::Filter
                                    | Operation::Reduce
                                    | Operation::SortBy
                            )
                        ) {
                            let method = matches!(
                                self.program.expressions[*callee].kind,
                                ExprKind::Field(..)
                            );
                            let Some(callback) = args.get(usize::from(!method)) else {
                                return false;
                            };
                            let Some(f) = self.callback_function(*callback) else {
                                return false;
                            };
                            functions.push(f);
                        }
                        if operation.is_none() {
                            let Some(f) = self.callback_function(*callee) else {
                                return false;
                            };
                            functions.push(f);
                        }
                        pending.push(*callee);
                        pending.extend(args);
                    }
                    ExprKind::List(xs) => pending.extend(xs),
                    ExprKind::Record(xs) => pending.extend(xs.iter().map(|(_, id)| id)),
                    ExprKind::Unary(_, v) | ExprKind::Field(v, _) | ExprKind::Assign(_, v) => {
                        pending.push(*v)
                    }
                    ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
                        pending.push(*a);
                        pending.push(*b);
                    }
                    _ => {}
                }
            }
        }
        for id in expressions {
            if let Some(Resolution::Local(symbol)) = self.resolutions.get(&id) {
                if self.mutable_symbols.contains(symbol) && !locals.contains(symbol) {
                    return false;
                }
                if matches!(self.program.expressions[id].kind, ExprKind::Assign(..))
                    && !locals.contains(symbol)
                {
                    return false;
                }
            }
        }
        true
    }
    pub(super) fn callback_function(&self, mut id: ExprId) -> Option<FunctionId> {
        let mut seen = BTreeSet::new();
        while seen.insert(id) {
            match &self.program.expressions[id].kind {
                ExprKind::Function(f) => return Some(*f),
                ExprKind::Name(_) => {
                    let Resolution::Local(symbol) = self.resolutions.get(&id)? else {
                        return None;
                    };
                    if self.mutable_symbols.contains(symbol) {
                        return None;
                    }
                    if let Some(f) = self
                        .functions
                        .iter()
                        .position(|f| f.own_name == Some(*symbol))
                    {
                        return Some(f);
                    }
                    id = *self.initializers.get(symbol)?;
                }
                _ => return None,
            }
        }
        None
    }
}
