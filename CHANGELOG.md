# Changelog

We follow [Semantic Versioning](https://semver.org/) and [Keep a Changelog](https://keepachangelog.com/). CPQ Extractor is a single CLI surface.

<details>
<summary>To see more about versioning, expand this.</summary>

Every version string starts with `v` (required), for example `v0.1.0`.

The installable surface is **CLI** (`cpq-extract`).

| Part | What you install | Example |
| --- | --- | --- |
| **CLI** | `cpq-extract` in the terminal | `v0.1.0` |

With one surface there is no `m` in the tag and no per-surface sections — just the version notes.

Each release heading is the version and date (`## v0.1.0 · 26/10/2026`); under it, a short summary ends by naming the surface, date, and tag again.

Channel suffixes (`-alpha`, `-beta`, `-stable`) are used only when a release is explicitly marked with a channel. We only call a release stable when we mean it.

</details>

## v0.1.0 · 26/10/2026

First release of the Rust CopyQ backup extractor. This version was made for CLI on 26/10/2026 (v0.1.0).

- Decode CopyQ backup versions 1 through 5, including password-protected v5 archives.
- Export each item's MIME formats into numbered files, grouped by tab, with a JSON manifest.
- Preserve settings and commands as JSON when they are included in the archive.
- Prompt for encrypted-backup passwords without displaying them, with an optional password-file argument for scripted use.
- Keep file references as data; the extractor never opens paths embedded in a backup.
