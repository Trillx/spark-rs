// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import { SUB_LIST_ROUNDS, changed, scrolled, type ListNow, type Reach } from "./reachBack.ts";

/**
 * A list as MessageList sees it: each change or scroll is a look, and says
 * whether it reached back. A round it asks for goes out at the next change,
 * as App's `loading` does, and `arrive` ends it with what it brought.
 */
function list(start: Partial<ListNow> = {}) {
  let state: Reach | null = null;
  let now: ListNow = { listId: "inbox", rows: 3, sub: false, nearEnd: true, older: true, loading: false, ...start };
  let reached = 0;
  const step = (look: typeof changed, patch: Partial<ListNow>) => {
    now = { ...now, ...patch };
    const next = look(state, now);
    state = next.state;
    if (next.reach) reached++;
    return next.reach;
  };
  const change = (patch: Partial<ListNow> = {}) => step(changed, patch);
  return {
    change,
    scroll: (patch: Partial<ListNow> = {}) => step(scrolled, patch),
    /** The round goes out. */
    out: () => change({ loading: true }),
    /** The round comes back with `rows` more for this list, and is over. */
    arrive: (rows: number, patch: Partial<ListNow> = {}) => change({ rows: now.rows + rows, loading: false, ...patch }),
    get rows() {
      return now.rows;
    },
    get reached() {
      return reached;
    },
  };
}

test("A list that opens too short to scroll reaches back on its own", () => {
  for (const sub of [false, true]) {
    const l = list({ sub });
    assert.equal(l.change(), true, sub ? "a sub-list" : "the Inbox");
    assert.equal(l.out(), false, "not while the round is out");
  }
  const long = list({ nearEnd: false });
  assert.equal(long.change(), false, "a list that fills the screen waits for a scroll");
  assert.equal(list({ older: false }).change(), false, "nothing older on the servers");
});

test("A sub-list stops filling after a round that adds none of its rows", () => {
  const l = list({ listId: "inbox|all|focused|group:newsletters|", sub: true });
  assert.equal(l.change(), true);
  l.out();
  assert.equal(l.arrive(0), false, "the round brought only other kinds of mail");
  assert.equal(l.change(), false, "a later change doesn't start it again");
  assert.equal(l.change({ rows: l.rows + 1 }), false, "nor does new mail");
  assert.equal(l.reached, 1);
});

test("A sub-list fills for two rounds at most", () => {
  assert.equal(SUB_LIST_ROUNDS, 2);
  const l = list({ listId: "inbox|all|cards|card:people|", sub: true });
  assert.equal(l.change(), true);
  l.out();
  assert.equal(l.arrive(2), true, "the first round added rows: another");
  l.out();
  assert.equal(l.arrive(2), false, "still short, but that was the second");
  assert.equal(l.change({ rows: l.rows + 1 }), false);
  assert.equal(l.reached, 2);
  // Back to the cards, which don't reach back, and into the card again: it fills again.
  assert.equal(l.change({ listId: "inbox|all|cards||", sub: false, older: false }), false);
  assert.equal(l.change({ listId: "inbox|all|cards|card:people|", sub: true, older: true }), true);
});

test("A list that shrank doesn't reach back on its own", () => {
  // Opened long, then Done, Archive, Snooze or a move takes rows near the end.
  for (const sub of [false, true]) {
    const l = list({ sub, rows: 20, nearEnd: false });
    assert.equal(l.change(), false);
    assert.equal(l.change({ rows: 19, nearEnd: true }), false, sub ? "a sub-list" : "the Inbox");
    assert.equal(l.change({ rows: 18 }), false);
    assert.equal(l.change(), false, "nor at a later change that keeps its rows");
  }
  // A read email leaving an unread card's "Show all", right after it opened.
  const unread = list({ listId: "inbox|all|cards|card:people|", sub: true, rows: 4, nearEnd: false });
  unread.change();
  unread.change({ rows: 3, nearEnd: true });
  assert.equal(unread.change({ rows: 4 }), false, "a sub-list that shrank stays put even when it grows");
  assert.equal(unread.reached, 0);
});

test("A sub-list that shrinks while it fills stops filling", () => {
  const l = list({ sub: true, rows: 5 });
  assert.equal(l.change(), true);
  l.out();
  assert.equal(l.change({ rows: 4 }), false, "Done during the round");
  assert.equal(l.arrive(3), false, "the round added rows, but the list shrank");
});

test("The Inbox and folders fill until a round adds nothing, and reach again when they grow", () => {
  const l = list({ listId: "folder:7|all|cards||", rows: 2 });
  assert.equal(l.change(), true);
  l.out();
  assert.equal(l.arrive(5), true, "still short after a round that added rows: another");
  l.out();
  assert.equal(l.arrive(5), true, "no limit on rounds");
  l.out();
  assert.equal(l.arrive(0), false, "a round that added nothing ends it");
  assert.equal(l.change(), false);
  assert.equal(l.change({ rows: l.rows + 1 }), true, "new mail made it grow, still near the end");
  l.out();
  assert.equal(l.change({ rows: l.rows + 1 }), false, "more new mail, while the round is out");
  assert.equal(l.arrive(0), true, "it grew since it last reached");
});

test("Scrolling, wheeling or paging at the end reaches back whenever a round can start", () => {
  const l = list({ sub: true, rows: 30, nearEnd: false });
  l.change();
  assert.equal(l.scroll(), false, "scrolled, but not near the end");
  assert.equal(l.scroll({ nearEnd: true }), true);
  assert.equal(l.out(), false);
  assert.equal(l.scroll(), false, "a round is out");
  assert.equal(l.arrive(0), false);
  assert.equal(l.change({ rows: 29 }), false, "a Done near the end");
  assert.equal(l.scroll(), true, "a scroll after it shrank");
  l.out();
  l.arrive(0);
  assert.equal(l.scroll(), true, "a scroll past the sub-list's rounds");
  l.out();
  assert.equal(l.arrive(0, { older: false }), false);
  assert.equal(l.scroll(), false, "nothing older on the servers");
});

test("A round counts once it's been seen out, and only the list's own", () => {
  // Opened while another list's round was out: that round's end isn't this list's.
  const l = list({ sub: true, loading: true });
  assert.equal(l.change(), false);
  assert.equal(l.change({ loading: false }), true, "it fills once that round is over");
  // A look before the round shows as out (React hasn't rendered App's `loading` yet).
  assert.equal(l.change(), false);
  assert.equal(l.change({ loading: true }), false);
  assert.equal(l.arrive(1), true, "its round added a row: the second round");
});
