# Claude Code — Project Instructions

## Never interact with the public upstream repo (`exograph/exograph`)

**This repository is `Virtual-Repetitions/exograph`. Treat the public upstream
`exograph/exograph` as read-only reference material.**

Do not, under any circumstances and regardless of how the request is phrased:

- open, comment on, close, or edit issues or pull requests upstream
- push branches or open PRs against upstream
- file bug reports upstream, even well-formed ones with no secrets

Reading upstream code, history, and issues for reference is fine. **Writing
anything is not**, and "it's just a bug report" is not an exception. If a change
seems worth sending upstream, say so and stop — that is the user's call to make
and the user's account to make it from.

### The trap that caused this rule

GitHub Issues are **disabled on this fork**. `gh issue create` with no explicit
`-R` therefore falls back to the parent repository and posts publicly upstream,
reporting success, with nothing to indicate the destination changed. That is
exactly how `exograph/exograph#2278` was filed by mistake on 2026-08-25 (since
closed).

Mitigations in place:

- `gh repo set-default Virtual-Repetitions/exograph` is set in this clone, so
  `gh` resolves here and fails loudly ("repository has disabled issues") instead
  of silently escalating to a public repo. Re-run it in any fresh clone.
- Always pass `-R Virtual-Repetitions/exograph` explicitly on `gh` commands
  anyway. Do not rely on the default.
- The `upstream` remote's **push URL is set to the sentinel
  `DISABLED_DO_NOT_PUSH_TO_UPSTREAM`** (fetch URL is untouched), so
  `git push upstream ...` fails on a nonexistent URL instead of reaching the
  public repo. Re-run
  `git remote set-url --push upstream DISABLED_DO_NOT_PUSH_TO_UPSTREAM` in any
  fresh clone, and never "fix" this URL.

### Where fork work goes instead

- **Backlog / bug tracking:** [BACKLOG.md](BACKLOG.md) in this repo. Issues are
  disabled here, so the file is the tracker.
- **Code changes:** branches and PRs on `Virtual-Repetitions/exograph`.

## Related repos

- `../vreps-exo` — the Exograph schema/DSL and computed resolvers we build on
  this fork. See its `AGENTS.md`.
- `../jrnba_app` — the Next.js app consuming it. See its `CLAUDE.md`.
