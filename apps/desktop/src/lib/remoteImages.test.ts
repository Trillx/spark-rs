import assert from "node:assert/strict";
import { test } from "node:test";

const saved = new Map<string, string>();
Object.assign(globalThis, {
  localStorage: {
    getItem: (key: string) => saved.get(key) ?? null,
    setItem: (key: string, value: string) => void saved.set(key, value),
  },
});
const { ALLOWED_MAX, allowRemoteImages, loadRemoteImages, remoteImagesAllowed, saveLoadRemoteImages } = await import(
  "./remoteImages.ts"
);

const email = (id: number, timestamp = 1_000) => ({ id, timestamp });

test("remote images load unless the setting is off, as Spark ships it", () => {
  assert.equal(loadRemoteImages(), true);
  assert.equal(remoteImagesAllowed(email(1)), true);
  saveLoadRemoteImages(false);
  assert.equal(loadRemoteImages(), false);
  assert.equal(remoteImagesAllowed(email(1)), false);
  saveLoadRemoteImages(true);
  assert.equal(remoteImagesAllowed(email(1)), true);
});

test("Show images is kept for that email, and not for a later one given its id", () => {
  saveLoadRemoteImages(false);
  allowRemoteImages(email(7));
  assert.equal(remoteImagesAllowed(email(7)), true);
  assert.equal(remoteImagesAllowed(email(8)), false);
  // The next daily digest, same sender and subject, stored under the deleted one's id.
  assert.equal(remoteImagesAllowed(email(7, 87_400_000)), false);
  assert.deepEqual(JSON.parse(saved.get("spark-rs.remoteImagesAllowed")!), ["7:1000"]);
});

test("only the latest emails are remembered", () => {
  saveLoadRemoteImages(false);
  for (let id = 0; id <= ALLOWED_MAX; id++) allowRemoteImages(email(10_000 + id));
  assert.equal(remoteImagesAllowed(email(10_000)), false);
  assert.equal(remoteImagesAllowed(email(10_001)), true);
  // Showing one again moves it to the end, so it outlasts the others.
  allowRemoteImages(email(10_001));
  allowRemoteImages(email(20_000));
  assert.equal(remoteImagesAllowed(email(10_001)), true);
  assert.equal(remoteImagesAllowed(email(10_002)), false);
});

test("a damaged list counts as empty", () => {
  saveLoadRemoteImages(false);
  saved.set("spark-rs.remoteImagesAllowed", "{oops");
  allowRemoteImages(email(3));
  assert.equal(remoteImagesAllowed(email(3)), true);
  assert.deepEqual(JSON.parse(saved.get("spark-rs.remoteImagesAllowed")!), ["3:1000"]);
});
