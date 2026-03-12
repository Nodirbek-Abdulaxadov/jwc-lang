#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Program {
    pub dbcontexts: Vec<DbContextDecl>,
    pub entities: Vec<EntityDecl>,
    pub routes: Vec<RouteDecl>,
    pub functions: Vec<FunctionDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecl {
    pub method: String,
    pub path: String,
    pub handler: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbContextDecl {
    pub name: String,
    pub driver: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDecl {
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
}

/// WHERE clause in a DB select: `field op value`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbWhere {
    /// Column path, e.g. `"Entity.field"` or `"field"`. Runner strips the entity prefix.
    pub field: String,
    /// SQL comparison operator: `"="`, `"=="`, `"!="`, `"<"`, `"<="`, `">"`, `">="`
    pub op: String,
    /// Right-hand side value expression
    pub rhs: Expr,
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
    Str(String),
    Bool(bool),
    Null,
    Var(String),
    Call { name: String, args: Vec<Expr> },
    /// `var.field` — reads one field from a JSON-object variable
    FieldGet { var: String, field: String },
    /// `new EntityName()` — creates an empty JSON object `{}`
    NewEntity { entity: String },
    /// `select [Entity|*] from CTX.TABLE [where FIELD OP EXPR] [first]`
    DbSelect {
        entity: String,
        context_var: String,
        table: String,
        where_clause: Option<Box<DbWhere>>,
        first: bool,
    },
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
