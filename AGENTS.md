# AGENTS.md

This guide defines commenting and documentation practices for human contributors and AI coding agents.

## Project-specific guidance

This repository contains a Rust CLI that normalizes Google Cloud Storage object
names and uploads files from `uploads/`. The rules below capture constraints
that are easy to lose when changing the implementation.

### Execution and toolchain

-   Use the tasks in `.mise.toml` as the canonical development and workflow entry points.
    On the host, they build and run the `app` service; inside the application container,
    they run the tool directly. Keep this boundary so development does not require
    nested Docker.
-   The `app` image uses an inline Dockerfile. Keep the source copy after the pinned
    toolchain and audit/lint tool installation so source edits do not invalidate the
    expensive preparation layer. Update `tests/ci_configuration_test.rs` when this
    build contract changes.
-   The `googlecloud` service owns authentication and is intentionally ephemeral. Do
    not add persistent credential mounts or a logout flow without updating the
    authentication lifecycle in `README.md` and its regression tests.
-   The application talks to the Cloud Storage JSON API directly. Use the dedicated
    Cloud SDK container only for authentication and access-token retrieval, through
    its host-verified SSH channel.
-   The short `googlecloud` healthcheck interval is intentional: the default Docker
    healthcheck delay made `app` startup unnecessarily slow. Preserve the startup
    parameters and the configuration regression test when changing Compose healthchecks.

### Workflow invariants

-   Both `normalize` and `upload` must configure the requested project before
    interactive authentication, and authenticate before acquiring any Cloud Storage
    bucket lock. Acquiring a lock itself needs a token, and the login message must
    identify the requested project.
-   Treat each workflow as a transaction. Complete validation and name planning before
    the first mutation; stage remote changes under run-specific temporary names; record
    the generation of every owned object; finalize only with generation preconditions;
    and roll back in the reverse dependency order. A change to these phases requires
    success, partial-failure, and interruption coverage.
-   Hold each bucket lock across the complete remote snapshot or transaction, including
    finalization and rollback. Acquire multiple buckets in a stable order, reuse locks
    for nested calls, and never treat `.task-googlecloud-lock` as user data. All writers
    touching a bucket must honor this protocol.
-   Cloud Storage copy, rewrite, delete, and cleanup operations must be
    generation-guarded. A timeout, interruption, malformed response, or other failure
    after a request may have started is not proof that the remote state is unchanged:
    confirm ownership and generations before retrying or rolling back. If ownership
    cannot be established, preserve the recovery state and require manual recovery.
-   Retry only bounded connection-establishment failures identified by
    `reqwest::Error::is_connect()`. Rebuild streamed upload requests for each attempt;
    do not blindly retry failures after request transmission may have started.
-   Upload planning must remain tied to discovery. The one-level `uploads/<bucket>/`
    layout, hidden-file exclusion, symlinked-root/entry exclusion or rejection,
    root/directory/file identity checks, and content fingerprint check protect against
    path replacement and in-place source changes during a stream. Do not replace these
    checks with an unchecked pathname read.
-   Local normalization is NFC-based and must reject collisions before changing local
    or remote state. Renames are atomic and no-replace; retain the identity checks and
    the Linux/macOS Unicode-alias handling, including when the application runs in a
    Linux container over a macOS filesystem.
-   Keep Cloud Storage object names parsed and encoded through `ObjectPath` and the
    centralized validation helpers. Do not interpolate raw object names into API URLs,
    bypass the 1024-byte limit, or reuse another run's temporary or staging object
    without confirming its generation and contents.
-   Local source errors that occur before an HTTP request can be handled as local
    failures. Once a request may have started, classify the error as remote-state
    uncertainty and preserve the confirmation and recovery path; do not simplify this
    distinction for convenience.

### Tests and documentation

-   Keep tests under `tests/`; source modules include those files only as a layout
    convention. Storage transition tests should use a finite-timeout local HTTP server,
    assert the complete request line and request count, and cover success, partial
    responses, HTTP failures, interrupted requests, and rollback ownership.
-   For changes to filesystem safety, cover both normal operation and replacement races
    or symlink/dangling-entry cases on the supported Linux/macOS paths. For transaction
    changes, assert rollback order and generation propagation rather than only checking
    that an error was returned.
-   Run the relevant `mise` checks, including `fmt-check`, `clippy`, `audit`, `deny`,
    `test`, `markdownlint`, and `shellcheck`. Keep CI workflow configuration and its
    tests in sync when adding or moving a check.
-   Update `README.md` whenever a change affects command usage, credential lifetime,
    user-visible progress, temporary object names, or the manual recovery procedure.
    Keep recovery instructions conservative: never recommend deleting or restoring a
    remote object until the run's ownership and generation have been verified.

## Core principle

Code should make the mechanics understandable. Comments should preserve the context
that the code cannot express: **why** a decision was made, what constraints apply,
and what future maintainers must not accidentally change.

Do not add comments merely to increase comment coverage. Prefer clear code first, then add a concise comment for the remaining non-obvious rationale.

## Prefer clarity before comments

Before writing a comment, consider whether the same problem is better solved by:

1.  Giving a variable, constant, function, or class a precise name.
2.  Extracting a complex block into a focused function or class.
3.  Representing an invariant or valid state with a type, value object, or validation rule.
4.  Replacing a magic value with a named constant.

Use a comment when the code is already as clear as practical but the reason, external context, or non-local constraint still cannot be expressed in the code itself.

## What comments should explain

Add or update comments when they communicate information that a reader cannot reliably infer from the implementation, including:

