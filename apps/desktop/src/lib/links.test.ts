import assert from "node:assert/strict";
import { test } from "node:test";
import { linkVerdict, openLink, setLinkHandlers } from "./links.ts";

test("web, mail and phone links open; script and file links never; others ask", () => {
  const verdict = (href: string) => linkVerdict(href).verdict;
  assert.equal(verdict("https://x.com/a"), "open");
  assert.equal(verdict("MAILTO:ana@x.com"), "open");
  assert.equal(verdict("tel:+15551234"), "open");
  assert.equal(verdict(" javascript:alert(1)"), "refuse");
  assert.equal(verdict("file:///C:/Windows/system32/calc.exe"), "refuse");
  assert.equal(verdict("ms-msdt:/id PCWDiagnostic"), "refuse");
  assert.equal(verdict("/relative/path"), "refuse");
  assert.equal(verdict(undefined), "refuse");
  assert.deepEqual(linkVerdict("zoommtg://zoom.us/join?confno=1"), {
    verdict: "ask",
    url: "zoommtg://zoom.us/join?confno=1",
    scheme: "zoommtg:",
  });
});

test("each kind of link goes where it should", () => {
  const went: string[] = [];
  setLinkHandlers({
    mailto: (url) => went.push(`compose ${url}`),
    external: (url) => went.push(`open ${url}`),
    ask: (url, scheme) => went.push(`ask ${scheme}`),
  });
  for (const href of ["mailto:bo@x.com", "https://x.com/", "slack://open", "data:text/html,x"]) openLink(href);
  assert.deepEqual(went, ["compose mailto:bo@x.com", "open https://x.com/", "ask slack:"]);
  setLinkHandlers(null);
});
