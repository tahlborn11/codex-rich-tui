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
- The macOS Rich TUI uses an official OpenAI-signed backend. Direct commands also use that
  backend, so MCP credential updates share the desktop app's Keychain signing identity.

The fork's `rich-markdown` branch carries these changes on top of upstream Codex.
The `main` branch tracks `upstream/main`.

## Outputs viewer

Press Right from an empty, focused composer, or run `/outputs`, to open the Outputs
viewer for the displayed chat. The Right shortcut respects composer key remaps,
attachments, popups, paste, and Vim navigation. `global.open_outputs` can also bind
an explicit shortcut. The footer shows `→ for outputs` between the Agents and
shortcuts hints when Right is available. The `outputs` keymap section controls
list navigation.

The top tabs show Changes, Agents, and Files, with Changes selected when the viewer
opens. Click a tab or use Alt+Left and Alt+Right to select a screen. Left and
Right also switch screens in the Agents list. Use Tab to focus the preview.
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
- `c` copies the complete source document. `s` switches Files between
  rendered text and source. Code blocks use the document copy action.
- `o` opens a file path in Files, within the selected file root. In Changes,
  `o` associates a local checkout path with the chat.
- `r` refreshes the data. Agent previews do not attach to or resume the agent.
- In Changes, `[` and `]` select a repository, `t` opens the branch picker, `b`
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

Chats and branches have a many-to-many relationship. Outputs records local
branches observed at command execution directories and file-edit locations.
This uses structured tool locations, so scripts and shell aliases do not need
special command parsers. A checkout can have several branches associated with
one chat, and several chats can use the same branch. Linked worktrees retain
separate checkout identities. Read-only commands also count as checkout usage.

Select a repository tab, then press `t` or click the branch selector to browse
its branches. Associated branches appear first; other local branches can be
selected and associated explicitly. Press `o` to associate a checkout by path,
and Delete in the branch picker to detach a branch from this chat. These actions
do not check out or delete branches. The Show all filter reveals clean associated
repositories. Target and base choices persist per chat, checkout, and branch.

Opening an existing chat attempts a bounded, one-time recovery from structured
command and edit history. Recovered associations are labelled as inferred: they
identify the branch currently observed at a recorded location, rather than prove
which branch was checked out when an old command ran. The attempt is saved even
when a location is unavailable, so refreshing or detaching does not repeatedly
reinterpret old history. Explicit association is available when recovery cannot
find a checkout. Recognized successful branch-creation commands remain a
supplementary source of exact branch refs.

Live observations are bounded and coalesced per chat and checkout location.
They inspect Git state when processed; concurrent checkout changes can prevent
an intermediate branch from being observed. Explicit selection provides a
fallback. Remote and unverified execution contexts cannot establish local
associations.

Merged branches remain in chat history. Deleted refs become archived entries,
with the last observed commit and base retained for historical branch diffs
while those Git objects remain available. Working and staged views still require
a live checkout of the selected branch. Explicit detach saves a tombstone;
observations older than the detach cannot restore the link, while later work
can associate the branch again. Temporarily unavailable checkouts retain their
history. An unavailable remembered checkout requires an explicit selection.

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
brew install git just python zstd
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
The packaging helper resolves the required V8 artifacts and builds both companion executables.
Use that helper instead of a standalone `cargo build` for a complete installation.

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

The installer signs the Rich package with the same local identity on each rebuild.
The official backend retains OpenAI's signature and handles MCP credential access.
The local certificate does not authorize Rich to update shared Keychain credentials.

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

1. Builds the main `codex` executable, `codex-code-mode-host`, and `codex-exec-server`.
2. Creates a canonical package containing Codex's managed runtime resources.
3. Copies the complete package to `~/.local/share/codex-rich-tui`.
4. Signs all three executables and replaces them atomically.
5. Creates `~/.local/bin/codex-rich` without replacing the normal `codex` command.

The macOS frontend also requires a compatible official Codex backend.
It discovers the selected official daemon package, the desktop app's bundled backend,
or the official CLI at `~/.local/bin/codex`.
The backend must match the fork's upstream major and minor version, currently `0.167`.
Rich verifies OpenAI's code signature before it runs the backend.
Set `CODEX_RICH_OFFICIAL_CLI` to select a different compatible official executable.
An invalid explicit selection stops the command.

Finish active daemon work before the first backend cutover. Then select the official package:

```bash
codex-rich app-server daemon update --from-cli
codex-rich app-server daemon version
```

