#!/bin/zsh
set -euo pipefail

if (( $# > 2 )); then
  print -u2 "usage: $0 [package-directory] [install-directory]"
  exit 2
fi

repository_root="${0:A:h:h}"
package_directory="${1:-$repository_root/codex-rs/target/codex-rich-tui-dev}"
install_directory="${2:-${HOME}/.local/share/codex-rich-tui}"
signing_identity="${CODEX_RICH_SIGNING_IDENTITY:-Codex Rich Local Signing}"
staged_files=()

cleanup() {
  for staged_file in "${staged_files[@]}"; do
    if [[ "$staged_file" == "$install_directory/bin/".*.new.* ]]; then
      rm -f "$staged_file"
    fi
  done
}
trap cleanup EXIT

mkdir -p "$install_directory/bin"

install_signed_binary() {
  local binary_name="$1"
  local identifier="$2"
  local source_path="$package_directory/bin/$binary_name"
  local destination_path="$install_directory/bin/$binary_name"
  local staged_path

  if [[ ! -x "$source_path" ]]; then
    print -u2 "missing package binary: $source_path"
    exit 1
  fi

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
  mv -f "$staged_path" "$destination_path"
}

install_signed_binary codex com.tahlborn.codex-rich
install_signed_binary codex-code-mode-host com.tahlborn.codex-rich.code-mode-host

print "Installed signed Codex Rich binaries in $install_directory/bin"