-   **Intent and rationale:** why this approach is necessary or preferable.
-   **Constraints:** requirements imposed by an API, database, platform, protocol, compatibility target, security rule, or performance limit.
-   **Business rules:** the requirement, policy, or domain rule represented by the code, especially at boundaries and exceptions.
-   **Magic numbers and thresholds:** what a value means and why that value is correct.
-   **Public API contracts:** important preconditions, postconditions, defaults, failure behavior, and compatibility expectations.
-   **Side effects:** mutation, I/O, caching, event publication, persistence, retries, external calls, or other effects callers need to understand.
-   **Complex algorithms:** the purpose, invariant, or high-level strategy, particularly when the implementation is not obvious.
-   **Workarounds:** the problem being avoided, its scope, and the condition or issue that allows the workaround to be removed.
-   **Non-obvious safety or correctness decisions:** idempotency, ordering, race prevention, data retention, privacy, or error-handling choices.

Do not comment code that is self-explanatory. A comment such as `// increment the
count` or `// fetch the user` adds noise and can become stale without helping the
reader.

## Good and bad examples

### Explain intent, not mechanics

Good:

```ts
// The API may deliver a retry several seconds after the original request,
// so requestId—not the timestamp—is the stable identity for deduplication.
if (processedRequestIds.has(request.id)) {
  return;
}
```

Bad:

```ts
// Check whether the request ID has already been processed.
if (processedRequestIds.has(request.id)) {
  return;
}
```

### Document business rules and boundaries

Good:

```ts
// The free plan allows at most three projects per calendar day. Keep this
// limit here so changes to the product rule are visible at the enforcement point.
const DAILY_PROJECT_LIMIT = 3;
```

Bad:

```ts
// Set the limit to 3.
const DAILY_PROJECT_LIMIT = 3;
```

### Document side effects and retention decisions

Good:

```ts
// Audit records must remain available after account deletion, so anonymize the
// user reference instead of cascading the records away.
await anonymizeAuditLogs(userId);
```

Bad:

```ts
// Anonymize the audit logs.
await anonymizeAuditLogs(userId);
```

### Explain workarounds and their removal conditions

Good:

```ts
// Safari 17 can report an intermediate size during layout. Deferring one
// animation frame lets observers receive the settled dimensions.
// TODO(PROJ-1234): Remove this workaround when Safari 18 is the minimum version.
requestAnimationFrame(updateLayout);
```

Bad:

```ts
// TODO: fix this later.
requestAnimationFrame(updateLayout);
```

## Public APIs, functions, and classes

Document a public API when its name and types do not fully communicate the contract. Include only information callers need, such as:

-   the purpose and important usage constraints;
-   meaningful argument and return-value semantics;
-   side effects and state changes;
-   errors or exceptional conditions callers may need to handle;
-   performance, ordering, idempotency, or concurrency requirements.

Do not write documentation that simply repeats parameter names or the function body.
Keep documentation close to the API it describes and follow the language's standard
documentation format.

For private functions and classes, add a comment or docstring when the responsibility, invariant, business rule, or side effect is not clear from the name and implementation.

## Business rules and magic values

When code encodes a domain rule, make the rule visible at the point where it is
enforced. If the rule has a source of truth, reference a stable issue, requirement,
or specification identifier when available.

For every non-obvious numeric or string literal, prefer a named constant or type. Add
a comment when the value's meaning or origin still cannot be understood from its name.

```ts
// Keep the grace period aligned with the billing policy: access remains active
// until the end of the day following a failed renewal.
const RENEWAL_GRACE_PERIOD_DAYS = 1;
```

Do not use a comment to justify an unexplained value when the value can be made self-documenting through naming or domain modeling.

## Workarounds and temporary code

Every workaround or temporary exception must state:

1.  What problem it avoids.
2.  Why the normal approach cannot be used.
3.  How it can be removed or what condition makes it obsolete.
4.  A stable issue, ticket, or source link when one exists.

Use a specific TODO rather than a vague placeholder. For example:

```ts
// TODO(TEAM-482): Replace polling with the provider webhook once webhook
// delivery is available for archived accounts.
```

Do not invent an issue number, historical reason, or removal condition. If the
rationale is not discoverable from the codebase, tests, documentation, or version
history, investigate further or ask the user before documenting it as fact.

## Keeping comments synchronized

Comments are part of the implementation and must remain accurate.

-   When changing code, inspect and update nearby comments, docstrings, TODOs, and examples.
-   Remove comments whose rationale no longer applies; do not preserve them out of caution.
-   If a code change invalidates a documented constraint or business rule, update the documentation and its source reference together.
-   Avoid comments that duplicate a value, control flow, or identifier likely to change frequently.
-   Before finishing, reread every changed comment against the final code and confirm that it explains the current behavior and rationale.

## Tests

-   Place test code under the `tests` directory; do not place it under the `src` directory.

## Guidance for AI coding agents

When modifying code:

1.  Read the surrounding implementation, tests, documentation, and relevant source-of-truth references before inferring intent.
2.  Preserve existing comments that remain accurate, and revise or remove comments made stale by the change.
3.  Prefer naming, function extraction, types, validation, and tests over explanatory comments for code that is difficult to read.
4.  Add comments only for non-obvious intent, constraints, business rules, side effects, algorithms, or temporary workarounds.
5.  Keep comments concise and place them next to the decision or invariant they explain.
6.  Never claim a reason that has not been verified. Clearly identify uncertainty and request clarification when the missing context affects correctness.
7.  Treat TODOs as actionable maintenance records: describe the required change and include an issue or removal condition when available.
8.  As a final review step, check that comments do not merely restate the code and that no changed comment contradicts the implementation.

The goal is not to comment every line. The goal is to leave the next developer with the reasoning and constraints they would otherwise have to rediscover.
