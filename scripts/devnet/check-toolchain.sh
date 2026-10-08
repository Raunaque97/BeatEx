#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"
user_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -d "$user_cargo_bin" ]]; then
  PATH="$PATH:$user_cargo_bin"
fi
if [[ -d "$project_root/.devnet/tools/bin" ]]; then
  PATH="$project_root/.devnet/tools/bin:$PATH"
fi

failures=0

check_command() {
  local command_name="$1"
  local expected="$2"

  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'MISSING  %-24s required: %s\n' "$command_name" "$expected"
    failures=$((failures + 1))
    return
  fi

  local version
  version="$($command_name --version 2>&1 | head -n 1)"
  printf 'FOUND    %-24s %s\n' "$command_name" "$version"
}

printf 'BeatEx Devnet toolchain\n\n'
check_command anchor 'Anchor CLI 1.1.2'
check_command solana 'Solana/Agave CLI 3.1.10'
check_command solana-keygen 'bundled with Solana/Agave CLI 3.1.10'
check_command solana-test-validator 'bundled with Solana/Agave CLI 3.1.10'
check_command cargo-build-sbf 'platform tools installed by Solana/Agave CLI'
check_command spl-token 'SPL Token CLI 5.6.1 or newer (required for current Devnet ZK proofs)'

if (( failures > 0 )); then
  printf '\n%d required command(s) are missing. No tools were installed.\n' "$failures"
  exit 1
fi

printf '\nAll required commands are available. Compare the versions above with Anchor.toml.\n'
