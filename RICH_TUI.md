# Codex Rich TUI

Codex Rich TUI is a personal fork of the open-source Codex CLI. It keeps the complete upstream
workflow—streaming chat, tools, approvals, history, configuration, MCP, and Code Mode—while making
terminal responses feel closer to the rendered Markdown in the Codex desktop app.

The stable installation workflow below currently targets macOS. Developers on Linux and Windows
can still build and run the fork from source, but the Keychain-aware signed installer is
macOS-specific.

## What the fork changes

- Markdown headings render typographically without visible `#` markers.
- Blockquotes use a visual quote rail.
- Fenced code blocks use a full-width, palette-aware frame with the language on the left and a
  clickable `⎘` copy control on the right. Hovering highlights the control; clicking copies that
  block's exact code without changing transcript selection. Confirmed copies temporarily change
  the clicked control to `✓` and show `Copied!` in the transcript status area.
- `/copy-code` copies the only non-empty fenced block from the latest response immediately, or
  opens a numbered picker when the response contains multiple blocks. It remains the keyboard and
  non-mouse fallback, while `alt+y` keeps the fast path that copies the last non-empty fenced block.
- Task lists use semantic status markers, and expanded HTML `<details>` blocks render with a `▾`
  disclosure marker instead of raw tags.
- Completed background-terminal waits use the same visual rail language as completed commands.
- Successful commands remain individually visible in the conversation instead of being collapsed
  into an aggregate `Ran N commands` entry.
- Terminal titles can show live activity, project, thread, branch, model, and other configured
  values.
- Upstream MCP OAuth recovery refreshes expired or rejected access tokens and saves the new
  credentials. Packaged `codex-rich` builds add a separate Keychain service. This separation
  prevents refresh-token races with the Codex desktop app.

The fork's `rich-markdown` branch carries these changes on top of upstream Codex.
The `main` branch tracks `upstream/main`.

## Outputs viewer

Press Right from an empty, focused composer, or run `/outputs`, to open the Outputs
viewer for the displayed chat. The Right shortcut respects composer key remaps,
attachments, popups, paste, and Vim navigation. `global.open_outputs` can also bind
an explicit shortcut. The footer shows `→ for outputs` between the Agents and
shortcuts hints when Right is available. The `outputs` keymap section controls
list navigation.

The top tabs show Plans, Files, Changes, and Agents, with the current screen
highlighted. Click a tab or use Alt+Left and Alt+Right to select a screen. Left and
Right also switch screens in the Plans and Agents lists. Use Tab to focus the preview.
Enter opens a selected file or document. Wide terminals show the list and preview together.
Narrow terminals show one at a time. Escape returns from the preview, then closes
the viewer. The viewer preserves the composer draft. Single-click an item in any
Outputs list to select it; double-click it to focus its preview. Folder clicks
toggle expansion. The mouse wheel always scrolls the preview, leaving the list
selection unchanged. In split view, arrow keys still follow keyboard focus;
use Tab to switch panes. In narrow view, the wheel reveals the preview.

- Files shows an expandable directory tree. Enter or Right expands a folder;
  Left collapses it or selects its parent. Click a folder to toggle it. Directories
  load as you expand them, including empty and hidden folders. The tree includes
  all regular files; binary and large files remain subject to preview limits.
  Names and paths are relative to the file root.
- `/` filters the list or finds text in the preview. Enter applies the search.
  The Files filter searches directories that have already loaded.
- `c` copies the complete source document. `s` switches Plans and Files between
  rendered text and source. Code blocks use the document copy action.
