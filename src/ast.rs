#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Program {
    pub dbcontexts: Vec<DbContextDecl>,
    pub models: Vec<ModelDecl>,
    pub routes: Vec<RouteDecl>,
    pub functions: Vec<FunctionDecl>,
    pub middlewares: Vec<MiddlewareDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiddlewareDecl {
    pub name: String,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelKind {
    Entity,
    Class,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecl {
    pub method: String,
    pub path: String,
    pub handler: Option<String>,
    pub body: Vec<Stmt>,
    /// Names of middlewares applied to this route (in declaration order).
    pub middlewares: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbContextDecl {
    pub name: String,
    pub driver: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDecl {
    pub kind: ModelKind,
    pub name: String,
    /// Optional owning dbcontext name from: `entity X of AppDbContext { ... }`
    pub context_name: Option<String>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeSpec,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    pub references: Option<FieldReference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldReference {
    /// Target entity name.
    pub entity: String,
    /// Target column name (typically the PK).
    pub column: String,
    pub on_delete: OnDeleteAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDeleteAction {
    NoAction,
    Cascade,
    Restrict,
    SetNull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSpec {
    pub name: String,
    pub args: Vec<i64>,
}

/// A function parameter with an optional type annotation.
/// `name: string`  → `TypedParam { name: "name", ty: Some("string") }`
/// `x`             → `TypedParam { name: "x",    ty: None }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedParam {
    pub name: String,
    /// Type name if annotated, e.g. `"string"`, `"int"`, `"bool"`, `"User"` …
    pub ty: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<TypedParam>,
    /// Optional return-type annotation: `function foo(): User`
    pub return_type: Option<String>,
    pub body: Vec<Stmt>,
    /// True when the function was declared `async function ...`. Reserved —
    /// the interpreter executes everything synchronously today; flag exists so
    /// AST is forward-compatible with the future tokio-based runtime.
    pub is_async: bool,
}

/// Single comparison: `field op value`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbWhere {
    /// Column path, e.g. `"Entity.field"` or `"field"`. Runner strips the entity prefix.
    pub field: String,
    /// SQL comparison operator: `"="`, `"=="`, `"!="`, `"<"`, `"<="`, `">"`, `">="`
    pub op: String,
    /// Right-hand side value expression
    pub rhs: Expr,
}

/// Boolean tree over comparisons — supports `and`/`or` and parenthesised
/// sub-expressions. Builds SQL like `(a = $1 AND (b > $2 OR c IS NULL))`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhereExpr {
    Atom(DbWhere),
    And(Box<WhereExpr>, Box<WhereExpr>),
    Or(Box<WhereExpr>, Box<WhereExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbOrderBy {
    /// Column path, e.g. `"Entity.created_at"` or `"created_at"`.
    pub field: String,
    pub dir: SortDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidateRule {
    /// Field must be present and non-null.
    Required,
    /// String length must be ≥ n.
    MinLength(i64),
    /// String length must be ≤ n.
    MaxLength(i64),
    /// Numeric value must be ≥ n (string-stored to allow ints or decimals).
    Min(String),
    /// Numeric value must be ≤ n.
    Max(String),
    /// String must match the given regular expression (full-match semantics).
    Pattern(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateField {
    pub name: String,
    pub rules: Vec<ValidateRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Let { name: String, value: Expr },
    Assign { name: String, value: Expr },
    /// `var.field = value;` — sets one field on a JSON-object variable
    FieldAssign { var: String, field: String, value: Expr },
    Print(Expr),
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Expr(Expr),
    Return(Option<Expr>),
    /// `validate body { field: rule, rule; ... }` — runs against `body()` JSON
    /// and short-circuits the route with a 400 response if any rule fails.
    ValidateBody { fields: Vec<ValidateField> },
    /// `try { ... } catch (var[: ErrorType]) { ... }` — catches any runtime
    /// error from the try body and binds the error to `var` as a JSON object
    /// `{"message": "..."}` before running the catch body.
    Try {
        body: Vec<Stmt>,
        catch_var: String,
        /// Optional error type filter (e.g. `DbError`). Reserved — currently
        /// matches all errors.
        catch_type: Option<String>,
        catch_body: Vec<Stmt>,
    },
    /// `insert VAR into CTX.TABLE;`
    DbInsert { var: String, context_var: String, table: String },
    /// `update VAR in CTX.TABLE;`
    DbUpdate { var: String, context_var: String, table: String },
    /// `delete VAR from CTX.TABLE;`
    DbDelete { var: String, context_var: String, table: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int(i64),
    /// Decimal literal kept as source text (e.g. `0.2`) and parsed at runtime.
    Float(String),
    Str(String),
    Bool(bool),
    Null,
    Var(String),
    Call { name: String, args: Vec<Expr> },
    /// `var.field` — reads one field from a JSON-object variable
    FieldGet { var: String, field: String },
    /// `new EntityName()` — creates an empty JSON object `{}`
    NewEntity { entity: String },
    /// `select [Entity|*] from CTX.TABLE [where COND [and|or COND ...]]
    ///                                    [orderby FIELD [asc|desc]]
    ///                                    [limit N] [offset N] [first]`
    DbSelect {
        entity: String,
        context_var: String,
        table: String,
        where_clause: Option<Box<WhereExpr>>,
        order_by: Option<DbOrderBy>,
        limit: Option<Box<Expr>>,
        offset: Option<Box<Expr>>,
        first: bool,
    },
    /// `await expr` — placeholder for the future async runtime; today this
    /// is a transparent pass-through that evaluates the inner expression.
    Await(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Mod(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    Neq(Box<Expr>, Box<Expr>),
    Lt(Box<Expr>, Box<Expr>),
    Lte(Box<Expr>, Box<Expr>),
    Gt(Box<Expr>, Box<Expr>),
    Gte(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}
