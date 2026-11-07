import { TOKEN_2022_PROGRAM_ADDRESS } from "@beatex/sdk";
import {
  AccountRole,
  address,
  getAddressEncoder,
  getProgramDerivedAddress,
  type AccountSignerMeta,
  type Address,
  type Instruction,
  type TransactionSigner,
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

/** Token-2022 `MintToChecked`, signed by the mint's authority. */
export function mintToChecked(
  mint: Address,
  destination: Address,
  authority: Address,
  amount: bigint,
  decimals: number,
): Instruction {
  const data = new Uint8Array(10);
  data[0] = 14;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  data[9] = decimals;
  return {
    programAddress: TOKEN_2022_PROGRAM,
    accounts: [
      { address: mint, role: AccountRole.WRITABLE },
      { address: destination, role: AccountRole.WRITABLE },
      { address: authority, role: AccountRole.READONLY_SIGNER },
    ],
    data,
  };
}

/** Token-2022 `CloseAccount`: an empty account's rent goes to `destination`. */
export function closeTokenAccount(
  account: Address,
  destination: Address,
  owner: Address,
): Instruction {
  return {
    programAddress: TOKEN_2022_PROGRAM,
    accounts: [
      { address: account, role: AccountRole.WRITABLE },
      { address: destination, role: AccountRole.WRITABLE },
      { address: owner, role: AccountRole.READONLY_SIGNER },
    ],
    data: Uint8Array.of(9),
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

/**
 * System Program account holding only `lamports`, for a signing `account`. Unlike
 * a transfer it fails if the account already exists, so only one caller can win.
 */
export function createSystemAccount(
  payer: Address,
  account: TransactionSigner,
  lamports: bigint,
): Instruction {
  const data = new Uint8Array(52);
  const view = new DataView(data.buffer);
  view.setBigUint64(4, lamports, true);
  data.set(encoder.encode(SYSTEM_PROGRAM), 20);
  return {
    programAddress: SYSTEM_PROGRAM,
    accounts: [
      { address: payer, role: AccountRole.WRITABLE_SIGNER },
      {
        address: account.address,
        role: AccountRole.WRITABLE_SIGNER,
        signer: account,
      } as AccountSignerMeta,
    ],
    data,
  };
}
