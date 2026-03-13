import { Provider } from "urql";
import { graphqlClient } from "./client";

export function GraphQLProvider({ children }: { children: React.ReactNode }) {
  return <Provider value={graphqlClient}>{children}</Provider>;
}
