import { createContext, useContext } from "react";
import type { AccountId, AccountView } from "../types";

// The accounts App loaded from `listAccounts`, for components deep in the tree
// (avatar rings, the composer's From, the thread header).

export const AccountsContext = createContext<AccountView[]>([]);

export function useAccounts() {
  return useContext(AccountsContext);
}

export function useAccount(id: AccountId | undefined) {
  return useAccounts().find((a) => a.id === id);
}

/** Only the built-in demo accounts exist (none have loaded yet counts as not demo). */
export function isDemoMode(accounts: AccountView[]) {
  return accounts.length > 0 && accounts.every((a) => a.demo);
}
