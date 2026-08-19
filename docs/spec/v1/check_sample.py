#!/usr/bin/env python3
"""Sample <-> spec conformance scan.

Reads the sample project, classifies every construct it uses, and writes
spec-coverage.json mapping each construct to the normative clause that
defines it.  A construct with no clause is reported as `unspecified` and
makes the script exit non-zero -- that is the v0.20.0 done-criterion
(ROADMAP: "spec-coverage.json da 0 ta unspecified").

This is a lexical scan, not a parser.  The parser arrives in v0.21.0 and
tests/parse_corpus/ replaces the syntax half of this file; the clause map
stays.
"""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent
SAMPLE = ROOT / "sample"

# construct id -> (regex, spec clause)
CONSTRUCTS = [
    ("namespace",             r"^namespace\s",                       "names §6.1"),
    ("import.namespace",      r"^import\s+(?!redis|mail)",           "names §6.2"),
    ("import.package",        r"^import\s+(redis|mail);",            "names §6.2.3"),
    ("database",              r"^database\s+\w+\s*:",                "config §2"),
    ("database.init",         r"^\s*init\(\)\s*\{",                  "config §2.3"),
    ("schema",                r"^schema\s+\w+\s+of\s",               "schema §1"),
    ("server",                r"^server\s*\{",                       "config §3"),
    ("server.cors",           r"^\s*cors\s*\{",                      "config §3.4"),
    ("error.decl",            r"^error\s+\w+.*=\s*\d+",              "errors §1.1"),
    ("errorHandler",          r"^errorHandler\s*\(",                 "errors §4.1"),
    ("errorHandler.arm",      r"^\s*catch\s+\w+\s*\(",               "errors §4.2"),
    ("errorHandler.fault_arm", r"^\s*catch\s*\(",                    "errors §4.4"),
    ("table",                 r"^table\s+\w+\s+of\s",                "schema §1"),
    ("doc_comment",           r"^\s*---\s",                          "schema §7"),
    ("column.nullable",       r"^\s+\w+\s+[\w()\[\], ]+\?\s*[;,]",   "types §6.1"),
    ("column.identity",       r"\bprimary key identity\b",           "schema §2.3"),
    ("column.private",        r"\bprivate\b",                        "schema §3.1"),
    ("column.server",         r"\bserver;",                          "schema §3.2"),
    ("column.default",        r"\bdefault\s+\S",                     "schema §2.4"),
    ("column.on_update",      r"\bon update now\(\)",                "schema §6"),
    ("column.unique_msg",     r"\bunique\s*:\s*\"",                  "errors §6.1"),
    ("column.rule",           r"\b(minLength|pattern|min|max)\(",    "schema §4.4"),
    ("type.numeric",          r"\bnumeric\(\d+,\s*\d+\)",            "types §2.1"),
    ("type.bigint",           r"\bbigint\b",                         "types §2.3"),
    ("type.jsonb",            r"\bjsonb\b",                          "types §5.6"),
    ("type.inet",             r"\binet\b",                           "types §2.1"),
    ("type.array",            r"\b\w+\[\]",                          "types §2.1"),
    ("type.record_inline",    r"->\s*\{",                            "types §1"),
    ("constraint.pk_table",   r"^\s*primary key\s*\(",               "schema §4.1"),
    ("constraint.fk",         r"^\s*foreign key\s*\(",               "schema §4.2"),
    ("constraint.fk_action",  r"\bon delete (cascade|set null)",     "schema §4.2"),
    ("constraint.unique",     r"^\s*unique\s*\(",                    "schema §4.3"),
    ("constraint.unique_partial", r"^\s*unique\s*\([^)]*\)\s*where", "schema §4.3"),
    ("constraint.check",      r"^\s*check\s*\(",                     "schema §4.4"),
    ("index",                 r"^\s*index on\s*\(",                  "schema §1"),
    ("index.partial",         r"^\s*index on\s*\([^)]*\)\s*where",   "schema §4.3"),
    ("index.desc",            r"^\s*index on\s*\(.*\bdesc\b",        "schema §4.3"),
    ("enum.typed",            r"^enum\s+\w+\s+of\s",                 "schema §5.2"),
    ("enum.member_ref",       r"\b[A-Z]\w+\.[a-z_]+\b(?!\()",        "types §3.3"),
    ("class",                 r"^class\s+\w+\s*\{",                  "types §4"),
    ("class.rule_required",   r"\brequired\b",                       "types §11.1"),
    ("class.rule_minItems",   r"\bminItems\(",                       "types §11.1"),
    ("class.nested_array",    r"^\s+\w+\s+\w+\[\]\s+required",       "types §11.4"),
    ("view",                  r"^view\s+\w+\s+of\s",                 "queries §8.1"),
    ("service",               r"^service\s+\w+\s*\{",                "types §10"),
    ("function.free",         r"^function\s+\w+\s*\(",               "names §6.4"),
    ("function.typed_params", r"^\s*function\s+\w+\([\w\s:,?\[\]().]*\w+:\s*\w", "types §10.1"),
    ("function.return_annot", r"^\s*function\s+\w+\(.*\)\s*->",      "types §10.2"),
    ("middleware",            r"^middleware\s+\w+",                  "middleware §1"),
    ("middleware.binder",     r"^middleware\s+\w+\(@\w+\s*:",        "middleware §2"),
    ("middleware.requires",   r"^\s*requires\s+\w+",                 "middleware §3"),
    ("middleware.provides",   r"^\s*provides\s+\w+\s*:",             "middleware §6.1"),
    ("middleware.after",      r"^\s*after\s*\{",                     "middleware §5"),
    ("middleware.bare_return", r"^\s*(if\s*\(.*\)\s*\{\s*)?return;",  "middleware §5.3"),
    ("context.read",          r"context\.\w+(?!\?)\b(?!\s*=)",       "middleware §6.2"),
    ("context.read_opt",      r"context\.\w+\?",                     "middleware §6.3"),
    ("context.write",         r"context\.\w+\s*=",                   "middleware §6.4"),
    ("routes",                r"^routes\s+\"",                       "routing §1"),
    ("routes.use",            r"^routes\s+\"[^\"]*\"\s*\n?\s*use\s", "middleware §4.1"),
    ("route",                 r"^\s*route\s+[A-Z]+\s+\"",            "routing §1"),
    ("route.use",             r"^\s*route\s+[A-Z]+\s+\"[^\"]*\"\s+use\s", "middleware §4.1"),
    ("path_param.typed",      r"\{\w+\s*:\s*\w+\}",                  "routing §3.1"),
    ("path_param.ref",        r"@\w+",                               "names §5.2"),
    ("local.sigil",           r"\$\w+",                              "names §5.3"),
    ("let",                   r"^\s*let\s+\w+\s*=",                  "names §5.5"),
    ("select",                r"\bselect\s+\w+\s+from\s",            "queries §1"),
    ("select.first",          r"^\s*first\b",                        "queries §5.1"),
    ("select.where",          r"^\s*where\s",                        "queries §3"),
    ("select.optional_pred",  r"==\?",                               "queries §3.2"),
    ("select.orderby",        r"^\s*orderby\s",                      "queries §5.3"),
    ("select.projection",     r"^\s*as\s*\{",                        "queries §6.1"),
    ("select.nested_proj",    r"^\s*\w+:\s*\{",                      "queries §6.1"),
    ("select.group_by",       r"^\s*group by\s",                     "queries §6.2"),
    ("select.page",           r"^\s*page\s+after\s",                 "queries §9.2"),
    ("join.left_one",         r"left join .* as one \w+",            "queries §4.3"),
    ("join.left_many",        r"left join .* as many \w+",           "queries §4.3"),
    ("join.alias",            r"left join\s+\w+\.\w+\.\w+\s+\w+\s+on", "queries §4.2"),
    ("join.bare",             r"left join (?!.*\bas (one|many)\b).*\bon\b",  "queries §6.2"),
    ("join.child_order",      r"as many \w+ orderby",                "queries §4.6"),
    ("join.child_limit",      r"as many \w+ orderby .*limit",        "queries §4.6"),
    ("aggregate",             r"\b(count|sum|min|max|avg)\(",        "queries §6.3"),
    ("aggregate.filter",      r"\b(count|sum|min|max)\([^)]*\bwhere\b", "queries §6.3"),
    ("insert",                r"\binsert into\s",                    "writes §2"),
    ("insert.returning",      r"\}\s*as\s*\{",                       "writes §2.2"),
    ("insert.on_conflict",    r"on conflict\s*\([^)]*\)\s*do nothing", "writes §2.3"),
    ("update",                r"^\s*(let\s+\w+\s*=\s*)?update\s+App\.", "writes §3"),
    ("update.first",          r"^\s*first\b",                        "writes §3.2"),
    ("delete",                r"\bdelete from\s",                    "writes §5"),
    ("spread",                r"\.\.\.\$\w+",                        "types §9"),
    ("spread.except",         r"\.\.\.\$\w+\s+except\s",             "types §9.1"),
    ("transaction",           r"^\s*transaction\s*\{",               "writes §7"),
    ("or_throw",              r"^\s*or throw\s+\w+",                 "errors §5"),
    ("throw",                 r"^\s*throw\s+\w+",                    "errors §2.1"),
    ("catch_postfix",         r"\}\s*catch\s+\w+\s*\(\w+\)\s*\{",    "errors §7"),
    ("for",                   r"^\s*for\s*\(\w+\s+in\s",             "types §12.5"),
    ("ternary",               r"\?\s.*\s:\s",                        "types §12"),
    ("coalesce",              r"\?\?",                               "types §6.6"),
    ("response.json",         r"\bjson\(",                           "routing §6.1"),
    ("response.created",      r"\bcreated\(",                        "routing §6.1"),
    ("response.noContent",    r"\bnoContent\(",                      "routing §6.1"),
    ("response.statusCode",   r"\bstatusCode\(",                     "routing §6.1"),
    ("response.internalError", r"\binternalError\(",                 "routing §6.1"),
    ("response.with_headers", r"\bwith\s*\{\s*\"",                   "routing §6.2"),
    ("request.body_as",       r"request\.body\(\)\s+as\s+\w+",       "routing §5.2"),
    ("request.raw_body",      r"request\.raw_body\(\)",              "routing §5.1"),
    ("request.header",        r"request\.header\(",                  "routing §5.3"),
    ("request.query",         r"request\.query\(",                   "routing §5.3"),
    ("request.method",        r"request\.method\(\)",                "builtins §7"),
    ("request.route",         r"request\.route\(\)",                 "routing §5.4"),
    ("request.client_ip",     r"request\.client_ip\(\)",             "routing §5.4"),
    ("response.status",       r"response\.status\(\)",               "middleware §5.1"),
    ("builtin.env",           r"\benv\(",                            "builtins §2"),
    ("builtin.coercion",      r"\b(int|bigint|numeric|boolean|uuid)\(\$?\w", "types §7.2"),
    ("builtin.enum_coercion", r"\benum\(\w+,",                       "builtins §2"),
    ("builtin.date",          r"\bdate\.\w+\(",                      "builtins §3"),
    ("builtin.string",        r"\bstring\.\w+\(",                    "builtins §4"),
    ("builtin.array",         r"\barray\.\w+\(",                     "builtins §5"),
    ("builtin.hash",          r"\bhash\.\w+\(",                      "builtins §6"),
    ("builtin.jwt",           r"\bjwt\.\w+\(",                       "builtins §6"),
    ("builtin.crypto",        r"\bcrypto\.\w+\(",                    "builtins §6"),
    ("builtin.package",       r"\b(redis|mail)\.\w+\(",              "builtins §8"),
    ("builtin.serve",         r"\bserve\(",                          "builtins §2"),
    ("test",                  r"^test\s+\"",                         "DEFERRED-11"),
    ("test.assert",           r"^\s*assert\s+\$",                    "DEFERRED-11"),
    ("test.assert_fails",     r"^\s*assert fails\s+\w+\s*\{",        "DEFERRED-11"),
]

