---
name: semantic-commits
description: Enforces Conventional Commits format for all git commits in this repository. Use whenever staging changes, committing, amending commits, or writing commit messages. Ensures consistency with feat, fix, docs, style, refactor, perf, test, chore, ci, and build types.
metadata:
  author: my-sanctuary
  version: '1.0.1'
---

# Semantic Commit Messages

All commits in this repository follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.

## Format

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

## Types

| Type       | Use when...                                             |
| ---------- | ------------------------------------------------------- |
| `feat`     | Adding a new feature or capability                      |
| `fix`      | Fixing a bug                                            |
| `docs`     | Updating documentation only                             |
| `style`    | Code style changes (formatting, semicolons, etc.)       |
| `refactor` | Code change that neither fixes a bug nor adds a feature |
| `perf`     | Performance improvement                                 |
| `test`     | Adding or correcting tests                              |
| `chore`    | Build process, dependencies, tooling changes            |
| `ci`       | CI/CD configuration changes                             |
| `build`    | Changes affecting the build system or external deps     |

## Scopes

Common scopes for this monorepo:

- `web` — changes to `apps/web`
- `api` — changes to `apps/api`
- `repo` — workspace-level configuration (nx, git, ci, etc.)
- `deps` — dependency updates

## Examples

```
feat(api): add health check endpoint

fix(web): correct router base path in static build

docs(repo): update README with local dev instructions

chore(deps): bump @tanstack/react-router to 1.115

ci(repo): add GitHub Actions workflow for PR checks
```

## Rules

1. Use lowercase for type, scope, and description.
2. Do not end the description with a period.
3. Keep the description concise (≤ 72 characters).
4. Use the body to explain _what_ and _why_, not _how_.
5. Reference issues in the footer when applicable: `Closes #123`.
