# Commit Message Guidelines

## Format

```
<type>: <short summary>
```

- **type** — one of the prefixes below
- **summary** — one line, imperative present tense, no period at the end

## Types

| Type | Use for |
|---|---|
| `feat` | A new feature |
| `fix` | A bug fix |
| `docs` | Documentation only changes |
| `refactor` | A code change that neither fixes a bug nor adds a feature |
| `test` | Adding missing tests or correcting existing tests |
| `chore` | Build process, tooling, or dependency changes |

## Rules

- Default to one line. No footer.
- Imperative present tense: "add" not "added", "fix" not "fixed".
- No scope prefix (the project is small enough that `feat(core):` adds noise).
- No PR/issue references (commits go straight to master).
- English only.
- **Body is allowed when the change is large or the reasoning is non-obvious.** Separate it from the summary with a blank line. Keep it concise — explain *why*, not *what*.

## Examples

```
feat: add visit deletion with confirmation panel
fix: correct the test counts in development.md
docs: document the write path in architecture.md
refactor: drop the never-varying hash argument from app.rs test entry()
test: pin host_color saturation and value exactly once
chore: rename doc/ to docs/ and update every reference
```

With body:

```
refactor: unify delete mode design

BackTab now toggles symmetrically: when all listed rows are marked, it unmarks only
those, preserving marks on collapsed days. The confirm panel walks groups instead of
rows so collapsed-day marks are visible. Scope note switches text based on whether
collapsed-day marks exist.
```