# Constructs that MUST NOT appear: the removed vocabulary and the
# invented-but-rejected surface.
FORBIDDEN = [
    (r"\bentity\s+\w+\s*\{",        "routing §10 — 'entity' removed"),
    (r"\bdbcontext\b",              "routing §10 — 'dbcontext' removed"),
    (r"\bvalidate\s+body\b",        "routing §10 — 'validate body' removed"),
    (r"\bnew\s+\w+\s+from\b",       "routing §10 — 'new X from Y' removed"),
    (r"\bvia\b",                    "routing §10 — 'via' removed"),
    (r"\bmount\b",                  "routing §10 — 'mount' removed"),
    (r"(?<![.\w])now\(\)",           "types §2.4 — bare now() in code"),
    (r"context\.(get|set)\(",       "middleware §6.1 — untyped context"),
    (r"=>",                         "ROADMAP §8 — lambdas"),
    (r"\bselect\s+from\b",          "names §5.4 — missing query binder"),
    (r"\brandom_token\(",           "builtins §10 — use crypto.token"),
    (r"\bsend_email\(",             "builtins §10 — use the mail package"),
    (r"\blog_insert\(",             "builtins §10 — use insert into"),
    (r"\bverify_signature\(",       "builtins §10 — use hash.hmac_verify"),
    (r"(?<![.\w])days\(",           "builtins §10 — use date.days"),
    (r"\bseed\.\w+",                "DEFERRED-11 — no shared fixtures"),
]


