# Fork backlog

Fork-side improvements we want in Exograph itself, found while building on it in
`vreps-exo` / `jrnba_app`. GitHub Issues are disabled on this fork, so this file
is the backlog.

> **Do not file any of these upstream.** Nothing in this repo gets sent to the
> public `exograph/exograph` — no issues, comments, or PRs. See
> [CLAUDE.md](CLAUDE.md).

Each entry records what we hit, why it matters, and what we currently do about
it, so the workaround can be removed when the fix lands.

---

## 1. `update` mutations fail closed when the access predicate requires a join

**Status:** open — worked around in `vreps-exo`
**Found:** 2026-08-25, chasing a coach-facing "Not authorized" in jrnba_app#253

An `update` access predicate that can only be proven through a relation
traversal is not enforced — it is rejected outright. The mutation returns
`Not authorized` to every non-privileged caller **even when the predicate is
unambiguously true** for the target row.

`query` and `delete` with the identical predicate shape work correctly. It is
`update` specifically.

```
@access(
  update = (AuthContext.role == "admin" || self.tag.owner_uuid == AuthContext.userId)
)
type PublicTraining {
  @pk uuid: Uuid
  tag_uuid: Uuid
  @manyToOne @column("tag_uuid") tag: Tag?
  show_filter_tags: Boolean
}
```

Measured against a caller who owns the tag, so the predicate holds:

| Operation | Predicate    | Result                             |
| --------- | ------------ | ---------------------------------- |
| `query`   | `self.tag.*` | works                              |
| `delete`  | `self.tag.*` | works                              |
| `update`  | `self.tag.*` | **Not authorized**, even when true |

Confirmed in SQL that the predicate evaluates true for the row, and that the
same rule anchored on a **column of the mutated table**
(`self.owner_uuid == AuthContext.userId`) behaves correctly — unauthorized rows
are filtered and the mutation returns null rather than erroring.

**Why it matters.** Any entity whose write authority is defined in terms of a
parent row is silently **admin-only for updates**. The rule reads as though it
grants access to owners and the schema gives no hint otherwise; it surfaces only
at runtime, as a blanket `Not authorized` that is indistinguishable from an
ordinary denial. It gets misdiagnosed as bad ownership data or a JWT problem.

**Wanted.** Either push the join into the UPDATE's residual predicate (Postgres
supports `UPDATE ... FROM`, and the working `delete` path already does the
equivalent) — or, if a predicate genuinely cannot be enforced for an operation,
**fail at compile time**. A rule that can never grant access should not compile
silently. The compile-time check alone would have saved the debugging.

**Workaround today.** `vreps-exo` bypasses the rule with a computed resolver
that re-implements the ownership check in SQL and writes with raw SQL:
`schema/computed/tags/set-public-training-filter-tags.ts`, and
`reorder-tag-contents.ts` before it. That moves authorization out of the
declarative layer and duplicates the ownership rules, where drift is a privilege
bug. Both resolvers can be deleted when this is fixed.

---

## 2. A top-level `or` in a `where` object silently drops sibling column filters

**Status:** open — worked around repo-wide in jrnba_app
**Found:** 2026-08-24, verified empirically against a local exo

If a `where` object contains a top-level `or` key, **all sibling column filters
in that object are silently ignored**. The filter collapses to just the
disjunction — not AND, not OR-with-siblings. Siblings are dropped.

```graphql
where: { owner_uuid: { eq: $me }, or: [...] }   # matches rows for ANY owner
```

**Why it matters.** It fails open and silently, on filters that look obviously
correct. In our case it produced a phantom "already has an active subscription"
warning, and a webhook supersede query that would have canceled **unrelated
customers' subscriptions** had it shipped. There is no error and no warning —
the query just returns more than it should.

**Wanted.** Reject the combination at compile time, or AND the siblings with the
disjunction. Either is fine; silently discarding a filter the user wrote is not.

**Workaround today.** Always compose explicitly:
`where: { and: [{ col: {...} }, { or: [...] }] }`. Fixed across 9 call sites in
jrnba_app#248.

---

## 3. `QueryExtractor` treats a null `@query` context field as an extraction error

