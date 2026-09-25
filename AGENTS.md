Always use best practices. Focus on keeping the code simple and easy to understand. Don't over-engineer. Write some test for high impact stuff.

Instead of doing format check then format, just run format.

Don't rebuild docs unless specifically working on docs site. You can update docs without rebuilding.

Use Conventional Commits for commit titles so Release Please can parse them.
Use `fix:` for fixes and `feat:` for features (for example, `fix(ci): repair release checks`).
For non-release changes, use types such as `docs:` or `chore:`.
