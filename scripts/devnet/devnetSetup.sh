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

TOKEN_2022_PROGRAM_ID='TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb'
cluster='devnet'
wallet=''
state_dir=''
skip_deploy=0
redeploy=0
skip_idl=0
allow_idl_update=0
program_keypair='.devnet/program/beatex-keypair.json'
idl_file='target/idl/beatex.json'
custom_idl=0

usage() {
  cat <<'EOF'
Usage:
  devnetSetup.sh --wallet <KEYPAIR> [options]

Options:
  --cluster localnet|devnet       Target cluster (default: devnet)
  --wallet <KEYPAIR>              Fee payer and test-token mint authority
  --state-dir <DIRECTORY>         Persistent setup state (default: .devnet/<cluster>)
  --program-keypair <KEYPAIR>     Stable BeatEx program keypair
  --skip-deploy                   Keep the currently deployed program
  --redeploy                      Explicitly authorize upgrading an existing program
  --idl <FILE>                    IDL to publish (default: target/idl/beatex.json)
  --skip-idl                      Do not generate or publish program metadata
  --allow-idl-update              Explicitly authorize replacing existing IDL metadata
  -h, --help                       Show this help

The script refuses mainnet. It checks every configured program before deploying
and asks before upgrading an existing deployment. It creates tBTC/tUSD, creates
two market-maker keypairs, funds them with SOL and test tokens, initializes the
market, and publishes the IDL through Solana Program Metadata.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --cluster)
      cluster="${2:?--cluster requires a value}"
      shift 2
      ;;
    --wallet)
      wallet="${2:?--wallet requires a value}"
      shift 2
      ;;
    --state-dir)
      state_dir="${2:?--state-dir requires a value}"
      shift 2
      ;;
    --program-keypair)
      program_keypair="${2:?--program-keypair requires a value}"
      shift 2
      ;;
    --skip-deploy)
      skip_deploy=1
      shift
      ;;
    --redeploy)
      redeploy=1
      shift
      ;;
    --idl)
      idl_file="${2:?--idl requires a value}"
      custom_idl=1
      shift 2
      ;;
    --skip-idl)
      skip_idl=1
      shift
      ;;
    --allow-idl-update)
      allow_idl_update=1
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

case "$cluster" in
  localnet) rpc_url='http://127.0.0.1:8899' ;;
  devnet) rpc_url='https://api.devnet.solana.com' ;;
  *)
    printf 'Unsupported cluster %q; only localnet and devnet are allowed.\n' "$cluster" >&2
    exit 2
    ;;
esac

if [[ -z "$wallet" || ! -f "$wallet" ]]; then
  printf 'A valid --wallet keypair is required.\n' >&2
  exit 2
fi
if (( skip_deploy && redeploy )); then
  printf '%s\n' '--skip-deploy and --redeploy cannot be used together.' >&2
  exit 2
fi
wallet="$(cd "$(dirname "$wallet")" && pwd)/$(basename "$wallet")"
if [[ -z "$state_dir" ]]; then
  state_dir=".devnet/$cluster"
