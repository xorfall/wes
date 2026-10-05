//! Definite completion checks. Loops and conditional returns retain runtime checks.
use super::*;

#[derive(Clone, Copy)]
enum Completion {
    Finish,
    Return,
    Unknown,
}
impl Compiled {
    pub(super) fn check_completion(&self) -> Result<(), crate::Diagnostic> {
        fn completion(program: &Program, id: StmtId) -> Completion {
            match &program.statements[id].kind {
                StmtKind::Return(_) => Completion::Return,
                StmtKind::Block(statements) => {
                    let mut result = Completion::Finish;
                    for id in statements {
                        if matches!(result, Completion::Return) {
                            break;
                        }
                        let next = completion(program, *id);
                        result = match (result, next) {
                            (_, Completion::Return) => Completion::Return,
                            (Completion::Unknown, _) | (_, Completion::Unknown) => {
                                Completion::Unknown
                            }
                            _ => Completion::Finish,
                        };
                    }
                    result
                }
                StmtKind::If { condition, yes, no } => {
                    if let ExprKind::Literal(wes_core::Data::Bool(value)) =
                        program.expressions[*condition].kind
                    {
                        return if value {
                            completion(program, *yes)
                        } else {
                            no.map_or(Completion::Finish, |id| completion(program, id))
                        };
                    }
                    match (
                        completion(program, *yes),
                        no.map_or(Completion::Finish, |id| completion(program, id)),
                    ) {
                        (Completion::Finish, Completion::Finish) => Completion::Finish,
                        (Completion::Return, Completion::Return) => Completion::Return,
                        _ => Completion::Unknown,
                    }
                }
                StmtKind::While { .. }
                | StmtKind::For { .. }
                | StmtKind::Break
                | StmtKind::Continue => Completion::Unknown,
                _ => Completion::Finish,
            }
        }
        for (body, span) in std::iter::once((self.program.root, self.program.span))
            .chain(self.program.functions.iter().map(|f| (f.body, f.span)))
        {
            if matches!(completion(&self.program, body), Completion::Finish) {
                return Err(crate::Diagnostic::error(
                    "CAL013",
                    span,
                    "calculation/function reaches its end without returning a value; add return",
                ));
            }
        }
        Ok(())
    }
}
