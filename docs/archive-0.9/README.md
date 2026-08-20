# Archive — the 0.9.x documentation

This directory documents the language JWC had **before** the v0.25.0
cutover. That language no longer compiles: `entity`, `dbcontext`, `with`,
`via`, `validate body`, `new … from`, `patch`, `group`, `mount` and `dome`
were all removed, and the compiler now names their replacement rather than
accepting them (routing.md §10).

It is kept because 0.9.x binaries are deployed and this is what they
implement. It is not a description of the current compiler and it is not
checked against one.

The current language is specified in **`docs/spec/v1/`**. The docs site is
rewritten from that spec in v1.0.0-rc.1 (ROADMAP), and this directory goes
away when it is.
