import type { CodegenConfig } from "@graphql-codegen/cli";

const config: CodegenConfig = {
  schema: "../embedded/schema.graphql",
  documents: ["src/**/*.graphql"],
  generates: {
    "src/lib/graphql/generated/": {
      preset: "client",
      config: {
        documentMode: "string",
      },
    },
  },
};

export default config;