- `o` opens a file path in Files. Paths must stay within the selected file root.
- `r` refreshes the data. Agent previews do not attach to or resume the agent.
- In Changes, `[` and `]` select a repository, `t` selects the target branch, `b`
  selects the base, and `v` cycles the change views. Repository tabs show each
  available checkout. Click a repo to select it; overflow pages have clickable
  arrows. The Changed repos filter hides checkouts confirmed clean in the current
  view. The selected checkout and repos with unknown status stay visible. Click
  the filter or press `a` to show all repos. Availability checks repeat ten seconds
  after each scan while Changes is open. Press `r` to reload the file patches.
  A second tab row shows Branch,
  Changes, Staged, and Turn patch. Changes combines unstaged edits and untracked
  files. New files have a `+` marker and green names in the file tree.
  Click a tab or press `1` through `4`
  to select it. The list groups changed files into expandable folders, with paths
  relative to the selected repository. Select a file with Up or Down, then use Enter or Tab to
  scroll its patch. Wide previews show old and new lines side by side. Source
  code uses syntax highlighting, with red backgrounds for deletions and green
  backgrounds for additions. Narrow previews use the same styling in a unified
  diff. Colors follow the terminal palette and syntax theme.
  Line numbers identify each side. Previews hide routine Git patch headers and
  keep rename, permission, and binary-file details. `c` copies the selected file's
  complete original patch, including headers.

A chat inside a repository uses that checkout. A chat in a parent project folder
shows only repositories with a recorded branch created by that chat. Its target
follows the recorded branch; `t` cannot select an unrelated branch. The Show all
filter reveals clean associated repositories, not unrelated checkouts. Explicit
target choices remain available for chats opened inside a repository.

Successful literal `git switch -c`, `git checkout -b`, and `git worktree add -b`
commands in POSIX shells record the exact local branch ref and checkout for the chat.
Quiet and tracking flags are supported. Commands
may use `git -C`, a literal `cd` with an absolute or `./`/`../` path, or an `&&` chain. Failed or ambiguous commands,
including pipelines and OR recovery, do not establish an association. A branch
creation with `switch -c` or `checkout -b`, followed by `; git status` (or a
newline and `git status`), is accepted
only when the recorded output also contains Git’s explicit new-branch confirmation. Branches
created through opaque scripts or aliases cannot be inferred automatically.
Existing chats can recover evidence from the latest bounded history pages.
Old viewer selections and the current checkout do not establish ownership.

Associations persist per chat and checkout, including recorded worktrees outside
the original project directory. Linked worktrees have separate identities.
When a recorded branch is deleted, opening or refreshing Outputs removes its
chat association. The ten-second repository checks also remove deleted branches
while Changes is open. A temporarily unavailable checkout does not erase an
association. Removed evidence is retained as a tombstone so old command history
cannot restore the link; a newer branch-creation event can establish a new link.
Repository, target, and base choices are saved per chat and checkout. An
unavailable remembered checkout requires an explicit repository selection.

Branch changes compare the target against its merge base with the selected base.
Viewing a branch does not check it out. The header shows the base and target,
plus the checkout branch when it differs from the target. Changes and Staged belong to the
checkout and require the selected branch to remain checked out. If it differs,
the viewer reports that working changes are unavailable; it does not switch branches.
Working changes are shared by chats that use that checkout. Latest turn patch shows
the chat's recorded tool edits across repositories, rather than its checkout state.

File and Git reads require a verified local execution context. Remote or unknown
contexts show an unavailable message. Discovery requires Python 3 and Git. File
previews reject binary files and paths outside the root, and read at most 512 KiB.
History and repository discovery are bounded. Each file directory lists at most
2,000 entries. The open viewer caches at most 128 directories and 20,000 entries until refresh.
Closing the viewer or switching chats releases its cache. Separate CLI windows
keep independent caches.
Refresh updates the catalog and agent status.

## Clean macOS setup

### 1. Install prerequisites

Install the Xcode command-line tools, a current Rust toolchain, `just`, Git, and Python 3. On a
machine that uses Homebrew for developer tooling, the non-Rust prerequisites can be installed with:

```bash
xcode-select --install
brew install git just python
```

