import test from "node:test";
import assert from "node:assert/strict";
import { usernamePattern, validUsername } from "../res/static/src/username.js";

const accepted = [
    "Alice_1",
    "Alice.Smith-family+tag@example.org",
    "Alice..Smith",
    "Alice.",
    "-",
    "@",
    "+",
    "a".repeat(64),
];
const rejected = [
    "",
    ".",
    "..",
    ".hidden",
    "...",
    "../escape",
    "a/b",
    "a\\b",
    "a b",
    "a\n",
    "a\r",
    "a\0",
    "%2e%2e",
    "a?b",
    "a#b",
    "ä",
    "a".repeat(65),
];

test("username availability validation accepts punctuation and rejects unsafe names", () => {
    for (const name of accepted) assert.equal(validUsername(name), true, name);
    for (const name of rejected) assert.equal(validUsername(name), false, name);
});

test("login and registration HTML pattern accepts the same names with Unicode sets", () => {
    const pattern = new RegExp(`^(?:${usernamePattern})$`, "v");
    for (const name of accepted) assert.equal(pattern.exec(name)?.[0], name);
    for (const name of rejected) assert.notEqual(pattern.exec(name)?.[0], name);
});
