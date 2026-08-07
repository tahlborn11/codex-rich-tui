#!/bin/zsh
set -euo pipefail

if (( $# > 2 )); then
  print -u2 "usage: $0 [package-directory] [install-directory]"
  exit 2
fi

repository_root="${0:A:h:h}"
package_directory="${1:-$repository_root/codex-rs/target/codex-rich-tui-dev}"
install_directory="${2:-${HOME}/.local/share/codex-rich-tui}"
launcher_directory="${CODEX_RICH_BIN_DIR:-${HOME}/.local/bin}"
signing_identity="${CODEX_RICH_SIGNING_IDENTITY:-Codex Rich Local Signing}"
staged_files=()

if [[ "$(uname -s)" != "Darwin" ]]; then
  print -u2 "the signed local installer currently supports macOS only"
  exit 1
fi

for required_command in codesign rsync security; do
  if ! command -v "$required_command" >/dev/null 2>&1; then
    print -u2 "missing required command: $required_command"
    exit 1
  fi
done

package_directory="${package_directory:A}"
install_directory="${install_directory:A}"
launcher_directory="${launcher_directory:A}"

if [[ ! -f "$package_directory/codex-package.json" ]]; then
  print -u2 "invalid Codex package (missing codex-package.json): $package_directory"
  exit 1
fi

for binary_name in codex codex-code-mode-host; do
  if [[ ! -x "$package_directory/bin/$binary_name" ]]; then
    print -u2 "missing package binary: $package_directory/bin/$binary_name"
    exit 1
  fi
done

if ! security find-identity -v -p codesigning \
  | grep -F "\"$signing_identity\"" >/dev/null; then
  print -u2 "code-signing identity not found: $signing_identity"
  print -u2 "create it using the macOS setup steps in RICH_TUI.md, then rerun this installer"
  exit 1
fi

launcher_path="$launcher_directory/codex-rich"
if [[ -e "$launcher_path" && ! -L "$launcher_path" ]]; then
  print -u2 "refusing to replace non-symlink launcher: $launcher_path"
  exit 1
fi

cleanup() {
  for staged_file in "${staged_files[@]}"; do
    if [[ "$staged_file" == "$install_directory/bin/".*.new.* ]]; then
      rm -f "$staged_file"
    fi
  done
}
trap cleanup EXIT

mkdir -p "$install_directory/bin" "$launcher_directory"

stage_signed_binary() {
  local binary_name="$1"
  local identifier="$2"
  local source_path="$package_directory/bin/$binary_name"
  local staged_path

  staged_path="$(mktemp "$install_directory/bin/.${binary_name}.new.XXXXXX")"
  staged_files+=("$staged_path")
  cp "$source_path" "$staged_path"
  chmod 755 "$staged_path"
  codesign \
    --force \
    --sign "$signing_identity" \
    --identifier "$identifier" \
    --timestamp=none \
    "$staged_path"
  local signing_requirement
  signing_requirement="$(codesign -d -r- "$staged_path" 2>&1)"
  if [[ "$signing_requirement" != *"identifier \"$identifier\""* ]] \
    || [[ "$signing_requirement" != *"certificate root ="* ]]; then
    print -u2 "unexpected signing requirement for $binary_name: $signing_requirement"
    exit 1
  fi
  REPLY="$staged_path"
}

stage_signed_binary codex com.tahlborn.codex-rich
staged_codex="$REPLY"
stage_signed_binary codex-code-mode-host com.tahlborn.codex-rich.code-mode-host
staged_code_mode_host="$REPLY"

# Install package metadata and managed runtime resources only after both
# executables are signed and validated. Keeping the binaries out of rsync avoids
# in-place Mach-O writes, which would invalidate their signatures and destabilize
# Keychain ACL matching.
rsync -a \
  --exclude '/bin/codex' \
  --exclude '/bin/codex-code-mode-host' \
  "$package_directory/" \
  "$install_directory/"

mv -f "$staged_codex" "$install_directory/bin/codex"
mv -f "$staged_code_mode_host" "$install_directory/bin/codex-code-mode-host"

ln -sfn "$install_directory/bin/codex" "$launcher_path"

print "Installed signed Codex Rich package in $install_directory"
print "Launcher: $launcher_path"
