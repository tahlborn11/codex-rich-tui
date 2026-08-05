# Headers, primary, and secondary text

- **Headers:** Use typography rather than literal Markdown syntax. Heading levels should be
  distinguished with bold, underline, and italic styling; do not render the leading `#` signs.
- **Primary text:** Default.
- **Secondary text:** Use `dim`.
- **Blockquotes:** Use a green vertical rail with primary-color body text.
- **Fenced code blocks:** Use a full-width terminal-native frame with a subtle palette-aware
  background, show the language when known, and right-align copy affordances outside the semantic
  code content.
- **Task lists:** Use green `✓` and dim `○` status markers instead of raw checkbox syntax.

# Foreground colors

- **Default:** Most of the time, just use the default foreground color. `reset` can help get it back.
- **User input tips, selection, and status indicators:** Use ANSI `cyan`.
- **Success and additions:** Use ANSI `green`.
- **Errors, failures and deletions:** Use ANSI `red`.
- **Codex:** Use ANSI `magenta`.

# Avoid

- Avoid custom colors because there's no guarantee that they'll contrast well or look good in various terminal color themes. (`shimmer.rs` is an exception that works well because we take the default colors and just adjust their levels.)
- Avoid ANSI `black` & `white` as foreground colors because the default terminal theme color will do a better job. (Use `reset` if you need to in order to get those.) The exception is if you need contrast rendering over a manually colored background.
- Avoid ANSI `blue` and `yellow` because for now the style guide doesn't use them. Prefer a foreground color mentioned above.

(There are some rules to try to catch this in `clippy.toml`.)
