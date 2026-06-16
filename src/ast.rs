#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceFile {
    /// Display label for the file. The project loader sets this to a
    /// repo-relative path (`"main.jwc"`, `"users/login.jwc"`); single-file
    /// `parse_program` callers leave it empty and only `at line X, col Y`
    /// is rendered.
    pub label: String,
    /// Verbatim text the parser saw. Used by `validate_program` to look up
    /// line/col for an AST-stamped offset.
    pub text: String,
}

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
    /// All `using ns.path;` statements collected from every file. Each entry
    /// is attached to a namespace scope; used by the runtime resolver.
    pub imports: Vec<ImportDecl>,
    /// All `mount <ns> [at "/prefix"];` declarations. Library routes are
    /// inactive until a mount references their namespace. The same namespace
    /// may be mounted multiple times at different prefixes (e.g. `/api/foo`
    /// and `/public/foo`) — each mount produces its own active route set.
    pub mounts: Vec<MountDecl>,
    /// Module-level `const NAME = <expr>;` declarations. Evaluated once and
    /// frozen; inside route/function bodies a const name resolves like a
    /// read-only variable (locals shadow consts).
    pub consts: Vec<ConstDecl>,
    /// One entry per `.jwc` file the parser saw. Each top-level decl carries
    /// a `file_idx` into this vec, so the validator can render
    /// `at <label>:<line>:<col>` even in multi-file projects. Empty for
    /// hand-built `Program::default()` instances; the validator falls back
    /// to the bare `<msg>` shape when sources is empty or `file_idx` is
    /// out of range.
    pub sources: Vec<SourceFile>,
}

/// `const NAME = <expr>;` — a module-level immutable binding. The expression
/// must be a constant expression (literals, other consts, arithmetic /
/// comparison / logical ops, unary `-`/`!`, array/object literals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstDecl {
    pub name: String,
    pub expr: Expr,
    /// Byte offset of the `const` keyword in the source. 0 for synthesized
    /// nodes — `validate_program` treats `0` as "no location available".
    pub offset: usize,
    /// Index into [`Program::sources`] for the file this decl came from.
    /// Defaults to 0 (the first / only file).
    pub file_idx: usize,
}

/// Visibility marker for top-level declarations. Default is `Private` —
/// only callable from within the same namespace. `pub function ...`,
/// `pub entity ...`, etc., opt into export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    Public,
    #[default]
    Private,
}

/// `using foo.bar;` — opens namespace `foo.bar` for the file (and the
/// namespace it declares). Names from that namespace become reachable
/// both with and without the `foo.bar.` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
    /// Dot-split namespace path, e.g. `"math.utils"` → `["math", "utils"]`.
    pub path: Vec<String>,
    /// Namespace declared by the file this `using` lives in. Empty Vec
    /// means root. Used to compute import-visibility per call site.
    pub in_namespace: Vec<String>,
}

