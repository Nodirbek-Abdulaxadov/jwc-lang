//! v1 AST. One node per grammar production; nothing from the pre-1.0 tree is
//! reused (`crate::ast` describes a different language).
//!
//! Every declaration carries `docs` (from `---` doc comments, which become
//! `COMMENT ON` — schema.md §7) and `comments` (from `--` line comments,
//! which exist so `jwc v1 fmt` does not delete them).

use crate::token::Span;

#[derive(Clone, Debug, Default)]
pub struct Attached {
    pub docs: Vec<String>,
    pub comments: Vec<String>,
    /// A blank line preceded this item in the source. `fmt` reproduces it.
    pub blank_before: bool,
}

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

impl Ident {
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
        }
    }
}

/// `a.b.c`
#[derive(Clone, Debug)]
pub struct DottedName {
    pub parts: Vec<Ident>,
    pub span: Span,
}

impl DottedName {
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

// ---------------------------------------------------------------- program

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub decls: Vec<Decl>,
}

#[derive(Clone, Debug)]
pub enum Decl {
    Namespace(NamespaceDecl),
    Import(ImportDecl),
    Database(DatabaseDecl),
    Schema(SchemaDecl),
    Table(TableDecl),
    View(ViewDecl),
    Enum(EnumDecl),
    Class(ClassDecl),
    Error(ErrorDecl),
    Service(ServiceDecl),
    Middleware(MiddlewareDecl),
    Routes(RoutesDecl),
    ErrorHandler(ErrorHandlerDecl),
    Server(ServerDecl),
    Function(FunctionDecl),
    Test(TestDecl),
}

impl Decl {
    pub fn span(&self) -> Span {
        match self {
            Decl::Namespace(d) => d.span,
            Decl::Import(d) => d.span,
            Decl::Database(d) => d.span,
            Decl::Schema(d) => d.span,
            Decl::Table(d) => d.span,
            Decl::View(d) => d.span,
            Decl::Enum(d) => d.span,
            Decl::Class(d) => d.span,
            Decl::Error(d) => d.span,
            Decl::Service(d) => d.span,
            Decl::Middleware(d) => d.span,
            Decl::Routes(d) => d.span,
            Decl::ErrorHandler(d) => d.span,
            Decl::Server(d) => d.span,
            Decl::Function(d) => d.span,
            Decl::Test(d) => d.span,
        }
    }

