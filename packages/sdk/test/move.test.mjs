import assert from "node:assert/strict";
import test from "node:test";

import {
  TOKEN_2022_PROGRAM_ADDRESS,
  deriveAssociatedToken2022Address,
} from "../dist/index.js";

test("associated Token-2022 accounts match the wallet derivation", async () => {
  // Same owner, mint, and expected account as the web app's token helper test.
  const [account] = await deriveAssociatedToken2022Address(
    "GYuK4aof3fS7M1dzYzwLrrdokdQsVieuPfVuEBxjCitq",
    "33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm",
    TOKEN_2022_PROGRAM_ADDRESS,
  );
  assert.equal(account, "CHSkbKWPe9cMNH8fgxWyzgn1VbZFnnBE1UheeveYzEUr");
});
