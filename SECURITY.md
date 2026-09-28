# Security policy

## Reporting a vulnerability

Report suspected vulnerabilities through GitHub's private vulnerability
reporting ("Report a vulnerability" on the Security tab), or email
Connoravila@gmail.com. Please do not open public issues for
unconfirmed vulnerabilities.

## Scope

Converra parses attacker-influenced input by design: case JSON, run
records, dataset bundles and field maps are all document formats that
flow through `optcoil-model`/`optcoil-search` validation and the
independent verifier. Panics, hangs or incorrect verdicts on crafted
documents are security-relevant bugs — please report them.

Physics verdicts (`PASS`/`FAIL`/`INCONCLUSIVE`/`NOT_EVALUATED`) are
screening outputs for engineering review, not a safety boundary.
A wrong verdict is a correctness bug; the safety-relevant case is a
toolchain that silently converts `FAIL`/`INCONCLUSIVE` into `PASS`
against the documented semantics.

## Supported versions

Only the latest release receives fixes — this is a pre-1.0 project.
Check the newest tag before reporting a fixed bug.

## Supply chain

Dataset bundles are content-bound and issuer-attested (ed25519); the
verifier rejects tampered bundles. Run records bind inputs by SHA-256.
Dependency policy: versions are pinned, `Cargo.lock` is committed, and
new dependencies should be released for at least a week before use.