fi
if [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi
if [[ "$program_keypair" != /* ]]; then
  program_keypair="$project_root/$program_keypair"
fi

"$script_dir/check-toolchain.sh"

if [[ ! -f "$program_keypair" ]]; then
  printf 'Program keypair does not exist: %s\n' "$program_keypair" >&2
  exit 2
fi
program_id="$(solana-keygen pubkey "$program_keypair")"
program_is_deployed=0
program_status=''
if program_status="$(solana program show "$program_id" --url "$rpc_url" 2>&1)"; then
  program_is_deployed=1
  printf '\nFound existing BeatEx deployment on %s:\n' "$cluster"
  printf '%s\n' "$program_status"
elif [[ ! "$program_status" =~ [Aa]ccount.*does[[:space:]]not[[:space:]]exist ]]; then
  printf 'Could not determine whether BeatEx program %s is deployed on %s:\n%s\n' \
    "$program_id" "$cluster" "$program_status" >&2
  exit 1
fi

if (( program_is_deployed && ! skip_deploy && ! redeploy )); then
  if [[ -t 0 ]]; then
    read -r -p 'Redeploy and upgrade BeatEx? [y/N] ' confirmation
    case "$confirmation" in
      y|Y|yes|YES|Yes) redeploy=1 ;;
      *) skip_deploy=1 ;;
    esac
  else
    printf '%s\n' 'Existing deployment left unchanged in non-interactive mode. Pass --redeploy to authorize an upgrade.'
    skip_deploy=1
  fi
elif (( ! program_is_deployed && skip_deploy )); then
  printf '%s\n' '--skip-deploy was requested, but the configured BeatEx program is not deployed.' >&2
  exit 1
fi

"$script_dir/setup-test-tokens.sh" \
  --cluster "$cluster" \
  --wallet "$wallet" \
  --state-dir "$state_dir"

if (( ! skip_deploy )); then
  deployment_args=(
    --cluster "$cluster"
    --wallet "$wallet"
    --program-keypair "$program_keypair"
    --state-dir "$state_dir"
  )
  if (( redeploy )); then
    deployment_args+=(--allow-redeploy)
  fi
  "$script_dir/deploy-beatex.sh" "${deployment_args[@]}"
else
  printf 'Keeping deployed BeatEx program %s unchanged.\n' "$program_id"
fi

if (( ! skip_idl )); then
  if [[ "$idl_file" != /* ]]; then
    idl_file="$project_root/$idl_file"
  fi
  if (( ! custom_idl )); then
    printf '\nGenerating the current Anchor IDL.\n'
    (
      cd "$project_root"
      anchor idl build
    )
  fi
  if [[ ! -f "$idl_file" ]]; then
    printf 'IDL file does not exist: %s\n' "$idl_file" >&2
    exit 1
  fi
  idl_args=(
    --program-id "$program_id"
    --idl "$idl_file"
    --wallet "$wallet"
    --rpc-url "$rpc_url"
  )
  if (( allow_idl_update )); then
    idl_args+=(--allow-update)
  fi
  "$script_dir/publish-idl.sh" "${idl_args[@]}"
fi

config_file="$state_dir/solana-cli.yml"
solana_args=(--config "$config_file")
token_args=(
  --config "$config_file"
  --fee-payer "$wallet"
  --program-id "$TOKEN_2022_PROGRAM_ID"
)
addresses_file="$state_dir/addresses.tsv"
tbtc_mint="$(awk -F '\t' '$1 == "tBTC" { print $2 }' "$addresses_file")"
tusd_mint="$(awk -F '\t' '$1 == "tUSD" { print $2 }' "$addresses_file")"
if [[ -z "$tbtc_mint" || -z "$tusd_mint" ]]; then
  printf 'Could not read tBTC/tUSD mints from %s.\n' "$addresses_file" >&2
  exit 1
fi

account_exists() {
  solana "${solana_args[@]}" account "$1" --output json-compact >/dev/null 2>&1
}

ensure_keypair() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    solana-keygen new --no-bip39-passphrase --silent --outfile "$path" >&2
  fi
}

ensure_owner_token_account() {
  local owner_keypair="$1"
  local mint="$2"
  local owner_pubkey account
  owner_pubkey="$(solana-keygen pubkey "$owner_keypair")"
  account="$(
    spl-token "${token_args[@]}" address \
      --verbose --owner "$owner_pubkey" --token "$mint" |
      sed -n 's/^Associated token address: //p'
  )"
  if [[ -z "$account" ]]; then
    printf 'Could not derive the Token-2022 account for owner %s.\n' "$owner_pubkey" >&2
    exit 1
  fi
  if ! account_exists "$account"; then
    spl-token "${token_args[@]}" create-account "$mint" --owner "$owner_pubkey" >&2 \
      || return 1
  fi
  account_exists "$account" || return 1
  if ! spl-token "${token_args[@]}" display "$account" | grep -Eiq 'confidential.?transfer'; then
    spl-token "${token_args[@]}" configure-confidential-transfer-account \
      --address "$account" --owner "$owner_keypair" >&2 || return 1
  fi
  printf '%s' "$account"
}

fund_if_empty() {
  local mint="$1"
  local account="$2"
  local amount="$3"
  local balance
  balance="$(
    spl-token "${token_args[@]}" balance --address "$account" |
      tr -d '[:space:],'
  )"
  if [[ "$balance" =~ ^0([.]0+)?$ ]]; then
    spl-token "${token_args[@]}" mint "$mint" "$amount" "$account" >&2 || return 1
  fi
}

mkdir -p "$state_dir/market-makers"
market_makers_file="$state_dir/market-makers.tsv"
printf 'name\tstrategy\towner\tkeypair\ttbtc-account\ttusd-account\n' >"$market_makers_file"

for specification in 'pyth-maker:pyth' 'binance-maker:binance'; do
  name="${specification%%:*}"
  strategy="${specification##*:}"
  keypair="$state_dir/market-makers/$name.json"
  ensure_keypair "$keypair"
  owner="$(solana-keygen pubkey "$keypair")"

  # The payer funds transaction fees without relying on faucet availability.
  owner_lamports="$(solana "${solana_args[@]}" balance "$owner" --lamports 2>/dev/null | awk '{print $1}')"
  if [[ ! "$owner_lamports" =~ ^[0-9]+$ ]] || (( owner_lamports < 250000000 )); then
    solana "${solana_args[@]}" transfer \
      --allow-unfunded-recipient --keypair "$wallet" "$owner" 1 >/dev/null
  fi

  tbtc_account="$(ensure_owner_token_account "$keypair" "$tbtc_mint")"
  tusd_account="$(ensure_owner_token_account "$keypair" "$tusd_mint")"
  fund_if_empty "$tbtc_mint" "$tbtc_account" 2
  fund_if_empty "$tusd_mint" "$tusd_account" 50000
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$strategy" "$owner" "$keypair" "$tbtc_account" "$tusd_account" \
    >>"$market_makers_file"
done

"$script_dir/setup-market.sh" \
  --cluster "$cluster" \
  --wallet "$wallet" \
  --state-dir "$state_dir"

artifact_args=(
  --cluster "$cluster"
  --state-dir "$state_dir"
)
if [[ -f "$idl_file" ]]; then
  artifact_args+=(--idl "$idl_file")
fi
node "$project_root/scripts/generate-protocol-artifacts.mjs" "${artifact_args[@]}"

printf '\nDevnet setup complete.\nAssets: %s\nMarket makers: %s\n' \
  "$addresses_file" "$market_makers_file"
