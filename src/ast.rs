#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Program {
    pub dbcontexts: Vec<DbContextDecl>,
    pub models: Vec<ModelDecl>,
    pub routes: Vec<RouteDecl>,
    pub functions: Vec<FunctionDecl>,
    pub middlewares: Vec<MiddlewareDecl>,
    /// Optional top-level fallback that catches uncaught errors from any
    /// route handler. Body sees `<catch_var>` bound to the error JSON.
    pub error_handler: Option<ErrorHandlerDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorHandlerDecl {
    pub catch_var: String,
    pub body: Vec<Stmt>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteProtocol {
    Http,
    Ws,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecl {
    /// HTTP method for HTTP routes (`"GET"`, `"POST"`, ...). For WS routes,
    /// this is the literal `"WS"` so existing routing/diagnostic code keeps a
    /// uniform shape.
    pub method: String,
    pub path: String,
    pub handler: Option<String>,
    pub body: Vec<Stmt>,
    /// Names of middlewares applied to this route (in declaration order).
    pub middlewares: Vec<String>,
    pub protocol: RouteProtocol,
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
    /// Navigation properties (relations) declared inside this entity.
    /// Empty for plain DTO classes.
    pub navigations: Vec<NavigationField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationKind {
    /// `name: List<Other> via Other.fk_col;`
    OneToMany,
    /// `name: Other via Other.fk_col;` — at most one matching row.
    OneToOne,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationField {
    pub name: String,
    pub kind: NavigationKind,
    pub target_entity: String,
    /// The FK column on the target entity that points back at this entity's PK.
    pub target_field: String,
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
    /// `field in (expr1, expr2, ...)` — SQL `"field" IN ($1, $2, ...)`.
    InList { field: String, values: Vec<Expr> },
    /// `field between @low and @high` — SQL `"field" BETWEEN $1 AND $2`.
    Between {
        field: String,
        low: Expr,
        high: Expr,
    },
    And(Box<WhereExpr>, Box<WhereExpr>),
    Or(Box<WhereExpr>, Box<WhereExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Sum,
    Avg,
    Min,
    Max,
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
    /// `transaction { ... }` — all DB statements inside run on a single
    /// pooled connection wrapped in a SQL transaction; an uncaught error
    /// rolls back, otherwise commits at the end of the block.
    Transaction { body: Vec<Stmt> },
    /// `for VAR in EXPR { ... }` — iterate over a JSON array (returned by
    /// `select`, `body()` parsed array, etc). `VAR` is rebound per iteration.
    ForIn { var: String, iter: Expr, body: Vec<Stmt> },
    /// `insert VAR into CTX.TABLE;`
    DbInsert { var: String, context_var: String, table: String },
    /// `update VAR in CTX.TABLE;`
    DbUpdate { var: String, context_var: String, table: String },
    /// `delete VAR from CTX.TABLE;`
    DbDelete { var: String, context_var: String, table: String },
    /// `delete from CTX.TABLE where COND ...;` — bulk delete without a
    /// preloaded object. `where` is required (a missing where would wipe the
    /// whole table) and is enforced at parse time.
    DbDeleteWhere {
        context_var: String,
        table: String,
        where_clause: Box<WhereExpr>,
    },
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
    /// `select count(*) from CTX.TABLE [where COND ...]` — returns `int`.
    DbCount {
        context_var: String,
        table: String,
        where_clause: Option<Box<WhereExpr>>,
    },
    /// `select sum|avg|min|max(Entity.col) from CTX.TABLE [where COND ...]`.
    /// Result is parsed to int/float/string depending on the SQL response.
    DbAggregate {
        kind: AggregateKind,
        field: String,
        context_var: String,
        table: String,
        where_clause: Option<Box<WhereExpr>>,
    },
    /// `select [Entity|*] [ { col1, col2, ... } ] [with rel, ...] from CTX.TABLE
    ///        [where COND [and|or COND ...]]
    ///        [orderby FIELD [asc|desc]]
    ///        [limit N] [offset N] [first]`
    DbSelect {
        entity: String,
        context_var: String,
        table: String,
        where_clause: Option<Box<WhereExpr>>,
        order_by: Option<DbOrderBy>,
        limit: Option<Box<Expr>>,
        offset: Option<Box<Expr>>,
        first: bool,
        /// Navigation property names to eagerly join (`with posts, comments`).
        with_relations: Vec<String>,
        /// Column-name subset to project (`select User { name, email } ...`).
        /// Empty vec means `SELECT *` — every column from the source table.
        projection: Vec<String>,
    },
    /// `await expr` — placeholder for the future async runtime; today this
    /// is a transparent pass-through that evaluates the inner expression.
    Await(Box<Expr>),
    /// `!expr` — boolean negation. Inner must evaluate to a bool.
    Not(Box<Expr>),
    /// `{ key: value, key2: value2 }` — JSON object literal. Each value is
    /// evaluated and the result is serialised as a JSON string Value.
    ObjectLit(Vec<(String, Expr)>),
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
