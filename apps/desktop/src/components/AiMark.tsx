/** The "+ai" mark Spark puts on every AI control: lowercase text in the AI gradient. */
export default function AiMark({ className = "" }: { className?: string }) {
  return (
    <span aria-hidden className={`text-ai font-bold leading-none tracking-tight ${className}`}>
      +ai
    </span>
  );
}
