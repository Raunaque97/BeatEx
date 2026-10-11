#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"
source "$script_dir/load-rpc-env.sh"
npm_cache="${NPM_CONFIG_CACHE:-$project_root/.devnet/npm-cache}"
mkdir -p "$npm_cache"
export NPM_CONFIG_CACHE="$npm_cache"

program_id=''
idl_file=''
wallet=''
rpc_url=''
state_dir='.devnet/devnet'
allow_update=0

usage() {
  cat <<'EOF'
Usage:
  publish-idl.sh --program-id <ADDRESS> --idl <FILE> --wallet <KEYPAIR> \
    --rpc-url <URL> [options]

Options:
  --program-id <ADDRESS>  Deployed Solana program address
  --idl <FILE>            Anchor IDL JSON file
  --wallet <KEYPAIR>      Program authority and transaction fee payer
  --state-dir <DIRECTORY> RPC environment directory (default: .devnet/devnet)
  --rpc-url <URL>         Localnet or Devnet RPC URL
  --allow-update          Authorize publication without an interactive prompt
  -h, --help              Show this help

The current canonical IDL is fetched first. An identical IDL is left unchanged.
A differing IDL, or an inconclusive fetch, requires interactive confirmation
unless --allow-update was explicitly supplied by an enclosing workflow.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --program-id)
      program_id="${2:?--program-id requires a value}"
      shift 2
      ;;
    --idl)
      idl_file="${2:?--idl requires a value}"
      shift 2
      ;;
    --wallet)
      wallet="${2:?--wallet requires a value}"
      shift 2
      ;;
    --rpc-url)
      rpc_url="${2:?--rpc-url requires a value}"
      shift 2
      ;;
    --state-dir)
      state_dir="${2:?--state-dir requires a value}"
      shift 2
      ;;
    --allow-update)
      allow_update=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$program_id" ]]; then
  printf '%s\n' 'A --program-id is required.' >&2
  exit 2
fi
if [[ -z "$idl_file" || ! -f "$idl_file" ]]; then
  printf 'A valid --idl file is required: %s\n' "$idl_file" >&2
  exit 2
fi
if [[ -z "$wallet" || ! -f "$wallet" ]]; then
  printf 'A valid --wallet keypair is required: %s\n' "$wallet" >&2
  exit 2
fi
if [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi
beatex_load_rpc_env "$state_dir"
if [[ "$rpc_url" == 'https://api.devnet.solana.com' ]]; then
  rpc_url="${BEATEX_RPC_SESSION_URL:-${BEATEX_RPC_URL:-$rpc_url}}"
fi
case "$rpc_url" in
  http://*|https://*) ;;
  *)
    printf 'Unsupported RPC URL %q; expected an http(s) URL.\n' "$rpc_url" >&2
    exit 2
    ;;
esac
case "$rpc_url" in
  *[Mm][Aa][Ii][Nn][Nn][Ee][Tt]*|*[Mm][Aa][Ii][Nn][Nn][Ee][Tt]-[Bb][Ee][Tt][Aa]*)
    printf 'Mainnet RPC URLs are not allowed: %q\n' "$rpc_url" >&2
    exit 2
    ;;
esac

for required_command in node npx; do
  if ! command -v "$required_command" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$required_command" >&2
    exit 1
  fi
done

if ! node -e \
  'JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8"));' \
  "$idl_file" >/dev/null 2>&1; then
  printf 'IDL is not valid JSON: %s\n' "$idl_file" >&2
  exit 1
fi

# Compare parsed JSON so whitespace and object-key ordering do not trigger a
# metadata transaction.
json_equal() {
  node -e '
    try {
      const fs = require("node:fs");
      const normalize = (value) => {
        if (Array.isArray(value)) return value.map(normalize);
        if (value && typeof value === "object") {
          return Object.fromEntries(
            Object.keys(value).sort().map((key) => [key, normalize(value[key])]),
          );
        }
        return value;
      };
      const left = normalize(JSON.parse(fs.readFileSync(process.argv[1], "utf8")));
      const right = normalize(JSON.parse(fs.readFileSync(process.argv[2], "utf8")));
      process.exit(JSON.stringify(left) === JSON.stringify(right) ? 0 : 1);
    } catch (error) {
      console.error(error.message);
      process.exit(2);
    }
  ' "$1" "$2"
}

temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/beatex-idl.XXXXXX")"
trap 'rm -rf "$temporary_directory"' EXIT
published_idl="$temporary_directory/published-idl.json"
fetch_log="$temporary_directory/fetch.log"

should_publish=0
if npx --yes @solana-program/program-metadata@latest fetch idl "$program_id" \
  --rpc "$rpc_url" --output "$published_idl" >"$fetch_log" 2>&1; then
  if json_equal "$idl_file" "$published_idl"; then
    printf 'Canonical IDL metadata for %s is already current.\n' "$program_id"
    exit 0
  else
    comparison_status=$?
    if (( comparison_status != 1 )); then
      printf '%s\n' 'The fetched canonical IDL was not valid JSON; refusing to replace it.' >&2
      exit 1
    fi
  fi

  printf 'The canonical IDL metadata for %s differs from %s.\n' \
    "$program_id" "$idl_file"
  if (( allow_update )); then
    should_publish=1
  elif [[ -t 0 ]]; then
    read -r -p 'Replace the published IDL metadata? [y/N] ' confirmation
    case "$confirmation" in
      y|Y|yes|YES|Yes) should_publish=1 ;;
      *) printf '%s\n' 'Published IDL metadata left unchanged.' ;;
    esac
  else
    printf '%s\n' 'Published IDL metadata left unchanged in non-interactive mode. Pass --allow-update after explicit approval.'
  fi
else
  printf 'Could not fetch canonical IDL metadata for %s.\n' "$program_id"
  sed -n '1,12p' "$fetch_log" >&2
  if (( allow_update )); then
    should_publish=1
  elif [[ -t 0 ]]; then
    read -r -p 'Publish the local IDL despite the inconclusive fetch? [y/N] ' confirmation
    case "$confirmation" in
      y|Y|yes|YES|Yes) should_publish=1 ;;
      *) printf '%s\n' 'IDL metadata was not published.' ;;
    esac
  else
    printf '%s\n' 'IDL metadata was not published in non-interactive mode. Pass --allow-update after explicit approval.'
  fi
fi

if (( ! should_publish )); then
  exit 0
fi

npx --yes @solana-program/program-metadata@latest write idl \
  "$program_id" "$idl_file" \
  --keypair "$wallet" \
  --rpc "$rpc_url"

printf 'Published canonical IDL metadata for %s from %s.\n' \
  "$program_id" "$idl_file"
