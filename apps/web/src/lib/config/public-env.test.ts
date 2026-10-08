import { describe, expect, it } from "vitest";

import { publicEnv } from "./public-env";

describe("publicEnv", () => {
  it("defaults to the deployed devnet program", () => {
    expect(publicEnv.cluster).toBe("devnet");
    expect(publicEnv.programId).toBe(
      "2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd",
    );
  });
});
