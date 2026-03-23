import { betterAuth } from "better-auth";
import { bearer } from "better-auth/plugins/bearer";
import { organization } from "better-auth/plugins/organization";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { db } from "../db/index.js";
import * as schema from "../db/schema.js";

export const auth = betterAuth({
  database: drizzleAdapter(db, { provider: "pg", schema }),
  emailAndPassword: {
    enabled: true,
  },
  plugins: [
    bearer(),
    organization(),
  ],
  trustedOrigins: process.env.TRUSTED_ORIGINS?.split(",") || [
    "http://localhost:3100",
    "http://localhost:5175",
  ],
});
