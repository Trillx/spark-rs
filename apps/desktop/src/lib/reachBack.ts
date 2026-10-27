// When the message list brings in older mail from the servers ("reaches
// back"). Scrolling toward the end does, whenever the end is near. A list
// also does on its own, to fill itself when it opens too short to scroll,
// but never because rows left it (Done, Archive, a move, Snooze, a read
// email leaving an unread list): a scroll has to then. The Inbox and folders
// fill for as long as a round adds rows, and reach again when new mail makes
// them grow. A sub-list (a card's "Show all", a Focused group) shows only
// some of the mail a round brings, so it fills for two rounds at most, and
// stops at one that adds none of its rows.

/** The most rounds a sub-list reaches back on its own after it opens; rounds by scroll count towards them. */
export const SUB_LIST_ROUNDS = 2;

/** A list as the last look at it left it. */
export interface Reach {
  listId: string;
  /** Its rows then. */
  rows: number;
  /** A round was out then, for it or another list. */
  loading: boolean;
  /** Its rows when it asked for a round, until that round is over. */
  asked: number | null;
  /** Rounds it has asked for since it opened. */
  rounds: number;
  /**
   * It reaches back the next time it's near the end: it opened, or since its
   * last round, it grew (the Inbox, a folder) or that round added rows (a
   * sub-list). Rows leaving clear it.
   */
  owed: boolean;
  /** A sub-list that lost rows: only a scroll reaches back now. */
  shrank: boolean;
}

/** The list now. */
export interface ListNow {
  /** Changes when another list opens. */
  listId: string;
  /** Its rows: `ListModel.order`. */
  rows: number;
  /** A card's "Show all" or a Focused group, not the Inbox or a folder. */
  sub: boolean;
  /** Its end is near the screen, or on it. */
  nearEnd: boolean;
  /** The servers have older mail for it. */
  older: boolean;
  /** A round is out, for it or another list. */
  loading: boolean;
}

export interface Step {
  state: Reach;
  /** Bring in older mail now. */
  reach: boolean;
}

/** The list, from what the last look saw (`was`) and what it shows now. A list the last look didn't see opened. */
function look(was: Reach | null, now: ListNow): Reach {
  if (was?.listId !== now.listId) {
    const { listId, rows, loading } = now;
    return { listId, rows, loading, asked: null, rounds: 0, owed: true, shrank: false };
  }
  // Its round is over once it has been seen out and isn't any more.
  const over = was.asked !== null && was.loading && !now.loading;
  const lost = now.rows < was.rows;
  const gained = now.sub ? over && now.rows > was.asked! : now.rows > was.rows;
  return {
    ...was,
    rows: now.rows,
    loading: now.loading,
    asked: over ? null : was.asked,
    owed: gained || (was.owed && !lost),
    shrank: was.shrank || (now.sub && lost),
  };
}

/** Whether a round can start now: the end is near, the servers have more, and none is out. */
const ready = (now: ListNow) => now.nearEnd && now.older && !now.loading;

/** A round asked for. */
const asking = (state: Reach): Step => ({
  state: { ...state, asked: state.rows, rounds: state.rounds + 1, owed: false },
  reach: true,
});

/** The list opened, changed, or a round started or ended: whether it reaches back on its own. */
export function changed(was: Reach | null, now: ListNow): Step {
  const state = look(was, now);
  const filled = now.sub && (state.shrank || state.rounds >= SUB_LIST_ROUNDS);
  return state.owed && !filled && ready(now) ? asking(state) : { state, reach: false };
}

/** The user scrolled toward the end, or wheeled, paged or pressed End at it: whether it reaches back. */
export function scrolled(was: Reach | null, now: ListNow): Step {
  const state = look(was, now);
  return ready(now) ? asking(state) : { state, reach: false };
}
