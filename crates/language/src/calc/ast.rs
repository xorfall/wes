use super::{Binary, Package};
use crate::{Name, Span};
use std::sync::Arc;
use wes_core::Data;

pub type ExprId = usize;
pub type StmtId = usize;
pub type FunctionId = usize;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Negate,
    Positive,
    Not,
}
#[derive(Clone, Debug)]
pub enum ExprKind {
    Literal(Data),
    Name(String),
    Workspace(String),
    List(Vec<ExprId>),
    Record(Vec<(String, ExprId)>),
    Unary(Unary, ExprId),
    Binary(Binary, ExprId, ExprId),
    Assign(String, ExprId),
    Index(ExprId, ExprId),
    Field(ExprId, String),
    Call(ExprId, Vec<ExprId>),
    Function(FunctionId),
}
#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}
#[derive(Clone, Debug)]
pub enum StmtKind {
    Block(Vec<StmtId>),
    Binding {
        name: Name,
        mutable: bool,
        value: ExprId,
    },
    Expression(ExprId),
    Return(ExprId),
    If {
        condition: ExprId,
        yes: StmtId,
        no: Option<StmtId>,
    },
    While {
        condition: ExprId,
        body: StmtId,
    },
    For {
        name: Name,
        mutable: bool,
        values: ExprId,
        body: StmtId,
    },
    Break,
    Continue,
}
#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}
#[derive(Clone, Debug)]
pub struct Function {
    pub name: Option<Name>,
    pub parameters: Vec<Name>,
    pub body: StmtId,
    pub span: Span,
}

/// Flat arenas keep deep left-associative expressions safe to clone/drop and traverse iteratively.
#[derive(Clone, Debug)]
pub struct Program {
    pub requires_pure: bool,
    pub source: Arc<str>,
    pub origin: Arc<crate::SourceText>,
    pub origin_offset: usize,
    pub package: Arc<Package>,
    pub span: Span,
    pub expressions: Vec<Expr>,
    pub statements: Vec<Stmt>,
    pub functions: Vec<Function>,
    pub root: StmtId,
}
