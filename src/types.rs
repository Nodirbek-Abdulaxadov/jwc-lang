//! The value lattice (types.md).
//!
//! `Raw` and `Record` are the two representations a query result can have,
//! and keeping them apart is the whole reason this module exists: reading a
//! field of a `Raw` is a compile error, and a `Raw` may only ever be spliced
//! into a response, never inspected.

use std::fmt;

/// Ordered field list. A `Record`'s field order is its projection order,
/// which is also its JSON key order — so it is a `Vec`, not a map.
pub type Fields = Vec<(String, Ty)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    /// A Postgres scalar, named as JWC spells it (`bigint`, `varchar`,
    /// `numeric`, …). Precision is not part of the type: `varchar(40)` and
    /// `varchar(80)` are the same type to the checker, and the database
    /// enforces the width.
    Scalar(Scalar),
    /// A declared `enum`, by declared name.
    Enum(String),
    /// A declared `class` — request input only (types.md §4.1).
    Class(String),
    Record(Fields),
    /// An opaque JSON fragment from Postgres (types.md §5.1).
    Raw,
    Array(Box<Ty>),
    Optional(Box<Ty>),
    /// The result of a response builder (types.md §8).
    Response,
    /// `null` before it is assigned to anything.
    Null,
    Void,
    /// Error recovery. Assignable to and from everything, and never the
    /// cause of a second diagnostic.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    Smallint,
    Int,
    Bigint,
    Numeric,
    Boolean,
    Varchar,
    Text,
    Timestamptz,
    Date,
    Time,
    Interval,
    Uuid,
    Jsonb,
    Inet,
    Bytea,
}

impl Scalar {
    pub fn from_name(s: &str) -> Option<Scalar> {
        Some(match s {
            "smallint" => Scalar::Smallint,
            "int" => Scalar::Int,
            "bigint" => Scalar::Bigint,
            "numeric" => Scalar::Numeric,
            "boolean" => Scalar::Boolean,
            "varchar" => Scalar::Varchar,
            "text" => Scalar::Text,
            "timestamptz" => Scalar::Timestamptz,
            "date" => Scalar::Date,
            "time" => Scalar::Time,
            "interval" => Scalar::Interval,
            "uuid" => Scalar::Uuid,
            "jsonb" => Scalar::Jsonb,
            "inet" => Scalar::Inet,
            "bytea" => Scalar::Bytea,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Scalar::Smallint => "smallint",
            Scalar::Int => "int",
            Scalar::Bigint => "bigint",
            Scalar::Numeric => "numeric",
            Scalar::Boolean => "boolean",
            Scalar::Varchar => "varchar",
            Scalar::Text => "text",
            Scalar::Timestamptz => "timestamptz",
            Scalar::Date => "date",
            Scalar::Time => "time",
            Scalar::Interval => "interval",
            Scalar::Uuid => "uuid",
            Scalar::Jsonb => "jsonb",
            Scalar::Inet => "inet",
            Scalar::Bytea => "bytea",
        }
    }

    /// Numeric widening order (types.md §10.3 rule 5). `None` for
    /// non-numerics.
    pub fn numeric_rank(self) -> Option<u8> {
        Some(match self {
            Scalar::Smallint => 0,
            Scalar::Int => 1,
            Scalar::Bigint => 2,
            Scalar::Numeric => 3,
            _ => return None,
        })
    }

    pub fn is_numeric(self) -> bool {
        self.numeric_rank().is_some()
    }

    pub fn is_text(self) -> bool {
        matches!(self, Scalar::Varchar | Scalar::Text)
    }

    /// Types `<`, `<=`, `>`, `>=` are defined for (types.md §12.6).
    pub fn is_ordered(self) -> bool {
        self.is_numeric()
            || self.is_text()
            || matches!(
                self,
                Scalar::Timestamptz | Scalar::Date | Scalar::Time | Scalar::Interval | Scalar::Uuid
            )
    }

    /// JSON wire form. `bigint` and `numeric` are **strings** on every path
    /// (types.md §2.3).
    pub fn wire_is_string(self) -> bool {
        matches!(
            self,
            Scalar::Bigint
                | Scalar::Numeric
                | Scalar::Varchar
                | Scalar::Text
                | Scalar::Timestamptz
                | Scalar::Date
                | Scalar::Time
                | Scalar::Interval
                | Scalar::Uuid
                | Scalar::Inet
                | Scalar::Bytea
        )
    }
}

impl Ty {
    pub fn text() -> Ty {
        Ty::Scalar(Scalar::Text)
    }
    pub fn int() -> Ty {
        Ty::Scalar(Scalar::Int)
    }
    pub fn bigint() -> Ty {
        Ty::Scalar(Scalar::Bigint)
    }
    pub fn boolean() -> Ty {
        Ty::Scalar(Scalar::Boolean)
    }
    pub fn numeric() -> Ty {
        Ty::Scalar(Scalar::Numeric)
    }
    pub fn timestamptz() -> Ty {
        Ty::Scalar(Scalar::Timestamptz)
    }
    pub fn interval() -> Ty {
        Ty::Scalar(Scalar::Interval)
    }
    pub fn inet() -> Ty {
        Ty::Scalar(Scalar::Inet)
    }

