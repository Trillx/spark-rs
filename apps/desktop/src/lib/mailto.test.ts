import assert from "node:assert/strict";
import { test } from "node:test";
import { parseMailto } from "./mailto.ts";

test("a mailto link becomes the email it asks for", () => {
  assert.deepEqual(
    parseMailto("mailto:ana@x.com,bo+news@y.org?cc=cy@z.net&subject=Lunch%20on%20Friday%3F&body=Hi%20Ana%2C%0D%0A%0D%0ASee%20you&attach=C%3A%5Cx"),
    { to: "ana@x.com, bo+news@y.org", cc: "cy@z.net", bcc: "", subject: "Lunch on Friday?", body: "Hi Ana,\n\nSee you" },
  );
});

test("addresses can come from the query too, and odd escapes survive", () => {
  assert.deepEqual(parseMailto("MAILTO:?to=a%40x.com&To=b@x.com&BCC=c@x.com&subject=100%&body"), {
    to: "a@x.com, b@x.com",
    cc: "",
    bcc: "c@x.com",
    subject: "100%",
    body: "",
  });
  assert.equal(parseMailto("https://x.com"), null);
});
