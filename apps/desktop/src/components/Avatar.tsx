import type { AccountId } from "../types";
import { useAccount } from "../lib/accounts";
import { avatarColor, initials } from "../lib/format";

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
  account?: AccountId;
}) {
  const ring = useAccount(account)?.color;
  return (
    <span
      className={`grid shrink-0 place-items-center rounded-full ${SIZES[size]} ${ring ? "border-2" : ""}`}
      style={ring ? { borderColor: ring } : undefined}
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
