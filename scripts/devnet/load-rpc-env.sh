#!/usr/bin/env bash

# Load RPC overrides without evaluating the file as shell code. The local file
# lives under .devnet (which is gitignored) so paid-provider credentials never
# enter tracked source.
beatex_load_rpc_env() {
  local state_dir="$1"
  local config_file="$state_dir/rpc.env"
  local key value

  [[ -f "$config_file" ]] || return 0
  while IFS='=' read -r key value; do
    case "$key" in
      BEATEX_RPC_URL|BEATEX_RPC_SESSION_URL|BEATEX_RPC_FALLBACK_URLS)
        export "$key=$value"
        ;;
      ''|'#'*) ;;
      *)
        printf 'Ignoring unsupported setting %s in %s.\n' "$key" "$config_file" >&2
        ;;
    esac
  done < "$config_file"
}