    pub fn opt(self) -> Ty {
        match self {
            Ty::Optional(_) | Ty::Unknown | Ty::Null => self,
            other => Ty::Optional(Box::new(other)),
        }
    }

    pub fn array(self) -> Ty {
        Ty::Array(Box::new(self))
    }

    pub fn is_optional(&self) -> bool {
        matches!(self, Ty::Optional(_) | Ty::Null)
    }

    /// The type with its outermost `?` removed. `null` narrows to `Unknown`
    /// so a chained mistake produces one diagnostic, not two.
    pub fn strip_opt(&self) -> Ty {
        match self {
            Ty::Optional(inner) => (**inner).clone(),
            Ty::Null => Ty::Unknown,
            other => other.clone(),
        }
    }

    pub fn scalar(&self) -> Option<Scalar> {
        match self {
            Ty::Scalar(s) => Some(*s),
            _ => None,
        }
    }

    pub fn is_raw(&self) -> bool {
        match self {
            Ty::Raw => true,
            Ty::Array(inner) | Ty::Optional(inner) => inner.is_raw(),
            _ => false,
        }
    }

    pub fn fields(&self) -> Option<&Fields> {
        match self {
            Ty::Record(f) => Some(f),
            Ty::Optional(inner) | Ty::Array(inner) => inner.fields(),
            _ => None,
        }
    }

    pub fn element(&self) -> Option<&Ty> {
        match self {
            Ty::Array(inner) => Some(inner),
            Ty::Optional(inner) => inner.element(),
            _ => None,
        }
    }

