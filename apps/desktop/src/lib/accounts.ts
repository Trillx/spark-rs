import { createContext, useContext } from "react";
import type { AccountId, AccountView, Message } from "../types";

// The accounts App loaded from `listAccounts`, for components deep in the tree
// (avatar rings, the composer's From, the thread header).

export const AccountsContext = createContext<AccountView[]>([]);

export function useAccounts() {
  return useContext(AccountsContext);
}

export function useAccount(id: AccountId | undefined) {
  return useAccounts().find((a) => a.id === id);
}

/** `m` came from its own account's address: it's mail you sent. */
export function isFromYou(m: Message, account: AccountView | undefined) {
  return account !== undefined && m.address.toLowerCase() === account.email.toLowerCase();
}

/** Only the built-in demo accounts exist (none have loaded yet counts as not demo). */
export function isDemoMode(accounts: AccountView[]) {
  return accounts.length > 0 && accounts.every((a) => a.demo);
}
