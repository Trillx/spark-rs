/** An account's color: a ring with a dot, as on the sidebar's per-account Inbox rows. */
export default function AccountMark({ color, size = "sm" }: { color: string; size?: "sm" | "lg" }) {
  return (
    <span
      aria-hidden
      className={`grid shrink-0 place-items-center rounded-full border-2 ${size === "lg" ? "size-9" : "size-4"}`}
      style={{ borderColor: color }}
    >
      <span className={`rounded-full ${size === "lg" ? "size-3" : "size-1.5"}`} style={{ backgroundColor: color }} />
    </span>
  );
}
