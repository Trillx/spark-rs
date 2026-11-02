import type { AccountView } from "../types";

/**
 * An account's first letter on a tile of its color: on the rail, where a mark
 * of color alone doesn't say which account it is.
 */
export default function AccountTile({ account, size }: { account: Pick<AccountView, "label" | "email" | "color">; size: number }) {
  const letter = [...(account.label.trim() || account.email)][0]?.toLocaleUpperCase() ?? "?";
  return (
    <span
      aria-hidden
      className="grid shrink-0 place-items-center font-semibold leading-none text-white"
      style={{
        width: size,
        height: size,
        borderRadius: Math.round(size * 0.3),
        fontSize: Math.max(8, Math.round(size * 0.5)),
        backgroundColor: account.color,
      }}
    >
      {letter}
    </span>
  );
}