def is_line_comment(line: str) -> bool:
    """A `--` line comment, but not a `---` doc comment."""
    stripped = line.lstrip()
    return stripped.startswith("--") and not stripped.startswith("---")


def main() -> int:
    files = sorted(SAMPLE.rglob("*.jwc"))
    if not files:
        print("no sample files found", file=sys.stderr)
        return 2

    seen: dict[str, list[str]] = {}
    violations: list[str] = []

    for path in files:
        rel = str(path.relative_to(SAMPLE))
        text = path.read_text()
        lines = text.splitlines()

        for cid, pattern, clause in CONSTRUCTS:
            rx = re.compile(pattern, re.MULTILINE)
            for i, line in enumerate(lines, 1):
                if is_line_comment(line):
                    continue
                if rx.search(line):
                    seen.setdefault(cid, []).append(f"{rel}:{i}")
                    break

        for pattern, why in FORBIDDEN:
            rx = re.compile(pattern)
            for i, line in enumerate(lines, 1):
                if is_line_comment(line):
                    continue
                # `default now()` and `on update now()` are schema clauses,
                # not application code (types §2.4, schema §2.4/§6).
                probe = re.sub(r"\b(default|on update)\s+now\(\)", "", line)
                if rx.search(probe):
                    violations.append(f"{rel}:{i}: {why}\n    {line.strip()}")

    # Every clause reference must resolve to a heading that exists.
    docs = {}
    for md in sorted(ROOT.glob("*.md")):
        docs[md.stem] = md.read_text()

    def clause_exists(clause: str) -> bool:
        if clause.startswith("DEFERRED-"):
            return clause in docs.get("DEFERRED", "")
        if clause.startswith("ROADMAP"):
            return True
        m = re.fullmatch(r"(\w[\w-]*) §([\d.]+)", clause)
        if not m:
            return False
        doc, num = m.group(1), m.group(2)
        body = docs.get(doc)
        if body is None:
            return False
        top = num.split(".")[0]
        # a heading "## N." or "### N.M" must exist for the top level
        return re.search(rf"^#+ {re.escape(top)}\.? ", body, re.MULTILINE) is not None

    dangling = sorted({c for _, _, c in CONSTRUCTS if c and not clause_exists(c)})

    clause_of = {cid: clause for cid, _, clause in CONSTRUCTS}
    entries = []
    unspecified = []
    for cid in sorted(seen):
        clause = clause_of[cid]
        entry = {
            "construct": cid,
            "clause": clause,
            "uses": len(seen[cid]),
            "first_use": seen[cid][0],
        }
        if not clause:
            entry["clause"] = "unspecified"
            unspecified.append(cid)
        entries.append(entry)

    unused = [cid for cid, _, _ in CONSTRUCTS if cid not in seen]

    out = {
        "generated_by": "docs/spec/v1/check_sample.py",
        "dangling_clauses": dangling,
        "sample": "docs/spec/v1/sample",
        "files": len(files),
        "constructs": len(entries),
        "unspecified": len(unspecified),
        "coverage": entries,
        "declared_but_unused": unused,
    }
    (ROOT / "spec-coverage.json").write_text(json.dumps(out, indent=2) + "\n")

    print(f"files:        {len(files)}")
    print(f"constructs:   {len(entries)}")
    print(f"unspecified:  {len(unspecified)}")
    if unused:
        print(f"unused ids:   {', '.join(unused)}")
    if violations:
        print("\nFORBIDDEN CONSTRUCTS:")
        for v in violations:
            print("  " + v)
    if unspecified:
        print("\nUNSPECIFIED:", ", ".join(unspecified))
    if dangling:
        print("\nDANGLING CLAUSE REFERENCES:")
        for d in dangling:
            print("  " + d)

    return 1 if (violations or unspecified or dangling) else 0


if __name__ == "__main__":
    sys.exit(main())
