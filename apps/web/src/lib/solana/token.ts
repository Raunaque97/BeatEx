import { TOKEN_2022_PROGRAM_ADDRESS } from "@beatex/sdk";
import {
  AccountRole,
  address,
  getAddressEncoder,
  getProgramDerivedAddress,
  type Address,
  type Instruction,
} from "@solana/kit";

export const ASSOCIATED_TOKEN_PROGRAM = address(
  "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
);
export const SYSTEM_PROGRAM = address("11111111111111111111111111111111");
export const TOKEN_2022_PROGRAM = address(TOKEN_2022_PROGRAM_ADDRESS);

const encoder = getAddressEncoder();

/** The owner's associated Token-2022 account for `mint`. */
export async function associatedTokenAddress(
  owner: Address,
  mint: Address,
): Promise<Address> {
  const [ata] = await getProgramDerivedAddress({
    programAddress: ASSOCIATED_TOKEN_PROGRAM,
    seeds: [
      encoder.encode(owner),
      encoder.encode(TOKEN_2022_PROGRAM),
      encoder.encode(mint),
    ],
  });
  return ata;
}

/** Token amount from an SPL / Token-2022 account's base layout (u64 at offset 64). */
export function tokenAccountAmount(data: Uint8Array): bigint {
  if (data.length < 72) throw new Error("not a token account");
  return new DataView(data.buffer, data.byteOffset + 64, 8).getBigUint64(
    0,
    true,
  );
}

export function createAssociatedTokenAccountIdempotent(
  payer: Address,
  owner: Address,
  mint: Address,
  ata: Address,
): Instruction {
  return {
    programAddress: ASSOCIATED_TOKEN_PROGRAM,
    accounts: [
      { address: payer, role: AccountRole.WRITABLE_SIGNER },
      { address: ata, role: AccountRole.WRITABLE },
      { address: owner, role: AccountRole.READONLY },
      { address: mint, role: AccountRole.READONLY },
      { address: SYSTEM_PROGRAM, role: AccountRole.READONLY },
      { address: TOKEN_2022_PROGRAM, role: AccountRole.READONLY },
    ],
    data: Uint8Array.of(1),
  };
}

/** Token-2022 `TransferChecked` (instruction 12). */
export function transferChecked(
  source: Address,
  mint: Address,
  destination: Address,
  authority: Address,
  amount: bigint,
  decimals: number,
): Instruction {
  const data = new Uint8Array(10);
  data[0] = 12;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  data[9] = decimals;
  return {
    programAddress: TOKEN_2022_PROGRAM,
    accounts: [
      { address: source, role: AccountRole.WRITABLE },
      { address: mint, role: AccountRole.READONLY },
      { address: destination, role: AccountRole.WRITABLE },
      { address: authority, role: AccountRole.READONLY_SIGNER },
    ],
    data,
  };
}

/** System Program transfer of `lamports` from a signing `source`. */
export function systemTransfer(
  source: Address,
  destination: Address,
  lamports: bigint,
): Instruction {
  const data = new Uint8Array(12);
  const view = new DataView(data.buffer);
  view.setUint32(0, 2, true);
  view.setBigUint64(4, lamports, true);
  return {
    programAddress: SYSTEM_PROGRAM,
    accounts: [
      { address: source, role: AccountRole.WRITABLE_SIGNER },
      { address: destination, role: AccountRole.WRITABLE },
    ],
    data,
  };
}