    pub fn attached(&self) -> &Attached {
        match self {
            Decl::Namespace(d) => &d.at,
            Decl::Import(d) => &d.at,
            Decl::Database(d) => &d.at,
            Decl::Schema(d) => &d.at,
            Decl::Table(d) => &d.at,
            Decl::View(d) => &d.at,
            Decl::Enum(d) => &d.at,
            Decl::Class(d) => &d.at,
            Decl::Error(d) => &d.at,
            Decl::Service(d) => &d.at,
            Decl::Middleware(d) => &d.at,
            Decl::Routes(d) => &d.at,
            Decl::ErrorHandler(d) => &d.at,
            Decl::Server(d) => &d.at,
            Decl::Function(d) => &d.at,
            Decl::Test(d) => &d.at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NamespaceDecl {
    pub at: Attached,
    pub name: DottedName,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub at: Attached,
    pub name: DottedName,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct DatabaseDecl {
    pub at: Attached,
    pub name: Ident,
    pub driver: Ident,
    pub init: Vec<Assignment>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Assignment {
    pub key: Ident,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct SchemaDecl {
    pub at: Attached,
    pub name: Ident,
    pub database: Ident,
    pub physical: Option<String>,
    pub span: Span,
}

// ---------------------------------------------------------------- table

#[derive(Clone, Debug)]
pub struct TableDecl {
    pub at: Attached,
    pub name: Ident,
    /// `App.auth`
    pub schema: QualifiedSchema,
    pub physical: Option<String>,
    pub was: Option<String>,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub indexes: Vec<IndexDef>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct QualifiedSchema {
    pub database: Ident,
    pub schema: Ident,
    pub span: Span,
}

/// `App.auth.Accounts`
#[derive(Clone, Debug)]
pub struct QualifiedTable {
    pub database: Ident,
    pub schema: Ident,
    pub object: Ident,
    pub span: Span,
}

impl QualifiedTable {
    pub fn text(&self) -> String {
        format!(
            "{}.{}.{}",
            self.database.name, self.schema.name, self.object.name
        )
    }
}

#[derive(Clone, Debug)]
pub struct ColumnDef {
    pub at: Attached,
    pub name: Ident,
    pub ty: TypeRef,
    pub modifiers: Vec<ColumnModifier>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ColumnModifier {
    PrimaryKey(Span),
    Identity(Span),
    Unique {
        message: Option<String>,
        span: Span,
    },
    Private(Span),
    Server(Span),
    Default(Expr, Span),
    OnUpdate(Expr, Span),
    Physical(String, Span),
    Was(String, Span),
    /// `minLength(2)`, `pattern(r"…")`, `required`, `transient`, …
    Rule(RuleCall),
}

#[derive(Clone, Debug)]
pub struct RuleCall {
    pub name: Ident,
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TableConstraint {
    PrimaryKey {
        columns: Vec<Ident>,
        span: Span,
    },
    ForeignKey {
        columns: Vec<Ident>,
        target: QualifiedTable,
        target_columns: Vec<Ident>,
        on_delete: Option<RefAction>,
        on_update: Option<RefAction>,
        span: Span,
    },
    Unique {
        columns: Vec<Ident>,
        predicate: Option<Expr>,
        message: Option<String>,
        span: Span,
    },
    Check {
        expr: Expr,
        message: Option<String>,
        span: Span,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefAction {
    Cascade,
    Restrict,
    NoAction,
    SetNull,
    SetDefault,
}

impl RefAction {
    pub fn as_sql(self) -> &'static str {
        match self {
            RefAction::Cascade => "CASCADE",
            RefAction::Restrict => "RESTRICT",
            RefAction::NoAction => "NO ACTION",
            RefAction::SetNull => "SET NULL",
            RefAction::SetDefault => "SET DEFAULT",
        }
    }
}

#[derive(Clone, Debug)]
pub struct IndexDef {
    pub at: Attached,
    pub columns: Vec<IndexColumn>,
    pub predicate: Option<Expr>,
    pub method: Option<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct IndexColumn {
    pub name: Ident,
    pub desc: bool,
    pub nulls: Option<NullsOrder>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NullsOrder {
    First,
    Last,
}

// ---------------------------------------------------------------- enum, view, class

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub at: Attached,
    pub name: Ident,
    /// `None` = varchar + CHECK; `Some` = a real `CREATE TYPE` (schema.md §5).
    pub schema: Option<QualifiedSchema>,
    pub physical: Option<String>,
    pub members: Vec<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ViewDecl {
    pub at: Attached,
    pub name: Ident,
    pub schema: QualifiedSchema,
    pub physical: Option<String>,
    pub body: Box<SelectExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ClassDecl {
    pub at: Attached,
    pub name: Ident,
    pub fields: Vec<ClassField>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ClassField {
    pub at: Attached,
    pub name: Ident,
    pub ty: TypeRef,
    pub rules: Vec<RuleCall>,
    pub transient: bool,
    pub span: Span,
}

// ---------------------------------------------------------------- error, service, middleware

#[derive(Clone, Debug)]
pub struct ErrorDecl {
    pub at: Attached,
    pub name: Ident,
    pub params: Vec<Param>,
    pub status: u16,
    pub message: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ServiceDecl {
    pub at: Attached,
    pub name: Ident,
    pub functions: Vec<FunctionDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FunctionDecl {
    pub at: Attached,
    pub name: Ident,
    pub params: Vec<Param>,
    pub returns: Option<TypeRef>,
    pub raises: Vec<Ident>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Ident,
    pub ty: TypeRef,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MiddlewareDecl {
    pub at: Attached,
    pub name: Ident,
    /// `(@org_id: bigint)` — declared path-parameter dependencies.
    pub binders: Vec<Binder>,
    /// `requires RequireAuth`
    pub requires: Vec<Ident>,
    /// `provides account_id: bigint`
    pub provides: Vec<CtxDecl>,
    pub body: Block,
    pub after: Option<Block>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Binder {
    pub name: Ident,
    pub ty: TypeRef,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct CtxDecl {
    pub name: Ident,
    pub ty: TypeRef,
    pub span: Span,
}

// ---------------------------------------------------------------- routing

#[derive(Clone, Debug)]
pub struct RoutesDecl {
    pub at: Attached,
    pub prefix: String,
    pub prefix_span: Span,
    pub uses: Vec<Ident>,
    pub routes: Vec<RouteDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct RouteDecl {
    pub at: Attached,
    pub method: Ident,
    pub suffix: String,
    pub suffix_span: Span,
    pub uses: Vec<Ident>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ErrorHandlerDecl {
    pub at: Attached,
    pub binder: Ident,
    pub arms: Vec<CatchArm>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct CatchArm {
    /// `None` = the untyped arm, which catches faults only (errors.md §4.4).
    pub error: Option<Ident>,
    pub binder: Ident,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ServerDecl {
    pub at: Attached,
    pub entries: Vec<ServerEntry>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ServerEntry {
    Set(Assignment),
    Group {
        name: Ident,
        entries: Vec<Assignment>,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct TestDecl {
    pub at: Attached,
    pub name: String,
    pub body: Block,
    pub span: Span,
}

// ---------------------------------------------------------------- types

#[derive(Clone, Debug)]
pub struct TypeRef {
    pub kind: TypeKind,
    /// Number of `[]` suffixes.
    pub array_depth: u8,
    /// `?` on the base type.
    pub optional: bool,
    /// `?` on each array level, outermost last.
    pub array_optional: Vec<bool>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    /// `bigint`, `varchar(255)`, `numeric(14, 2)`, …
    Scalar { name: String, args: Vec<u32> },
    /// `{ status: text }` — types.md §1.
    Record(Vec<(Ident, TypeRef)>),
    /// A declared name: enum, class, view.
    Named(DottedName),
}

// ---------------------------------------------------------------- statements

pub type Block = Vec<Stmt>;

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        at: Attached,
        name: Ident,
        ty: Option<TypeRef>,
        value: Expr,
        span: Span,
    },
    Assign {
        at: Attached,
        target: AssignTarget,
        value: Expr,
        span: Span,
    },
    If {
        at: Attached,
        cond: Expr,
        then: Block,
        otherwise: Option<Block>,
        span: Span,
    },
    For {
        at: Attached,
        binder: Ident,
        iterable: Expr,
        body: Block,
        span: Span,
    },
    Return {
        at: Attached,
        value: Option<Expr>,
        span: Span,
    },
    Throw {
        at: Attached,
        error: Ident,
        args: Vec<Expr>,
        span: Span,
    },
    Transaction {
        at: Attached,
        body: Block,
        span: Span,
    },
    Assert {
        at: Attached,
        kind: AssertKind,
        span: Span,
    },
    Expr {
        at: Attached,
        expr: Expr,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub enum AssignTarget {
    /// `$x = …`
    Local(Ident),
    /// `context.k = …`
    Context(Ident),
}

#[derive(Clone, Debug)]
pub enum AssertKind {
    Expr(Expr),
    Fails {
        /// Mandatory since v0.28.0: an untyped `assert fails` passes when a
        /// typo makes the block raise something unrelated (testing.md §4.1).
        error: Option<Ident>,
        body: Block,
        /// `with "…"` — the raised error's message, compared exactly
        /// (testing.md §4.2).
        message: Option<String>,
        message_span: Option<Span>,
    },
}

// ---------------------------------------------------------------- expressions

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: Box<ExprKind>,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self {
            kind: Box::new(kind),
            span,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(String),
    Decimal(String),
    Str(String),
    RawStr(String),
    Bool(bool),
    Null,

    /// A bare name: a column inside a query clause, a declaration name
    /// elsewhere (names.md §5.3).
    Name(Ident),
    /// `$x`
    Local(Ident),
    /// `@x`
    PathParam(Ident),

    Field {
        base: Expr,
        field: Ident,
    },
    Index {
        base: Expr,
        index: Expr,
    },
    Call {
        callee: Expr,
        args: Vec<Expr>,
        /// `count(x where pred)` — queries.md §6.3.
        filter: Option<Expr>,
    },

    Unary {
        op: UnaryOp,
        rhs: Expr,
    },
    Binary {
        op: BinOp,
        lhs: Expr,
        rhs: Expr,
    },
    Ternary {
        cond: Expr,
        then: Expr,
        otherwise: Expr,
    },
    Coalesce {
        lhs: Expr,
        rhs: Expr,
    },
    /// `x in (a, b)` / `x not in ($xs)`
    In {
        lhs: Expr,
        items: Vec<Expr>,
        negated: bool,
    },
    /// `exists (select …)` / `not exists (…)`
    Exists {
        query: Expr,
        negated: bool,
    },

    Object(Vec<ObjEntry>),
    Array(Vec<Expr>),

    Select(Box<SelectExpr>),
    Insert(Box<InsertExpr>),
    Update(Box<UpdateExpr>),
    Delete(Box<DeleteExpr>),

    /// `<expr> or throw E(args)` — errors.md §5.
    OrThrow {
        value: Expr,
        error: Ident,
        args: Vec<Expr>,
    },
    /// `<expr> catch E (err) { … }` — errors.md §7.
    CatchPostfix {
        value: Expr,
        error: Ident,
        binder: Ident,
        body: Block,
    },
    /// `request.body() as Register` — the validated-input cast
    /// (routing.md §5.2). Only legal outside a query clause.
    Cast {
        value: Expr,
        ty: Ident,
    },
    /// `<response> with { … }` — routing.md §6.2.
    WithHeaders {
        value: Expr,
        headers: Vec<ObjEntry>,
    },
    /// `<response> cookie(name, value, opts)` — one `Set-Cookie` per
    /// occurrence. A JSON object cannot carry a duplicate key, so repeated
    /// headers need their own chained form (routing.md §6.2).
    Cookie {
        value: Expr,
        args: Vec<Expr>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    And,
    Or,
    Eq,
    Ne,
    /// `==?` — dropped when the right operand is null (queries.md §3.2).
    EqOpt,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
    ILike,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::EqOpt => "==?",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Like => "like",
            BinOp::ILike => "ilike",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
        }
    }
}

#[derive(Clone, Debug)]
pub enum ObjEntry {
    /// `k: v` (projection / JSON) or `k = v` (write target).
    Field {
        key: Ident,
        value: Expr,
        /// `=` rather than `:`.
        assign: bool,
        span: Span,
    },
    /// `...$x except a, b`
    Spread {
        source: Ident,
        except: Vec<Ident>,
        span: Span,
    },
}

// ---------------------------------------------------------------- queries

#[derive(Clone, Debug)]
pub struct SelectExpr {
    pub binder: Ident,
    pub source: QualifiedTable,
    pub joins: Vec<JoinClause>,
    pub filter: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub projection: Option<ObjectShape>,
    pub order_by: Vec<SortKey>,
    pub limit: Option<Expr>,
    pub page: Option<PageClause>,
    pub first: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct JoinClause {
    pub kind: JoinKind,
    pub table: QualifiedTable,
    pub binder: Ident,
    pub on: Expr,
    /// `left join … on … where … as many admins` — filters the child
    /// collection, not the driving rows (queries.md §4.7).
    pub filter: Option<Expr>,
    /// `None` only on a malformed join; a bare join is `Cardinality::Group`
    /// and says so (queries.md §6.2).
    pub result: Option<JoinResult>,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinKind {
    Left,
    Inner,
}

#[derive(Clone, Debug)]
pub struct JoinResult {
    pub cardinality: Cardinality,
    pub name: Ident,
    /// `under <binding>` — the explicit attachment (queries.md §4.4).
    pub under: Option<Ident>,
    pub order_by: Vec<SortKey>,
    pub limit: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cardinality {
    One,
    Many,
    /// `as group` — the join contributes to filtering and to aggregates and
    /// produces no field. Written out rather than inferred from a missing
    /// `as`, because "I forgot the projection" and "I meant to aggregate"
    /// used to be the same syntax (queries.md §6.2).
    Group,
}

#[derive(Clone, Debug)]
pub struct ObjectShape {
    pub fields: Vec<ProjField>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ProjField {
    /// `id`
    Column(Ident),
    /// `alias: expr`
    Expr {
        alias: Ident,
        value: Expr,
        span: Span,
    },
    /// `alias: { … }`
    Nested {
        alias: Ident,
        shape: ObjectShape,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct SortKey {
    pub expr: Expr,
    pub desc: bool,
    pub nulls: Option<NullsOrder>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct PageClause {
    pub after: Option<Expr>,
    pub size: Expr,
    pub max: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InsertExpr {
    pub table: QualifiedTable,
    pub values: Vec<ObjEntry>,
    pub conflict: Option<ConflictClause>,
    pub projection: Option<ObjectShape>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ConflictClause {
    pub columns: Vec<Ident>,
    pub action: ConflictAction,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ConflictAction {
    DoNothing,
    DoUpdate(Vec<SetItem>),
}

#[derive(Clone, Debug)]
pub struct UpdateExpr {
    pub table: QualifiedTable,
    pub sets: Vec<SetItem>,
    pub filter: Option<Expr>,
    pub projection: Option<ObjectShape>,
    pub order_by: Vec<SortKey>,
    pub first: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum SetItem {
    Set {
        column: Ident,
        value: Expr,
        /// `=?` — skipped when the value is null (writes.md §3.3).
        optional: bool,
        span: Span,
    },
    Spread {
        source: Ident,
        except: Vec<Ident>,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct DeleteExpr {
    pub table: QualifiedTable,
    pub filter: Option<Expr>,
    pub projection: Option<ObjectShape>,
    pub order_by: Vec<SortKey>,
    pub first: bool,
    pub span: Span,
}
