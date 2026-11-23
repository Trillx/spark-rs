// What the reader's AI bar over selected text (`SelectionAi`) asks for.

/** The longest quote of the selection a question or reply request carries; the AI reads the whole email anyway. */
const QUOTE_MAX = 600;

/** The selection's text as it goes into a request: quoted, and cut short if it's long. */
export function quoteOf(text: string) {
  const cut = text.length > QUOTE_MAX ? `${text.slice(0, QUOTE_MAX).trimEnd()}…` : text;
  return `"${cut}"`;
}

/** What the bar asks the assistant, for Explain. */
export function explainQuestion(text: string) {
  return `What does this part of the email mean: ${quoteOf(text)}`;
}

/** What the bar asks the assistant, for a question typed into it. */
export function askQuestion(text: string, question: string) {
  return `About this part of the email, ${quoteOf(text)}: ${question.trim()}`;
}

/** What the reply composer's AI is asked to write, for Reply to this. */
export function replyInstructions(text: string) {
  return `respond to this part of it, ${quoteOf(text)}`;
}
