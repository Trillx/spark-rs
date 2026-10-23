import { useEffect, useState } from "react";
import { addDays } from "../lib/calendar";

/** Today's day of the month, updated at midnight. */
function useToday() {
  const [day, setDay] = useState(() => new Date().getDate());
  useEffect(() => {
    const now = Date.now();
    const timer = window.setTimeout(() => setDay(new Date().getDate()), addDays(now, 1) - now + 1000);
    return () => window.clearTimeout(timer);
  }, [day]);
  return day;
}

/** The Calendar button's icon, Spark's way: a calendar with today's date in it; the top band fills when open. */
export default function CalendarIcon({ size = 18, active = false }: { size?: number; active?: boolean }) {
  const day = useToday();
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M8 2v4M16 2v4" />
      <rect x="3" y="4" width="18" height="18" rx="2" />
      {active ? <path d="M3 6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v3H3z" fill="currentColor" /> : <path d="M3 9h18" />}
      <text
        x="12"
        y="18.6"
        textAnchor="middle"
        fontSize="8.5"
        fontWeight="600"
        fontFamily="Inter Variable, Inter, system-ui, sans-serif"
        fill="currentColor"
        stroke="none"
      >
        {day}
      </text>
    </svg>
  );
}
