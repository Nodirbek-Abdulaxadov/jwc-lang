//! Physical names and the versioned constraint-naming function.
//!
//! Two rules, both load-bearing for the DBA test:
//!
//! * a physical name is the snake_case transform of the declared name, with
//!   no pluralisation and no prefix (names.md §4.1);
//! * every constraint and index is named explicitly, deterministically, from
//!   table + columns + canonical predicate — never from the message text and
//!   never left to Postgres (schema.md §8).
//!
//! The second rule is what decouples a constraint's identity from its
//! message: editing `unique (...) : "…"` changes no DDL and produces no
//! migration, while the runtime can still map `SQLSTATE 23505` back to the
//! right sentence by name.

use sha2::{Digest, Sha256};

/// The naming scheme this build emits. Recorded in every migration
/// snapshot so a future scheme can be adopted without renaming live
/// constraints (schema.md §8.2).
pub const SCHEME_VERSION: &str = "v1";

/// Postgres identifier limit. A generated name that would exceed it has its
/// column segment replaced by a hash — never truncated, because two
/// truncated names can collide.
const MAX_IDENT: usize = 63;

/// snake_case transform (names.md §4.1).
///
/// Inserts `_` before an uppercase letter that follows a lowercase letter or
/// digit, or that is followed by a lowercase letter, then lowercases. Index
/// 0 never takes a separator.
pub fn physical(declared: &str) -> String {
    let chars: Vec<char> = declared.chars().collect();
    let mut out = String::with_capacity(declared.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || next_is_lower {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// First 8 lowercase hex digits of SHA-256 over `text`.
pub fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut s = String::with_capacity(8);
    for b in digest.iter().take(4) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn join_columns(columns: &[String]) -> String {
    columns.join("_")
}

/// Assemble `<prefix>_<table>__<cols>[__<suffix>]`, replacing the column
/// segment with its hash if the result would exceed Postgres's limit.
fn assemble(prefix: &str, table: &str, columns: &[String], suffix: Option<&str>) -> String {
    let cols = join_columns(columns);
    let build = |cols: &str| match suffix {
        Some(s) => format!("{prefix}_{table}__{cols}__{s}"),
        None => format!("{prefix}_{table}__{cols}"),
    };
    let name = build(&cols);
    if name.len() <= MAX_IDENT {
        return name;
    }
    let hashed = build(&short_hash(&cols));
    if hashed.len() <= MAX_IDENT {
        return hashed;
    }
    // Even the table name is too long: hash the whole thing, keeping the
    // prefix so the object class stays readable in \d output.
    format!("{prefix}_{}", short_hash(&name))
}

pub fn primary_key(table: &str) -> String {
    let name = format!("pk_{table}");
    if name.len() <= MAX_IDENT {
        name
    } else {
        format!("pk_{}", short_hash(table))
    }
}

pub fn unique_constraint(table: &str, columns: &[String]) -> String {
    assemble("uq", table, columns, None)
}

/// Partial unique indexes carry the predicate hash, so editing the `where`
/// clause renames the index and therefore shows up in the diff (#25).
pub fn unique_partial_index(table: &str, columns: &[String], canonical_predicate: &str) -> String {
    assemble("uq", table, columns, Some(&short_hash(canonical_predicate)))
}

pub fn foreign_key(table: &str, columns: &[String]) -> String {
    assemble("fk", table, columns, None)
}

/// Table-level `check (...)`: numbered in declaration order, because the
/// expression is arbitrary and has no stable column set.
pub fn check_table(table: &str, columns: &[String], ordinal: usize) -> String {
    assemble("ck", table, columns, Some(&ordinal.to_string()))
}

/// Column rule (`minLength(2)`, `pattern(...)`, `min(0)`, …).
pub fn check_column(table: &str, column: &str, rule: &str) -> String {
    assemble(
        "ck",
        table,
        std::slice::from_ref(&column.to_string()),
        Some(rule),
    )
}

/// `method` distinguishes two indexes on the same columns — a btree and a
/// GIN over `label` are different objects and must not share a name.
/// `btree` (the default) contributes no segment, so ordinary indexes keep
/// the short form.
pub fn index(table: &str, columns: &[String], method: Option<&str>) -> String {
    match method_segment(method) {
        Some(m) => assemble("ix", table, columns, Some(&m)),
        None => assemble("ix", table, columns, None),
    }
}

pub fn index_partial(
    table: &str,
    columns: &[String],
    canonical_predicate: &str,
    method: Option<&str>,
) -> String {
    let h = short_hash(canonical_predicate);
    match method_segment(method) {
        Some(m) => assemble("ix", table, columns, Some(&format!("{m}_{h}"))),
        None => assemble("ix", table, columns, Some(&h)),
    }
}

fn method_segment(method: Option<&str>) -> Option<String> {
    match method {
        None => None,
        Some(m) if m.eq_ignore_ascii_case("btree") => None,
        Some(m) => Some(m.to_lowercase()),
    }
}

pub fn touch_function(table: &str) -> String {
    let name = format!("tg_{table}__touch");
    if name.len() <= MAX_IDENT {
        name
    } else {
        format!("tg_{}__touch", short_hash(table))
    }
}

/// Quote an identifier for DDL. Generated names are always plain
/// snake_case, but a physical-name override (`as "tbl_user_accounts"`) can
/// be anything.
pub fn quote_ident(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain && !is_reserved_sql(name) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// A short list of Postgres reserved words a JWC name can plausibly collide
/// with. Quoting one that does not need it is harmless; failing to quote one
/// that does produces a syntax error at deploy time.
fn is_reserved_sql(name: &str) -> bool {
    matches!(
        name,
        "all"
            | "analyse"
            | "analyze"
            | "and"
            | "any"
            | "array"
            | "as"
            | "asc"
            | "authorization"
            | "between"
            | "both"
            | "case"
            | "cast"
            | "check"
            | "collate"
            | "column"
            | "constraint"
            | "create"
            | "current_date"
            | "current_role"
            | "current_time"
            | "current_timestamp"
            | "current_user"
            | "default"
            | "deferrable"
            | "desc"
            | "distinct"
            | "do"
            | "else"
            | "end"
            | "except"
            | "false"
            | "for"
            | "foreign"
            | "from"
            | "grant"
            | "group"
            | "having"
            | "in"
            | "initially"
            | "intersect"
            | "into"
            | "leading"
            | "limit"
            | "localtime"
            | "localtimestamp"
            | "new"
            | "not"
            | "null"
            | "off"
            | "offset"
            | "old"
            | "on"
            | "only"
            | "or"
            | "order"
            | "placing"
            | "primary"
            | "references"
            | "returning"
            | "select"
            | "session_user"
            | "some"
            | "symmetric"
            | "table"
            | "then"
            | "to"
            | "trailing"
            | "true"
            | "union"
            | "unique"
            | "user"
            | "using"
            | "when"
            | "where"
            | "window"
            | "with"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_transform() {
        assert_eq!(physical("Accounts"), "accounts");
        assert_eq!(physical("InvoiceLines"), "invoice_lines");
        assert_eq!(physical("ApiKeys"), "api_keys");
        assert_eq!(physical("OrgWithMembers"), "org_with_members");
        assert_eq!(physical("created_at"), "created_at");
        assert_eq!(physical("MemberRole"), "member_role");
    }

    #[test]
    fn acronyms_split_at_the_last_capital() {
        // `APIKeys` reads as API + Keys: the split goes before the capital
        // that starts a lowercase run.
        assert_eq!(physical("APIKeys"), "api_keys");
        assert_eq!(physical("HTTPServer"), "http_server");
    }

    #[test]
    fn digits_start_a_new_word() {
        assert_eq!(physical("Address2Line"), "address2_line");
    }

    #[test]
    fn constraint_names_are_deterministic() {
        let cols = vec!["org_id".to_string(), "account_id".to_string()];
        assert_eq!(primary_key("members"), "pk_members");
        assert_eq!(
            unique_constraint("invites", &cols),
            "uq_invites__org_id_account_id"
        );
        assert_eq!(
            foreign_key("members", &cols),
            "fk_members__org_id_account_id"
        );
        assert_eq!(
            index("members", &cols, None),
            "ix_members__org_id_account_id"
        );
    }

    #[test]
    fn partial_names_carry_the_predicate_hash() {
        let cols = vec!["org_id".to_string()];
        let a = unique_partial_index("subscriptions", &cols, "status <> 'canceled'");
        let b = unique_partial_index("subscriptions", &cols, "status <> 'paid'");
        assert_ne!(a, b, "a different predicate must produce a different name");
        assert!(a.starts_with("uq_subscriptions__org_id__"));
        assert_eq!(a.len(), "uq_subscriptions__org_id__".len() + 8);
    }

    #[test]
    fn the_message_is_not_part_of_the_name() {
        // schema.md §8.3: editing a message must produce no DDL change.
        let cols = vec!["org_id".to_string()];
        assert_eq!(
            unique_partial_index("subscriptions", &cols, "status <> 'canceled'"),
            unique_partial_index("subscriptions", &cols, "status <> 'canceled'")
        );
    }

    #[test]
    fn long_names_hash_the_column_segment_rather_than_truncate() {
        let cols: Vec<String> = (0..12).map(|i| format!("a_very_long_column_{i}")).collect();
        let n = index("some_table", &cols, None);
        assert!(n.len() <= 63, "{n} is {} bytes", n.len());
        assert!(n.starts_with("ix_some_table__"));

        // Two different long column lists must not collide.
        let mut other = cols.clone();
        other.push("extra".into());
        assert_ne!(n, index("some_table", &other, None));
    }

    #[test]
    fn the_index_method_is_part_of_the_name() {
        // Two indexes on the same column with different methods are two
        // objects; a shared name makes the second CREATE INDEX fail.
        let cols = vec!["label".to_string()];
        assert_eq!(index("children", &cols, None), "ix_children__label");
        assert_eq!(
            index("children", &cols, Some("btree")),
            "ix_children__label"
        );
        assert_eq!(
            index("children", &cols, Some("gin")),
            "ix_children__label__gin"
        );
        assert_ne!(
            index("children", &cols, None),
            index("children", &cols, Some("gin"))
        );
    }

    #[test]
    fn identifiers_are_quoted_only_when_needed() {
        assert_eq!(quote_ident("accounts"), "accounts");
        assert_eq!(quote_ident("tblUserAccounts"), "\"tblUserAccounts\"");
        assert_eq!(quote_ident("order"), "\"order\"");
        assert_eq!(quote_ident("user"), "\"user\"");
    }
}