/// `mount <ns> [at "/prefix"];` — activate a library namespace's routes
/// (optionally under a path prefix). Middlewares come from the surrounding
/// `group` block, not the mount itself, so the same `use` syntax that
/// per-route uses can scope across both library mounts and own routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountDecl {
    /// Namespace path being mounted, e.g. `["greet"]` or `["pkg", "sub"]`.
    pub target: Vec<String>,
    /// Optional path prefix prepended to every route from the target ns.
    /// `None` means mount at root (`/`).
    pub prefix: Option<String>,
    /// Middleware chain inherited from enclosing `group` blocks. Each name
    /// is resolved against the symbol table at runtime (FQN-aware).
    pub middlewares: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorHandlerDecl {
    pub catch_var: String,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiddlewareDecl {
    pub name: String,
    /// Pre-handler statements. Runs before the route's handler. May
    /// short-circuit by `return`ing — that response becomes the
    /// route's response and the rest of the chain (plus `after_body`)
    /// is skipped.
    pub body: Vec<Stmt>,
    /// Optional `after { ... }` block — runs AFTER the route handler
    /// completes successfully. Reads `response_status()` /
    /// `response_duration_ms()` so middleware can record real
    /// observed latencies and statuses (the dogfooding gap
    /// jwc-shortener flagged: `latency_ms` stuck at 0 because the
    /// pre-handler middleware couldn't see the response).
    pub after_body: Option<Vec<Stmt>>,
    /// Namespace this declaration lives in. Empty = root.
    pub namespace: Vec<String>,
    pub visibility: Visibility,
    /// Byte offset of the `middleware` keyword.
    pub offset: usize,
    /// Index into [`Program::sources`] for the file this decl came from.
    pub file_idx: usize,
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
    /// Server-Sent Events. Syntax is recognised end-to-end (parser +
    /// validator) but the runtime dispatch is a stub — Phase 10.4 v2 will
    /// flesh out the chunked `text/event-stream` transport, `sse_send`,
    /// `sse_close`, and the per-topic `sse_broadcast` registry.
    Sse,
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
    /// Namespace this route lives in. Routes from non-root namespaces are
    /// inactive until activated via a `mount <ns> [at "/p"];` declaration.
    pub namespace: Vec<String>,
    /// Byte offset of the `route` keyword.
    pub offset: usize,
    /// Index into [`Program::sources`] for the file this decl came from.
    pub file_idx: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbContextDecl {
    pub name: String,
    pub driver: String,
    /// Namespace this dbcontext lives in. Non-root dbcontexts come along
    /// automatically whenever the project imports or mounts the namespace.
    pub namespace: Vec<String>,
    /// Byte offset of the `dbcontext` keyword.
    pub offset: usize,
    /// Index into [`Program::sources`] for the file this decl came from.
    pub file_idx: usize,
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
    /// Namespace this model lives in. Empty = root.
    pub namespace: Vec<String>,
    pub visibility: Visibility,
    /// Byte offset of the `entity` / `class` keyword.
    pub offset: usize,
    /// Index into [`Program::sources`] for the file this decl came from.
    pub file_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationKind {
    /// `name: List<Other> via Other.fk_col;` — the target holds the FK.
    OneToMany,
    /// `name: Other via Other.fk_col;` — the target holds the FK, at most one row.
    OneToOne,
    /// `name: Parent via local_fk_col;` — *this* entity holds the FK that
    /// points at the parent's PK (belongs-to). Materialises as a single nested
    /// object. Distinguished from `OneToOne` by a bare (undotted) `via` column.
    BelongsTo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationField {
    pub name: String,
    pub kind: NavigationKind,
    pub target_entity: String,
    /// The join FK column. For `OneToMany`/`OneToOne` it is the FK column *on
    /// the target* that points back at this entity's PK. For `BelongsTo` it is
    /// the FK column *on this entity* that points at the target's PK.
    pub target_field: String,
    /// Optional column subset for the materialised nav (`author: User { id,
    /// name } via authorId`). Empty = the whole row. Lets an eager-loaded
    /// relation hide sensitive columns (e.g. `passwordHash`).
    pub projection: Vec<String>,
    /// Optional ordering for the materialised collection
    /// (`... via T.fk order by T.col [asc|desc]`). Applied inside the
    /// `json_agg(... ORDER BY ...)` so a `OneToMany` nav comes back in a
    /// deterministic order instead of arbitrary `json_agg` order.
    pub order_by: Option<NavOrder>,
}

/// Ordering for a navigation collection: a target column + direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavOrder {
    pub col: String,
    pub dir: SortDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeSpec,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    /// `id int pk autoincrement;` — emits `GENERATED BY DEFAULT AS IDENTITY`
    /// in DDL and lets Postgres pick an id when the value is omitted from
    /// the insert payload.
    pub is_auto_increment: bool,
    /// `email varchar(120) unique;` — emits a column-level `UNIQUE` constraint
    /// in the generated DDL / migration diff.
    pub is_unique: bool,
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
    /// Namespace this function lives in. Empty = root.
    pub namespace: Vec<String>,
    pub visibility: Visibility,
    /// Byte offset of the `function` keyword (or `async` if present).
    pub offset: usize,
    /// Index into [`Program::sources`] for the file this decl came from.
    pub file_idx: usize,
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
    InList {
        field: String,
        values: Vec<Expr>,
    },
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
    Let {
        name: String,
        value: Expr,
    },
    Assign {
        name: String,
        value: Expr,
    },
    /// `var.field = value;` — sets one field on a JSON-object variable
    FieldAssign {
        var: String,
        field: String,
        value: Expr,
    },
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
    ValidateBody {
        fields: Vec<ValidateField>,
    },
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
    Transaction {
        body: Vec<Stmt>,
    },
    /// **Sprint 4B [1.0-blocker]** — `savepoint <name> { ... }` — nested
    /// rollback boundary INSIDE a `transaction { ... }`. Emits
    /// `SAVEPOINT <name>` at entry, `RELEASE SAVEPOINT <name>` on normal
    /// exit, and `ROLLBACK TO SAVEPOINT <name>; RELEASE SAVEPOINT <name>`
    /// on error so the outer transaction stays usable. Validator (E017)
    /// rejects savepoints declared outside a transaction. The legacy
    /// `transaction { transaction { ... } }` literal is rejected by the
    /// parser with E016 directing users at savepoint.
    Savepoint {
        name: String,
        body: Vec<Stmt>,
    },
    /// `for VAR in EXPR { ... }` — iterate over a JSON array (returned by
    /// `select`, `body()` parsed array, etc). `VAR` is rebound per iteration.
    ForIn {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
    },
    /// `insert VAR into CTX.TABLE;`
    DbInsert {
        var: String,
        context_var: String,
        table: String,
    },
    /// `update VAR in CTX.TABLE;`
    DbUpdate {
        var: String,
        context_var: String,
        table: String,
    },
    /// `delete VAR from CTX.TABLE;`
    DbDelete {
        var: String,
        context_var: String,
        table: String,
    },
    /// `delete from CTX.TABLE where COND ...;` — bulk delete without a
    /// preloaded object. `where` is required (a missing where would wipe the
    /// whole table) and is enforced at parse time.
    DbDeleteWhere {
        context_var: String,
        table: String,
        where_clause: Box<WhereExpr>,
    },
    /// `update CTX.TABLE set col1 = expr1, col2 = expr2 where COND ...;` —
    /// atomic partial-row update. Compiles to a single SQL `UPDATE … SET …
    /// WHERE …`, no preceding read. Closes the lost-update window that the
    /// whole-row `update var in CTX.Table` form has under concurrent
    /// reads (observed in production: jwc-shortener's `hits` counter).
    /// `where` is required for the same reason `delete from` requires it
    /// — a missing predicate would touch every row in the table.
    DbUpdateSet {
        context_var: String,
        table: String,
        /// Each entry is (column, RHS expression). RHS can reference
        /// other columns of the same row directly by name (the column
        /// validator accepts `Entity.col` references inside the RHS just
        /// like `where Entity.col == …` does).
        assignments: Vec<(String, Expr)>,
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
    Call {
        name: String,
        args: Vec<Expr>,
    },
    /// `var.field` — reads one field from a JSON-object variable
    FieldGet {
        var: String,
        field: String,
    },
    /// `new EntityName()` — creates an empty JSON object `{}`
    NewEntity {
        entity: String,
    },
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
    ///        [group by Entity.col [, ...]]
    ///        [having COND [and|or COND ...]]
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
        /// `group by Entity.col [, ...]` — column paths to GROUP BY at the
        /// SQL layer. Each entry is a `field` string in the same shape as
        /// `where Entity.col`. Empty vec when no `group by` was written.
        group_by: Vec<String>,
        /// `having <cond>` — post-aggregation filter applied after GROUP BY.
        /// Same shape as `where_clause`; only meaningful when `group_by` is
        /// non-empty, but the parser stores it unconditionally and the
        /// emitter just inlines whatever's here.
        having: Option<Box<WhereExpr>>,
    },
    /// `await expr` — placeholder for the future async runtime; today this
    /// is a transparent pass-through that evaluates the inner expression.
    Await(Box<Expr>),
    /// `!expr` — boolean negation. Inner must evaluate to a bool.
    Not(Box<Expr>),
    /// `{ key: value, key2: value2 }` — JSON object literal. Each value is
    /// evaluated and the result is serialised as a JSON string Value.
    ObjectLit(Vec<(String, Expr)>),
    /// `[expr, expr, ...]` — array literal. Elements may be heterogeneous
    /// (`[1, "two", true]`); a trailing comma and the empty form `[]` are both
    /// valid. Evaluates to `Value::Array` in the interpreter.
    ArrayLit(Vec<Expr>),
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
