---
name: Bug report
about: Something is wrong — a crash, a wrong verdict, a bad record
title: ""
labels: bug
---

**What happened** vs what you expected.

**Input**: attach the case JSON / record (or describe it). If the case
is private, a minimal reproducing case is enough — the schemas are
public.

**Versions**: `optcoil --version`, OS, and whether you're on a release
binary or a source build.

**Verdict semantics**: if the issue is a verdict you disagree with,
say which outcome you expected (`PASS`/`FAIL`/`INCONCLUSIVE`/
`NOT_EVALUATED`) and why — `INCONCLUSIVE` naming its blocking reason is
usually intended behavior, not a bug.
