# Codex Rich TUI

This fork keeps the complete Codex CLI workflow and evolves the transcript toward the visual
language of the Codex desktop app. The customization lives in the existing Rust TUI rather than in
a terminal-output wrapper, so streamed Markdown remains semantic and chat, tools, approvals,
history, and configuration continue to use the upstream implementation.

## Current changes

- Markdown headings render typographically without visible `#` markers.
- Blockquotes use a visual quote rail while keeping body text in the primary foreground color.
- `/copy-code` copies the last non-empty fenced block from the latest response.
- Fenced code blocks use a full-width, palette-aware tinted frame with the language on the left and
  the `alt+y` copy shortcut on the right.
- Task lists use semantic status markers, and expanded HTML `<details>` blocks render with a `▾`
  disclosure marker instead of raw tags.
- Terminal titles are reasserted after shell and MCP activity so external processes cannot leave
  stale tab titles behind.
- The work is isolated on the `rich-markdown` branch for straightforward upstream rebases.

## Planned changes

- Optional mouse interaction for code-block copy controls after transcript ownership moves out of
  native terminal scrollback; enabling terminal mouse capture before that would break normal
  selection and scrolling without making historical rows reliably clickable.
- Richer blockquotes and response spacing where terminal width allows it.
- Snapshot coverage for each visual state and narrow terminal widths.

## Run locally

From `codex-rs`:

```bash
just codex
```

This builds a small development package before launching, including the companion
`codex-code-mode-host`, so Code Mode is available during development. The normal Codex
configuration and authentication are reused.

## Build a stable executable

From the repository root, build the canonical release package:

```bash
just package-rich-tui
```

Run the packaged fork directly:

```bash
./codex-rs/target/codex-rich-tui/bin/codex
```

The package keeps `codex` and `codex-code-mode-host` together and includes Codex's managed runtime
resources. To install a copy that survives `cargo clean` and make it available as `codex-rich`
without replacing an existing Codex install:

```bash
mkdir -p ~/.local/bin
mkdir -p ~/.local/share/codex-rich-tui
cp -R codex-rs/target/codex-rich-tui/. ~/.local/share/codex-rich-tui/
ln -sfn ~/.local/share/codex-rich-tui/bin/codex ~/.local/bin/codex-rich
```

Rebuild and repeat the copy when you want to update the installed version. The executable resolves
the package through the symlink, so it can still find its companion host and resources.

## Keep the fork current

The personal GitHub fork is configured as `origin`, and the official OpenAI repository is
configured as `upstream`. Fetch and rebase this branch onto upstream main:

```bash
git fetch upstream
git rebase upstream/main
```

Run the TUI tests and rebuild the stable package after resolving any conflicts. Keep upstream
updates supervised: renderer conflicts and snapshot changes should be reviewed rather than merged
automatically.
