# Contributing

## Commit messages

Commits follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).
This is not a matter of taste here: `.github/cliff.toml` builds the release notes from
these prefixes, and it is configured to drop anything it cannot parse. A commit written
any other way does not fail CI — it just never appears in a release.

```
<type>(<scope>): <imperative summary under 50 chars>

Body only when the diff cannot answer "why": the cause of the bug, the
alternative that was rejected, the constraint that forced this design.
Wrap at 72 chars.
```

| Type | For |
| --- | --- |
| `feat` | Something the addon could not do before |
| `fix` | A defect, in the extension or in the demo |
| `perf` | Same behaviour, less time or memory |
| `refactor` | Same behaviour, different shape |
| `docs` | README, this file, comments |
| `test` | The self check and anything else that verifies |
| `ci` | Workflows, build scripts, release packaging |
| `style` | Formatting only |
| `chore` | Anything left over. `chore(release):` is reserved for the workflow |

The scope is optional but earns its place whenever a change is confined to one platform
or one part of the addon, because that is what a reader scanning the release notes wants
to know first: `android`, `windows`, `macos`, `linux`, `vulkan`, `d3d12`, `metal`,
`webview`, `ime`, `demo`.

```
fix(android): restore Godot's GL context before tearing down
feat(webview): add scroll_to()
ci: ship the addon's helper scripts in the release archive
```

Only the summary line reaches the release notes, so it has to stand on its own. The body
is for the next person reading `git log`, and most commits do not need one — add it when
the diff leaves a question the code cannot answer. Do not restate the diff.

One logical change per commit. If the summary needs "and", it is two commits.

## Before you push

```sh
cargo fmt --check
cargo clippy --all-targets
python -m gdtoolkit.linter $(git ls-files '*.gd')   # pip install "gdtoolkit==4.*"
scripts/build.ps1 -Test                             # or ./scripts/build.sh, then -Test
```

CI runs the same four. The self check drives the extension end to end and is the one that
catches a broken input or signal path; run it on any change to `src/` or `demo/`.

## Releases

Run the Release workflow with a version like `v1.2.3`. It sets the crate version, builds
every platform, commits the bump back to `main` as `chore(release): v1.2.3`, and publishes
the archives with notes generated from the commits since the previous tag.
