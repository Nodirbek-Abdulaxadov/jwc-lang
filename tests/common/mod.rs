//! Shared test helpers. Not a test target — `tests/common/` is a
//! subdirectory, so cargo compiles it only where it is `mod`-ed in.

#![allow(dead_code)] // each including binary uses a different subset

/// The psql arguments that name `db` on the server `conn` describes.
///
/// Both documented forms of the connection variable have to work, and they
/// need opposite treatment. With the flag form (`-h … -p … -U …`) a
/// trailing `-d db` is the database. With a URI it is **not**: psql applies
/// the later `-d` as a whole new connection target, discarding the host,
/// port and user the URI carried, and falls back to the default unix
/// socket. Three golden suites did exactly that and had never been run
/// against a URI to notice — all of them are opt-in on an environment
/// variable, and an unset variable prints SKIPPED.
pub fn psql_target(conn: &str, db: &str) -> Vec<String> {
    let Some((scheme, rest)) = conn.split_once("://") else {
        let mut v: Vec<String> = conn.split_whitespace().map(str::to_string).collect();
        v.push("-d".into());
        v.push(db.into());
        return v;
    };
    // `authority[/dbname][?params]` — keep the params, replace the name.
    let (authority, params) = match rest.split_once('?') {
        Some((a, p)) => (a, format!("?{p}")),
        None => (rest, String::new()),
    };
    let authority = authority.split('/').next().unwrap_or(authority);
    vec![format!("{scheme}://{authority}/{db}{params}")]
}

/// Run psql against `db` on `conn` and return stdout + stderr together.
pub fn run_psql(conn: &str, db: &str, args: &[&str]) -> String {
    let mut cmd = std::process::Command::new("psql");
    for part in psql_target(conn, db) {
        cmd.arg(part);
    }
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("psql");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[cfg(test)]
mod tests {
    use super::psql_target;

    #[test]
    fn a_uri_target_keeps_its_host_and_a_flag_target_keeps_its_flags() {
        assert_eq!(
            psql_target("postgres://jwc@127.0.0.1:5432/jwctest", "golden_0"),
            vec!["postgres://jwc@127.0.0.1:5432/golden_0"]
        );
        assert_eq!(
            psql_target("postgres://jwc@h:5432/old?sslmode=disable", "golden_0"),
            vec!["postgres://jwc@h:5432/golden_0?sslmode=disable"]
        );
        // No database in the URI at all, which is legal.
        assert_eq!(
            psql_target("postgres://jwc@h:5432", "golden_0"),
            vec!["postgres://jwc@h:5432/golden_0"]
        );
        assert_eq!(
            psql_target("-h /tmp -p 5432 -U jwc", "golden_0"),
            vec!["-h", "/tmp", "-p", "5432", "-U", "jwc", "-d", "golden_0"]
        );
    }
}