    /// Assignability (types.md §10.3).
    pub fn assignable_to(&self, target: &Ty) -> bool {
        use Ty::*;
        if matches!(self, Unknown) || matches!(target, Unknown) {
            return true;
        }
        if self == target {
            return true;
        }
        match (self, target) {
            // rule 2: null fits any optional
            (Null, Optional(_)) => true,
            (Null, _) => false,
            // rule 3: T fits T?
            (_, Optional(inner)) => self.assignable_to(inner),
            // an optional never fits a non-optional
            (Optional(_), _) => false,
            // rule 4: width subtyping on records
            (Record(from), Record(to)) => to.iter().all(|(name, want)| {
                from.iter()
                    .find(|(n, _)| n == name)
                    .is_some_and(|(_, have)| have.assignable_to(want))
            }),
            (Array(a), Array(b)) => a.assignable_to(b),
            // types.md §5.6 — "a `jsonb` value written from code takes any
            // `Record`, array, scalar or `Raw`". It is the one column type
            // whose shape is not the schema's business, which is what a
            // per-event audit payload needs; without this an activity row
            // could only be written as a pre-encoded string.
            (Record(_) | Array(_) | Raw | Scalar(_), Scalar(crate::types::Scalar::Jsonb)) => true,
            // rule 5: numeric widening, never narrowing
            (Scalar(a), Scalar(b)) => match (a.numeric_rank(), b.numeric_rank()) {
                (Some(x), Some(y)) => x <= y,
                _ => a.is_text() && b.is_text(),
            },
            _ => false,
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Scalar(s) => write!(f, "{}", s.name()),
            Ty::Enum(n) | Ty::Class(n) => write!(f, "{n}"),
            Ty::Record(fields) => {
                write!(f, "{{ ")?;
                for (i, (n, t)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{n}: {t}")?;
                }
                write!(f, " }}")
            }
            Ty::Raw => write!(f, "raw"),
            Ty::Array(inner) => write!(f, "{inner}[]"),
            Ty::Optional(inner) => write!(f, "{inner}?"),
            Ty::Response => write!(f, "response"),
            Ty::Null => write!(f, "null"),
            Ty::Void => write!(f, "void"),
            Ty::Unknown => write!(f, "?"),
        }
    }
}

/// The result of `a + b` and friends (types.md §12.1, §12.2, §12.3).
pub fn arith(op: crate::ast::BinOp, lhs: &Ty, rhs: &Ty) -> Option<Ty> {
    use crate::ast::BinOp::*;
    if matches!(lhs, Ty::Unknown) || matches!(rhs, Ty::Unknown) {
        return Some(Ty::Unknown);
    }
    let (a, b) = (lhs.scalar()?, rhs.scalar()?);
    match op {
        Add => {
            if a.is_numeric() && b.is_numeric() {
                return Some(Ty::Scalar(widen(a, b)));
            }
            if a.is_text() && b.is_text() {
                return Some(Ty::text());
            }
            match (a, b) {
                (Scalar::Timestamptz, Scalar::Interval) => Some(Ty::timestamptz()),
                (Scalar::Date, Scalar::Interval) => Some(Ty::timestamptz()),
                (Scalar::Interval, Scalar::Interval) => Some(Ty::interval()),
                _ => None,
            }
        }
        Sub => {
            if a.is_numeric() && b.is_numeric() {
                return Some(Ty::Scalar(widen(a, b)));
            }
            match (a, b) {
                (Scalar::Timestamptz, Scalar::Timestamptz) => Some(Ty::interval()),
                (Scalar::Timestamptz, Scalar::Interval) => Some(Ty::timestamptz()),
                (Scalar::Date, Scalar::Date) => Some(Ty::interval()),
                (Scalar::Interval, Scalar::Interval) => Some(Ty::interval()),
                _ => None,
            }
        }
        Mul | Div => {
            if a.is_numeric() && b.is_numeric() {
                Some(Ty::Scalar(widen(a, b)))
            } else {
                None
            }
        }
        Rem => {
            // Integer only (types.md §12.2).
            let int = |s: Scalar| matches!(s, Scalar::Smallint | Scalar::Int | Scalar::Bigint);
            if int(a) && int(b) {
                Some(Ty::Scalar(widen(a, b)))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `smallint op smallint` widens to `int` rather than staying `smallint`:
/// the sum of two small values is routinely not small (types.md §12.3).
fn widen(a: Scalar, b: Scalar) -> Scalar {
    let rank = a
        .numeric_rank()
        .unwrap_or(1)
        .max(b.numeric_rank().unwrap_or(1));
    match rank {
        0 | 1 => Scalar::Int,
        2 => Scalar::Bigint,
        _ => Scalar::Numeric,
    }
}

/// Whether `==` / `!=` is defined for this pair (types.md §12.6).
pub fn comparable(a: &Ty, b: &Ty) -> bool {
    use Ty::*;
    if matches!(a, Unknown) || matches!(b, Unknown) {
        return true;
    }
    if matches!(a, Null) || matches!(b, Null) {
        return true;
    }
    let (a, b) = (a.strip_opt(), b.strip_opt());
    if a == b {
        return true;
    }
    match (&a, &b) {
        (Scalar(x), Scalar(y)) => {
            (x.is_numeric() && y.is_numeric()) || (x.is_text() && y.is_text())
        }
        _ => false,
    }
}

/// Whether `<`, `<=`, `>`, `>=` is defined (types.md §12.6).
pub fn orderable(a: &Ty, b: &Ty) -> bool {
    use Ty::*;
    if matches!(a, Unknown) || matches!(b, Unknown) {
        return true;
    }
    match (a, b) {
        (Scalar(x), Scalar(y)) => {
            x.is_ordered()
                && y.is_ordered()
                && ((x.is_numeric() && y.is_numeric()) || (x.is_text() && y.is_text()) || x == y)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_never_fits_a_required_slot() {
        assert!(Ty::int().assignable_to(&Ty::int().opt()));
        assert!(!Ty::int().opt().assignable_to(&Ty::int()));
        assert!(Ty::Null.assignable_to(&Ty::int().opt()));
        assert!(!Ty::Null.assignable_to(&Ty::int()));
    }

    #[test]
    fn numerics_widen_never_narrow() {
        assert!(Ty::int().assignable_to(&Ty::bigint()));
        assert!(Ty::bigint().assignable_to(&Ty::numeric()));
        assert!(!Ty::bigint().assignable_to(&Ty::int()));
        assert!(!Ty::numeric().assignable_to(&Ty::bigint()));
    }

    #[test]
    fn records_use_width_subtyping() {
        let wide = Ty::Record(vec![
            ("id".into(), Ty::bigint()),
            ("email".into(), Ty::text()),
        ]);
        let narrow = Ty::Record(vec![("id".into(), Ty::bigint())]);
        assert!(wide.assignable_to(&narrow), "extra fields are dropped");
        assert!(!narrow.assignable_to(&wide), "a missing field is not");
    }

    #[test]
    fn raw_is_assignable_only_to_raw() {
        assert!(Ty::Raw.assignable_to(&Ty::Raw));
        assert!(!Ty::Raw.assignable_to(&Ty::text()));
        assert!(!Ty::text().assignable_to(&Ty::Raw));
    }

    #[test]
    fn plus_has_exactly_three_overloads() {
        use crate::ast::BinOp::Add;
        assert_eq!(arith(Add, &Ty::int(), &Ty::int()), Some(Ty::int()));
        assert_eq!(arith(Add, &Ty::text(), &Ty::text()), Some(Ty::text()));
        assert_eq!(
            arith(Add, &Ty::timestamptz(), &Ty::interval()),
            Some(Ty::timestamptz())
        );
        // No implicit stringification (types.md §12.1).
        assert_eq!(arith(Add, &Ty::text(), &Ty::int()), None);
    }

    #[test]
    fn int_times_int_stays_int_but_smallint_widens() {
        use crate::ast::BinOp::Mul;
        assert_eq!(arith(Mul, &Ty::int(), &Ty::int()), Some(Ty::int()));
        assert_eq!(
            arith(
                Mul,
                &Ty::Scalar(Scalar::Smallint),
                &Ty::Scalar(Scalar::Smallint)
            ),
            Some(Ty::int())
        );
        assert_eq!(arith(Mul, &Ty::int(), &Ty::numeric()), Some(Ty::numeric()));
    }

    #[test]
    fn enums_compare_but_do_not_order() {
        let e = Ty::Enum("Role".into());
        assert!(comparable(&e, &e));
        assert!(comparable(&e, &Ty::Null));
        assert!(!orderable(&e, &e), "types.md §3.5");
    }

    #[test]
    fn bigint_and_numeric_go_out_as_strings() {
        assert!(Scalar::Bigint.wire_is_string());
        assert!(Scalar::Numeric.wire_is_string());
        assert!(!Scalar::Int.wire_is_string());
        assert!(!Scalar::Boolean.wire_is_string());
    }
}
