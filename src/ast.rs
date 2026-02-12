#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub dbcontexts: Vec<DbContextDecl>,
    pub entities: Vec<EntityDecl>,
    pub selects: Vec<SelectStmt>,
    pub routes: Vec<RouteDecl>,
    pub controllers: Vec<ClassDecl>,
    pub classes: Vec<ClassDecl>,
    pub functions: Vec<FunctionDecl>,
}

impl Program {
    pub fn new() -> Self {
        Self {
            dbcontexts: Vec::new(),
            entities: Vec::new(),
            selects: Vec::new(),
            routes: Vec::new(),
            controllers: Vec::new(),
            classes: Vec::new(),
            functions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDecl {
    pub name: String,
    pub members: Vec<ClassMember>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassMember {
    Field(FieldDeclRuntime),
    Method(FunctionDecl),
    Route(RouteDecl),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDeclRuntime {
    pub name: String,
    pub init: Option<ValueExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecl {
    pub method: String,
    pub path: String,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueExpr {
    Literal(Literal),
    Var(String),
    This,
    New { class_name: String },
    Member(Box<ValueExpr>, String),
    MethodCall {
        receiver: Box<ValueExpr>,
        name: String,
        args: Vec<ValueExpr>,
    },
    Assign {
        target: Box<ValueExpr>,
        value: Box<ValueExpr>,
    },
    Call { name: String, args: Vec<ValueExpr> },
    Array(Vec<ValueExpr>),
    Index(Box<ValueExpr>, Box<ValueExpr>),
    Length(Box<ValueExpr>),
    Add(Box<ValueExpr>, Box<ValueExpr>),
    Sub(Box<ValueExpr>, Box<ValueExpr>),
    Mul(Box<ValueExpr>, Box<ValueExpr>),
    Div(Box<ValueExpr>, Box<ValueExpr>),
    Mod(Box<ValueExpr>, Box<ValueExpr>),
    Neg(Box<ValueExpr>),
    Eq(Box<ValueExpr>, Box<ValueExpr>),
    Neq(Box<ValueExpr>, Box<ValueExpr>),
    Lt(Box<ValueExpr>, Box<ValueExpr>),
    Gt(Box<ValueExpr>, Box<ValueExpr>),
    Lte(Box<ValueExpr>, Box<ValueExpr>),
    Gte(Box<ValueExpr>, Box<ValueExpr>),
    And(Box<ValueExpr>, Box<ValueExpr>),
    Or(Box<ValueExpr>, Box<ValueExpr>),
    DbSelect {
        entity: String,
        where_field: Option<String>,
        where_value: Option<Box<ValueExpr>>,
    },
    DbInsert {
        entity: String,
        assignments: Vec<(String, ValueExpr)>,
    },
    DbUpdate {
        entity: String,
        assignments: Vec<(String, ValueExpr)>,
        where_field: String,
        where_value: Box<ValueExpr>,
    },
    DbDelete {
        entity: String,
        where_field: String,
        where_value: Box<ValueExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    VarDecl { name: String, value: ValueExpr },
    Assign { name: String, value: ValueExpr },
    AssignMember {
        receiver: ValueExpr,
        field: String,
        value: ValueExpr,
    },
    Print(ValueExpr),
    Expr(ValueExpr),
    If {
        cond: ValueExpr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
    },
    While { cond: ValueExpr, body: Vec<Stmt> },
    ForRange {
        var: String,
        start: ValueExpr,
        end: ValueExpr,
        body: Vec<Stmt>,
    },
    Switch {
        expr: ValueExpr,
        cases: Vec<(Literal, Vec<Stmt>)>,
        default: Option<Vec<Stmt>>,
    },
    Break,
    Continue,
    Return(Option<ValueExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbContextDecl {
    pub name: String,
    pub driver: String,
    pub url: Option<String>,
    pub sets: Vec<DbSetDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbSetDecl {
    pub entity: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeSpec,
    pub mods: FieldMods,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldMods {
    pub nullable: bool,
    pub unique: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSpec {
    pub name: String,
    pub args: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectStmt {
    pub target: SelectTarget,
    pub from: String,
    pub where_expr: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectTarget {
    Star,
    Entity(String),
    Fields(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Cmp(CmpExpr),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmpExpr {
    pub field: String,
    pub op: CmpOp,
    pub value: Literal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    Int(i64),
    Decimal(String),
    Str(String),
    Bool(bool),
    Array(Vec<Literal>),
    Obj(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Gt,
    Lt,
    Gte,
    Lte,
}
