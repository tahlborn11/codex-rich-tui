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
- Fenced code blocks use a full-width, palette-aware frame with the language on the left and the
  `alt+y` copy shortcut on the right.
- `/copy-code` copies the last non-empty fenced block from the latest response.
- Task lists use semantic status markers, and expanded HTML `<details>` blocks render with a `▾`
  disclosure marker instead of raw tags.
- Completed background-terminal waits use the same visual rail language as completed commands.
- Terminal titles can show live activity, project, thread, branch, model, and other configured
  values.
- MCP OAuth recovery handles expired or rejected access tokens through a serialized refresh and
  preserves refreshed credentials in the configured store.

The fork's `main` branch carries these changes on top of upstream Codex.

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
git clone https://github.com/tahlborn11/codex-rich-tui.git ~/code/codex-rich-tui
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

### 5. Sign in to Codex

The fork uses the normal Codex home directory (`~/.codex`) and therefore shares configuration,
conversation history, plugins, skills, and authentication with a standard Codex installation.

Start it and select **Sign in with ChatGPT**:

```bash
codex-rich
```

See the official OpenAI documentation for [Codex authentication](https://developers.openai.com/codex/auth/).

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

## Keep the fork current

The safe manual update flow is:

```bash
git switch main
git pull --ff-only origin main
git fetch upstream
git merge upstream/main
```

Review renderer conflicts and snapshot changes rather than accepting them mechanically. Run the
tests required by `AGENTS.md`, then rebuild the installed package:

```bash
just install-rich-tui-local
codex-rich --version
```

After validation, push the merged `main` branch:

```bash
git push origin main
```

Scheduled Codex tasks can automate discovery and preparation of upstream updates, but their state
lives outside this repository. Keep merges supervised so UI conflicts, OAuth behavior, and
upstream protocol changes receive review.

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