Rich delegates this update command to the official executable.
It copies and pins the official package, then restarts a running daemon.
For a new home without a selected daemon package, run `codex-rich app-server daemon start` first.
Reopen existing Rich TUI windows after the cutover.

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
test -x ~/.local/share/codex-rich-tui/bin/codex-exec-server
codesign -d -r- ~/.local/share/codex-rich-tui/bin/codex 2>&1
```

Successful fork builds use a calendar release version plus Git provenance for the displayed Rich identity.
A clean build reports an identity such as:

```text
codex-rich 2026.08.10+74432be3 (upstream d1093932)
```

The Cargo workspace uses `0.167.0+rich`, based on the verified upstream `rust-v0.167.0` release tag.
Backend model discovery uses the numeric version `0.167.0`. The HTTP user agent uses the full Cargo version.
Keep this compatibility version current when the fork adopts a newer upstream release baseline.
The package helper rejects the upstream `0.0.0` development placeholder because it restricts the remote model catalog.

The package records the full fork and upstream commit IDs in `codex-package.json`. Development
builds from a modified checkout include `.dirty` after the fork commit abbreviation.
The metadata also records the numeric discovery version as `richUpstreamClientVersion`.
Tag successful installed releases as `codex-rich-YYYY.MM.DD[.N]`.
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

Inspect configured servers and authenticate an OAuth server through either CLI:

```bash
codex-rich mcp list
codex mcp login <server-name>
codex-rich mcp list
```

Use `codex-rich mcp login <server-name>` to log in directly with Rich.

The official backend shares the standard `Codex MCP Credentials` Keychain service
with the desktop app and upstream CLI. File and encrypted-secrets stores use the same
`CODEX_HOME`, normally `~/.codex`. All sharing clients must use that same home and keep
`mcp_oauth_refresh_coordination` enabled. The upstream refresh lock covers the credential
reread, provider request, and persistence, so concurrent clients adopt the refreshed token.

The fork reuses existing desktop and CLI logins when the server name and URL match.
If a server has no shared login, authenticate it once through either current client.
All clients must support stable MCP OAuth refresh coordination. The desktop backend must support it too.
Avoid clients with refresh coordination disabled. macOS can request Keychain access when the
official backend first reads an entry that does not yet authorize it.

After login, start a new Codex Rich session so the authenticated MCP tools are loaded. In the TUI,
use `/mcp` to inspect the active servers. These commands follow the official OpenAI
[MCP configuration workflow](https://developers.openai.com/codex/mcp/).

On macOS, `codex-rich mcp`, `exec`, and other backend commands run in the official executable.
Interactive chat, resume, fork, and Agents keep the Rich TUI and connect to the official daemon.
Rich stops if the selected local daemon has an incompatible version or an untrusted signature.
It never falls back to its embedded backend. `--no-daemon` is unavailable in this mode.

Configure settings that require an embedded launch, such as `--profile` or arbitrary `-c` overrides,
on the official server. Rich reports these unsupported launch options.
An explicit `--remote` endpoint retains its normal server selection.

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

Run these commands from the repository root.
Install the tools used by the formatting and test commands:

```bash
brew install uv
cargo install --locked dotslash
cargo install --locked cargo-nextest
rustup component add rustfmt clippy
```

Run the current source tree without replacing the stable installation:

```bash
just codex
```

The development command builds a small package before launching it, including the companion Code
Mode host. It reuses the normal Codex configuration and authentication.
On macOS, it uses the same official-backend policy as the installed Rich command.

Build a release-profile package without installing it:

```bash
just package-rich-tui
./codex-rs/target/codex-rich-tui/bin/codex
```

For TUI changes, format and run the focused tests before committing:

```bash
just test -p codex-tui
just fix -p codex-tui
just fmt
```

Follow the applicable crate instructions for the complete validation requirements.
On macOS, start broad core or workspace suites with `--test-threads=4`.
Run a failing test alone with `--test-threads=1` before diagnosis.
Before Code Mode tests, build the companion host through the packaging helper above.

For OAuth changes, test the installed Rich package against a current official CLI:

```bash
CODEX_OFFICIAL_TEST_BIN="$(command -v codex)" \
CODEX_RICH_TEST_BIN="$(command -v codex-rich)" \
python3 scripts/check_codex_rich_shared_oauth.py -v
```

The check uses temporary homes, synthetic tokens, and a loopback provider.
Set `CODEX_SHARED_OAUTH_KEYCHAIN_TEST=1` to include the macOS Keychain check.
That check creates a synthetic Keychain entry and verifies its removal afterward.
It requires a signed Rich package. Forced termination can leave the synthetic entry behind.

### Automatic compressed build cache

The Rich package builder manages a local compressed Cargo cache for `just codex`,
`just package-rich-tui`, and `just install-rich-tui-local`.
Direct `cargo` commands use their normal expanded cache.

Before a package build, the helper keeps an existing warm cache. If the cache is
cold, it restores a compatible local `.tar.zst` archive. Cargo then checks
fingerprints and timestamps and rebuilds changed inputs during the normal build.

After a successful package build, the helper creates and verifies a new archive.
Only then does it remove the expanded Cargo cache directories. The helper preserves
package outputs, requested release archives, top-level executables, installed
files, source files, and Cargo lock files. A failed build does not save or evict
its cache. A failed snapshot leaves the expanded cache available.

Archives live under `~/.cache/codex-rich-tui/build-cache/`, in a directory scoped
to this checkout. Exact keys identify the dependency and build configuration.
Compatible fallback keys allow Cargo to reuse dependencies after a lockfile change.
Compiler, native architecture, profile definitions, and compiler-flag changes
prevent an incompatible restore. Each snapshot records all included target and
profile directories, so switching the requested build profile can reuse them.
The archives contain Cargo build artifacts. Downloaded Cargo sources and V8
resources keep their existing locations.

The default archive budget is 50 GiB, with a 100 GiB free-space reserve and
14-day retention. These limits apply to compressed archives. A restore
requires room for the expanded cache. If storage exceeds the budget, the helper
keeps the current verified archive and reports the remaining storage pressure.

The helper uses `tar` and `zstd`. On macOS, install Zstandard with `brew install zstd`.
Missing tools, active builds, unsafe paths, or insufficient space cause cache
maintenance to skip safely. The package build can still use a warm cache or build
from source. External Cargo build directories remain outside automatic cache management.

Use these environment variables to change retention:

```bash
CODEX_RICH_CACHE_MAX_GIB=50 \
CODEX_RICH_CACHE_KEEP_DAYS=14 \
CODEX_RICH_MIN_FREE_GIB=100 \
just package-rich-tui
```

Set `CODEX_RICH_CACHE=0` to disable automatic restore, snapshot, and eviction.
Set `CODEX_RICH_CLEANUP=0` to keep the expanded cache after a successful snapshot.
This option is useful during repeated development builds because it avoids an
extract cycle each time.

## Keep the fork current

Start with a clean checkout and no Git operation in progress.
Stop if any command fails. Inspect divergent commits before another attempt.
Preserve substantive fork work on `rich-markdown`.

```bash
git status
git fetch origin --tags
git fetch upstream --tags
rich_lease=$(git rev-parse origin/rich-markdown)
git switch rich-markdown
git merge --ff-only origin/rich-markdown
git switch main
git merge --ff-only upstream/main
git switch rich-markdown
git rebase upstream/main
```

Review renderer conflicts and snapshot changes. If a conflict is ambiguous, abort the rebase.
Run the required formatting, lint, crate tests, and snapshot checks.
Commit the validated result before the final package build so the package records a clean fork commit.

Choose the release version from the build date in `America/New_York`.
Use `YYYY.MM.DD`, or the next unused `YYYY.MM.DD.N` if that date already has a release.
Check local and origin tags before the build. Never replace an existing release tag.
Set the selected version explicitly:

```bash
rich_version=YYYY.MM.DD
rich_release_commit=$(git rev-parse HEAD)
CODEX_RICH_RELEASE_VERSION="$rich_version" just install-rich-tui-local
codex-rich --version
```

Verify the version, fork commit, upstream commit, and absence of a `.dirty` marker.
Check the package metadata, launcher, companion executables, help output, and signatures.
The package build applies the cache policy described above.

After all checks pass, push both branches. Use the recorded lease for a rebased branch:

```bash
git push origin main
git push --force-with-lease="refs/heads/rich-markdown:$rich_lease" origin rich-markdown
```

Stop if the lease fails. Inspect the remote changes before another push.
Use a normal push for an update that does not rewrite branch history.
After the branch push succeeds, tag the exact installed release commit:

```bash
git fetch origin --tags
test -z "$(git tag --list "codex-rich-$rich_version")"
git tag -a "codex-rich-$rich_version" "$rich_release_commit" -m "Codex Rich TUI $rich_version"
git push origin "refs/tags/codex-rich-$rich_version"
```

### Update running clients and the managed daemon

Reopen Rich TUI clients after installation to load the new executable.
Keep the daemon on an official OpenAI-signed package with a compatible protocol version.
A Rich TUI rebuild does not require a daemon update.
If the upstream baseline changes, install a compatible official backend before the cutover.
Finish active work first. Then copy and pin that official package:

```bash
codex-rich app-server daemon version
codex-rich app-server daemon update --from-cli
codex-rich app-server daemon version
```

The update restarts a running daemon. Reopen its clients afterward.
A restart alone retains the selected package.
An update without `--from-cli` selects the official production package.

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

Run `codex-rich app-server daemon version` and check that the managed package is official.
If an old Rich package remains selected, finish active work and run
`codex-rich app-server daemon update --from-cli`.

macOS resets a direct Keychain item's signing partition when a client updates its password data.
A self-signed Rich backend and the official desktop backend can repeatedly displace each other's authorization after token refreshes.
A stable local certificate does not prevent this reset.
The official-backend policy keeps both clients under OpenAI's signing identity.
Existing Grafana logins remain in the shared store. This change does not require another login.

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