Install Rust with [rustup](https://rustup.rs/) and confirm the required commands are available:

```bash
rustc --version
cargo --version
just --version
python3 --version
command -v codesign
```

The first package build downloads managed Codex runtime resources, so it requires network access.

### 2. Clone the fork and configure upstream

```bash
mkdir -p ~/code
git clone --branch rich-markdown https://github.com/tahlborn11/codex-rich-tui.git ~/code/codex-rich-tui
cd ~/code/codex-rich-tui
git remote add upstream https://github.com/openai/codex.git
git remote -v
```

`origin` should point to `tahlborn11/codex-rich-tui`, and `upstream` should point to
`openai/codex`. If `upstream` already exists, update it with:

```bash
git remote set-url upstream https://github.com/openai/codex.git
```

### 3. Create the local code-signing identity

The stable installer signs both Codex executables with the same persistent identity on every
rebuild. This gives macOS Keychain a stable designated requirement, preventing OAuth credentials
from being treated as though a different unsigned development binary is requesting access after
each build.

Open **Keychain Access**, then:

1. Choose **Keychain Access → Certificate Assistant → Create a Certificate**.
2. Set the name to `Codex Rich Local Signing`.
3. Select **Self Signed Root** as the identity type and **Code Signing** as the certificate type.
4. Enable **Let me override defaults**, use a long validity period such as 3,650 days, and keep the
   private key in the login keychain.
5. Restrict the certificate to code signing when Keychain Access presents the key-usage and trust
   options. Do not enable general TLS trust.

Verify that macOS sees the identity:

```bash
security find-identity -v -p codesigning | grep 'Codex Rich Local Signing'
```

The installer accepts a different identity when necessary:

```bash
export CODEX_RICH_SIGNING_IDENTITY="My Local Code Signing Identity"
```

### 4. Build and install

From the repository root:

```bash
just install-rich-tui-local
```

This command:

1. Builds the main `codex` executable and `codex-code-mode-host`.
2. Creates a canonical package containing Codex's managed runtime resources.
3. Copies the complete package to `~/.local/share/codex-rich-tui`.
4. Signs both executables and replaces them atomically.
5. Creates `~/.local/bin/codex-rich` without replacing the normal `codex` command.

The operation is safe to repeat after every update. To use a different launcher directory, set
`CODEX_RICH_BIN_DIR`. To change the package installation directory, invoke the installer directly
with its optional package and installation-directory arguments:

```bash
scripts/install_codex_rich_local.sh \
  codex-rs/target/codex-rich-tui-dev \
  ~/.local/share/codex-rich-tui
```

Ensure `~/.local/bin` is on `PATH`. For zsh, add this to `~/.zshrc` if it is not already present:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

Open a new shell and verify the installation:

```bash
command -v codex-rich
codex-rich --version
test -x ~/.local/share/codex-rich-tui/bin/codex-code-mode-host
codesign -d -r- ~/.local/share/codex-rich-tui/bin/codex 2>&1
```

Successful fork builds use a calendar release version plus Git provenance for the displayed Rich identity.
A clean build reports an identity such as:

```text
codex-rich 2026.08.10+74432be3 (upstream d1093932)
```

The Cargo workspace uses `0.164.0+rich`, based on the verified upstream `rust-v0.164.0` release tag.
Backend model discovery uses the numeric version `0.164.0`. The HTTP user agent uses the full Cargo version.
Keep this compatibility version current when the fork adopts a newer upstream release baseline.
The package helper rejects the upstream `0.0.0` development placeholder because it restricts the remote model catalog.

The package records the full fork and upstream commit IDs in `codex-package.json`. Development
builds from a modified checkout include `.dirty` after the fork commit abbreviation.
The metadata also records the numeric discovery version as `richUpstreamClientVersion`.
Tag successful installed releases as `codex-rich-YYYY.MM.DD`.
For another release on the same day, set `CODEX_RICH_RELEASE_VERSION=YYYY.MM.DD.N` and use the matching tag.

### 5. Sign in to Codex

The fork uses the normal Codex home directory (`~/.codex`) and therefore shares configuration,
conversation history, plugins, skills, and authentication with a standard Codex installation.

Start it and select **Sign in with ChatGPT**:

```bash
codex-rich
```

See the official OpenAI documentation for [Codex authentication](https://developers.openai.com/codex/auth/).

## Default model for new chats

Set the model and reasoning effort in `~/.codex/config.toml`:

```toml
model = "gpt-6.1-sol"
model_reasoning_effort = "medium"
```

New chats preserve these saved preferences before they apply managed model defaults.
Managed model defaults apply when neither preference has a value.
Launch flags and selected profiles keep their existing priority.
Existing chats keep their own model settings.
Restart an open CLI after installation to load this behavior.

If a server cannot report its configuration, the CLI preserves its existing managed-default behavior.
Explicit launch flags still override those defaults.

## MCP and Keychain-backed OAuth

For local macOS builds, explicitly select Keychain storage at the top level of
`~/.codex/config.toml`:

```toml
mcp_oauth_credentials_store = "keyring"
```

Configure remote MCP servers in the same file using the standard
`[mcp_servers.<server-name>]` tables. Do not commit tokens, client secrets, or private server
configuration to this repository.

Inspect configured servers and authenticate OAuth servers with the forked executable:

```bash
codex-rich mcp list
codex-rich mcp login <server-name>
```

Packaged `codex-rich` builds store direct Keychain-backed MCP credentials under
`Codex Rich MCP Credentials`, separately from the `Codex MCP Credentials` service used by other
Codex installations. When Codex's encrypted-secrets Keychain backend is selected, the fork also
uses a separate `${CODEX_HOME}/codex-rich` credential root (normally
`~/.codex/codex-rich`). The same root isolates the `auto` and `file` fallback stores. This
separation is necessary for providers that rotate refresh tokens: two independent Codex processes
must not consume the same one-time refresh token. After first installing a build with this
isolation, authenticate each OAuth server once with `codex-rich mcp login <server-name>` even if it
was already authorized in the desktop app. The desktop app's existing credentials are left
unchanged.

After login, start a new Codex Rich session so the authenticated MCP tools are loaded. In the TUI,
use `/mcp` to inspect the active servers. These commands follow the official OpenAI
[MCP configuration workflow](https://developers.openai.com/codex/mcp/).

The stable signature is important here: installing an unsigned replacement or signing each build
with a new identity can cause macOS to prompt for Keychain access again or deny the stored OAuth
credential.

## Terminal titles in iTerm2

Codex Rich emits standard terminal title sequences, but iTerm2 controls whether they are accepted
and whether its foreground **Job** name is appended.

In **iTerm2 → Settings → Profiles → General**:

1. Enable **Applications in terminal may change the title**.
2. Under **Title**, enable **Session Name**.
3. Disable **Job** if you do not want process names such as `codex-rich`, `esbuild`, or helper
   clients appended to the title.

For an existing tab, make the same change under **Edit → Edit Session**, or open a new tab after
changing the profile. Existing sessions can retain stale profile settings.

A representative Codex configuration is:

```toml
[tui]
terminal_title = ["activity", "project-name", "thread-title"]
status_line = ["model-with-reasoning", "thread-title"]
```

See the official OpenAI [configuration reference](https://developers.openai.com/codex/config-reference/)
for supported Codex settings.

## Development workflow

Run the current source tree without replacing the stable installation:

```bash
just codex
```

The development command builds a small package before launching it, including the companion Code
Mode host. It reuses the normal Codex configuration and authentication.

Build a release-profile package without installing it:

```bash
just package-rich-tui
./codex-rs/target/codex-rich-tui/bin/codex
```

For TUI changes, format and run the focused tests before committing:

```bash
cd codex-rs
just test -p codex-tui
just fix -p codex-tui
just fmt
```

Follow `AGENTS.md` for the complete validation requirements of whichever crates were changed.

### Automatic build-cache cleanup

The Rich package builder runs cache cleanup after each successful package build.
This includes `just codex`, `just package-rich-tui`, and `just install-rich-tui-local`.
You do not need a scheduled task or a separate cleanup command.
Direct `cargo` commands do not run this helper.

The default policy keeps a 50 GiB cache budget and a 100 GiB free-space reserve.
It removes inactive build profiles and old package copies after 14 days.
Under disk pressure, it removes other package copies and incremental caches before complete build profiles.

It preserves the current package, requested archives, installed executables, and source files.
It also protects other package copies for one hour after their last change.
Only packages with the builder's staging marker qualify for automatic removal.
The installer excludes this marker and identifies installation directories separately.
The installer rejects destinations inside this repository's disposable Cargo target directory.

If the remaining build cache exceeds the budget, cleanup can discard it and require a cold rebuild next time.
This fallback preserves the current profile's top-level executables.

Cleanup uses Cargo locks on macOS and Linux to avoid active builds.
If a lock is busy, cleanup skips the run.
Unsupported platforms and external build directories also skip cleanup.

Protected or unrecognized files can leave the cache above the budget.
Mounted directories also prevent cleanup.
The helper reports retained space and any unmet limits.
Cleanup errors do not fail a successful package build.

Use these environment variables to change the policy:

```bash
CODEX_RICH_CACHE_MAX_GIB=50 \
CODEX_RICH_CACHE_KEEP_DAYS=14 \
CODEX_RICH_MIN_FREE_GIB=100 \
just install-rich-tui-local
```

Set `CODEX_RICH_CLEANUP=0` to disable automatic cleanup.
To preview cleanup without changes, run:

```bash
python3 scripts/codex_rich_cleanup.py \
  --package-dir codex-rs/target/codex-rich-tui-dev \
  --cargo-profile dev-small --dry-run
```

## Keep the fork current

The safe manual update flow is:

```bash
git fetch origin --tags
git fetch upstream --tags
git switch main
git merge --ff-only upstream/main
git switch rich-markdown
git rebase upstream/main
```

Review renderer conflicts and snapshot changes. If a conflict is ambiguous, abort the rebase.
Run the tests required by `AGENTS.md`. Commit the validated result before the final package build
so the package records a clean fork commit. Then rebuild the installed package:

```bash
just install-rich-tui-local
codex-rich --version
```

The package build automatically applies the cache policy described above.
Review its cleanup report if disk pressure requires a cold rebuild.

After validation and installation checks, push both branches:

```bash
git push origin main
git push --force-with-lease origin rich-markdown
```

Stop if the lease fails. Inspect the remote changes before another push.

Scheduled Codex tasks can maintain the fork, but their state lives outside this repository.
Review UI conflicts, OAuth behavior, and upstream protocol changes before publication.

## Troubleshooting

### `code-signing identity not found`

Create the identity from step 3, confirm it appears in `security find-identity`, or set
`CODEX_RICH_SIGNING_IDENTITY` to the exact name of an existing code-signing identity.

### `codex-rich: command not found`

Confirm `~/.local/bin` is on `PATH` and that the launcher exists:

```bash
ls -l ~/.local/bin/codex-rich
```

### Code Mode host warning

Reinstall with `just install-rich-tui-local`. Do not copy only the main executable: Code Mode needs
the companion host and the canonical package layout.

### Repeated Keychain password prompts

Confirm that you consistently use `just install-rich-tui-local`, that the signing identity has not
changed, and that `mcp_oauth_credentials_store = "keyring"` is set. Reauthenticate the affected
server with `codex-rich mcp login <server-name>` after correcting the installation.

### MCP reports `OAuth metadata discovery failed`

First confirm that the installed executable is current and signed, then rerun
`codex-rich mcp login <server-name>`. The fork contains compatibility and stale-token recovery fixes
for remote MCP OAuth flows.

### iTerm2 shows `Default (codex-rich)` or a helper process name

Enable application-controlled titles and remove the **Job** title component using the iTerm2 steps
above. This is iTerm2 composing its own profile/job title, not a Markdown-rendering problem.

## Security notes

- Never commit `~/.codex`, OAuth credentials, API keys, or private MCP configuration.
- The local signing identity is for this machine's development installation. Do not export or share
  its private key.
- Restrict trust for the self-signed certificate to code signing; do not make it a general TLS root.
- Review upstream merges and installation-script changes before running them.
