---
sidebar_position: 2
title: Formatting
description: "What jwc fmt does, what it does not preserve, and why it re-prints from the AST instead of moving tokens."
---

# Formatting

```bash
jwc fmt                    # rewrite in place
jwc fmt src tests            # several inputs
jwc fmt src/app.jwc --stdout # print it, rewrite nothing
jwc fmt --check            # report and exit non-zero; write nothing
```

`--check` is the CI shape: it names every file that would change and
fails, without touching anything.

## It re-prints, it does not reformat

`jwc fmt` parses to an AST and prints the AST. It does not move
whitespace around your tokens.

The consequence worth knowing is that the output is a **fixed point by
construction** — `fmt(fmt(x)) == fmt(x)`, which the test suite checks
over the whole corpus. There is no configuration, because there is
nothing to configure: the printer has one way to write each node.

The other consequence is that **a file that does not parse is not
formatted**. `jwc fmt` reports the syntax error and leaves the file
alone, rather than half-rewriting it.

## What it normalises

```jwc no-compile
namespace f;
import  app ;

--- doc comment
service   S {
  function  g( a : int )  {
    -- a line comment
    let x=1;
    let y = not $a;   -- trailing
    if($x==1){return "one";}
    return "other";
  }
}
```

becomes

```jwc no-compile
namespace f;
import app;

--- doc comment
service S {
    function g(a: int) {
        -- a line comment
        let x = 1;
        let y = !$a;
        -- trailing
        if ($x == 1) {
            return "one";
        }
        return "other";
    }
}
```

| | |
|---|---|
| Indent | four spaces, never tabs |
| Blocks | always braced and always broken across lines |
| Binary operators | one space either side |
| `not x` | written `!x` — the two are one node |
| Consecutive one-liners | `import`, `static`, `const` pack together unless you separate them |
| Everything else | separated by one blank line |

`not` and `!` are the same AST node, so the printer has to pick one and
it picks `!`. Both spellings parse everywhere; only the formatted one is
fixed.

## What it preserves

**Doc comments (`---`) and line comments (`--`)** survive. The parser
attaches them to the declaration, statement, column or class field that
follows, and the printer writes them back in place — which is what keeps
a doc comment attached to its column, where `jwc migrate` turns it into a
Postgres `COMMENT ON`.

**Your blank lines** survive — both between two one-line declarations, so
grouping imports stays possible, and between statements inside a block.
What the printer decides is where a blank line is *required*, not where
one is allowed.

## What it does not preserve

**A trailing comment on the same line as code.** It is attached to the
*next* item and comes back on its own line above it, as in the example
above. Nothing is lost, but the comment moves.

**Your line breaks inside an expression.** A long query is printed with
one clause per line, whatever you wrote, and a short one stays on one
line however you broke it.

Since 0.9.942 the same holds for the three constructs that grow — an
array, a record literal and an argument list. Each is one line when it
fits inside **92 columns** at its indent, and broken one item per line
when it does not:

```jwc no-compile
return content("text/plain; charset=utf-8", string.join([
    "User-agent: *",
    "Allow: /",
    "Disallow: /api/"
], "\n"));
```

The bracket hugs its call and the remaining arguments ride the closing
line, which is what a person writes by hand. When that does not fit
either, every argument goes on its own line. Anything else — a `+` chain,
a ternary, a long identifier chain, a single long string — stays on one
line however far it runs: breaking one is a claim about which half
matters, and the printer does not have an opinion to express.

Until 0.9.942 there was no rule at all outside a query and an `insert`.
jwc-shortener's `robots.txt` route was a `string.join` over 36 short
strings and `jwc fmt` printed it as one **1608-column** line.

**A comment written *inside* a record literal, a `server { }` body or an
`insert` value list.** The AST carries a comment on a declaration or a
statement — `Attached` — and those three are neither, so there is nowhere
in the tree for the text to be. A multi-line record printer now exists,
so there is somewhere to *put* one; what is still missing is the parser
keeping it.

`jwc fmt` will not delete it. It compares the comments in the file against
the comments in what it would write, and if any would be lost it leaves
the file alone and says which:

```
./src/app.jwc: not formatted — 4 comments would be lost:
    -- `request.client_ip()` walks `X-Forwarded-For` right to left and
    …
```

Move the comment above the enclosing statement and the file formats.
Until 0.9.941 it was deleted and the run reported success.

## In a pre-commit hook

```bash
jwc fmt --check && jwc check --deny-warnings
```

Both are offline: the schema is in the source, so neither needs a
database or a network.
