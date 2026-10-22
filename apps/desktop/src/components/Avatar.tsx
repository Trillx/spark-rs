import type { Account } from "../types";
import { ACCOUNTS, avatarColor, initials } from "../lib/format";

const SIZES = {
  xs: "size-4 text-[7px]",
  sm: "size-[26px] text-[10px]",
  md: "size-9 text-xs",
  lg: "size-[38px] text-[13px]",
  xl: "size-14 text-lg",
} as const;

/** Initials on a color hashed from the name; `account` adds the account-color ring. */
export default function Avatar({
  name,
  size = "sm",
  account,
}: {
  name: string;
  size?: keyof typeof SIZES;
  account?: Account;
}) {
  return (
    <span
      className={`grid shrink-0 place-items-center rounded-full ${SIZES[size]} ${
        account ? `border-2 ${ACCOUNTS[account].ring}` : ""
      }`}
      aria-hidden
    >
      <span
        className={`grid size-full place-items-center rounded-full font-semibold leading-none text-white ${avatarColor(name)}`}
      >
        {initials(name)}
      </span>
    </span>
  );
}