**Status:** open — worked around in `vreps-exo` with a sentinel
**Found:** 2026-08-13 (vreps-exo#266)

A context field sourced from `@query(...)` that resolves to null is treated as a
context-extraction failure, and every extraction failure renders as
`Not authorized`. The result: any principal whose `userEmail` resolved to null —
service JWTs, and real users with a NULL `users.email` — got a blanket
`Not authorized` on **every** `requiresAuth` computed op, while ordinary CRUD
kept working.

Chain: computed op `@inject auth: AuthContext` → `userEmail` is
`@query("resolveAuthEmail")` → null result errors in
`crates/common/src/context/provider/query.rs` → rendered as `Not authorized` in
`context/error.rs`.

**Why it matters.** The failure is attributed to authorization when the cause is
an optional field being absent, so debugging starts at roles and claims and
never reaches the context pipeline. `resolveAuthRole` returning the correct role
does not clear it.

**Wanted.** `QueryExtractor` should return `Ok(None)` for a null result on an
**optional** context field rather than erroring. Separately, context-extraction
failures would ideally not be indistinguishable from access denials.

**Workaround today.** `resolveAuthEmail` returns an empty-string sentinel
instead of null, and the `UserInvite` invitee-email predicate excludes `""`.

---

## 4. `ExographError` is the only channel for client-visible resolver messages

**Status:** open — low priority, noted for completeness
**Found:** 2026-08-25, alongside item 1

Errors thrown from a Deno resolver reach the client only when they are
`ExographError`; everything else is masked as `Internal server error`. That is a
reasonable default, but the class is easy to lose — any wrapper that rebuilds an
error (to add context, say) downgrades a deliberate user-facing message into an
opaque one, with nothing at compile time or in logs to indicate it happened.

We hit exactly that in our own helper: a debug aid rebuilt matching errors as a
plain `Error` and so silently swallowed every auth message our resolvers raised
(fixed in vreps-exo#281).

**Wanted.** Some way to mark a message as client-visible that survives being
wrapped — an error `cause` chain that is walked when deciding visibility would
be enough.

---

## 5. Playground paper cuts (partially fixed on fork, 2026-08-25)

A pass over `playground/lib` found and fixed three root causes. Playground
assets are **embedded in the `exo` binary at compile time**
(`crates/playground-router/src/playground.rs` includes `playground/app/dist`),
so seeing changes requires `playground/lib` build → `playground/app` build →
`cargo build`.

**Fixed:**

- **Cmd+K doc search never worked.** The fork's
  `KEY_MAP.searchInDocs.key = "Ctrl-K"` line only changed the label in the
  shortcuts dialog; the real listener in `@graphiql/plugin-doc-explorer` is
  hardcoded to Cmd/Ctrl+**Alt**+K. Fixed with our own capture-phase Cmd+K /
  Ctrl+K listener in `GraphiQLPlayground.tsx`.
- **Saved-profile headers didn't apply until toggled.** Three stacked causes in
  `HeaderProfileSelector.tsx`: (1) GraphiQL renders one editor-tool section
  whose `aria-label` flips between "Variables" and "Headers"; the selector
  portal polled for `[aria-label="Headers"]`, so with the default Variables tab
  the whole profile machinery never mounted until the Headers tab was opened.
  (2) Generated JWTs are minted with a 10-minute expiry and were never
  refreshed. (3) A non-persisted `signedIn` flag blocked generated profiles
  after every reload. Fixed: apply-logic now always mounts (UI portals in/out
  via MutationObserver), tokens re-mint every 4 minutes and on window focus,
  sign-in gate removed.

**Still open / notes:**

- "Limited documentation in the UI": doc comments in `.exo` files DO flow
  through introspection (`doc_comments` is threaded through the postgres
  builders and introspection resolvers), and the doc explorer plugin is
  enabled. Most of the perceived gap was the unreachable doc search (fixed
  above) plus few doc comments written in our own schema. If docs still feel
  thin, next step is auditing which generated constructs (filter inputs,
  ordering args, etc.) lack descriptions server-side.
- Upstream GraphiQL's built-in search matches type/field names only (no
  descriptions); deeper search would be an upstream-shaped change.

---

## 6. Playground GitHub-OAuth gate (PR #61) — MCP endpoint not yet covered

**Status:** feature on fork (PR #61, 2026-08-25); one known gap

The playground (and, while the gate is configured, introspection queries) can
be put behind GitHub OAuth via `EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID` /
`_CLIENT_SECRET` / `_ORG` / `_USERS` / `_SESSION_SECRET`. Rationale and setup
in the PR description.

**Gap:** the MCP endpoint (`EXO_ENABLE_MCP`, **on by default**, and live on our
Fly deployments since nothing sets it) is not covered by this gate, or by any
other. Verified 2026-08-26:

- `McpRouter::route` performs **no authentication of any kind** — `suitable()`
  matches on path and HTTP method only. There is no shared-secret / API-key
  setting for MCP anywhere in the codebase.
- A single unauthenticated `tools/list` returns the **entire GraphQL schema**.
  `EXO_MCP_MODE` defaults to `combined`, which embeds the full introspection
  SDL in the `execute_query` tool's *description*, and `handle_tools_list`
  renders every tool's description. No tool call is required.
- **`EXO_INTROSPECTION=false` does not stop this.** `create_mcp_router`
  (`crates/system-router/src/system_router.rs`) constructs its own
  `IntrospectionResolver` unconditionally and never consults
  `introspection_mode`. The same separate-instance problem means PR #61's
  session guard does not reach it either.
- MCP also runs with `TrustedDocumentEnforcement::DoNotEnforce`, so if we ever
  lock `/graphql` to persisted queries, `/mcp` stays an arbitrary-query surface.

**Not** a data-exposure hole: `execute_query` resolves through the normal
GraphQL pipeline with the request's context, so `@access` rules apply and an
anonymous MCP caller gets exactly anonymous-role data — the same as an
anonymous POST to `/graphql`. The exposure is the schema plus an
unauthenticated arbitrary-query surface.

**What exists for auth today:** JWTs flow through identically to GraphQL (so
`@access` works), and `EXO_WWW_AUTHENTICATE_HEADER` adds a `WWW-Authenticate`
header on 401 as an OAuth discovery hint for MCP clients. Neither gates the
endpoint.

**Action:** `EXO_ENABLE_MCP=false` on prod/staging — verified to remove the
router from the chain entirely (`system_router.rs:328`), not merely hide the
playground's MCP tab. If we later want MCP remotely, the fix is to extend the
PR #61 session/JWT gate over `McpRouter::route` and to have `create_mcp_router`
respect `introspection_mode`.

**Docs links, for reference:** internal-doc links in the playground need no
feature work. `//!` at the top of an `.exo` file becomes `__schema.description`
and renders as markdown (links included) on the doc explorer's landing page;
`///` doc comments on types/fields do the same per element. As of PR #60 those
links open in a new tab.
