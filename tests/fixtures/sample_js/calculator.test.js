import { test } from "node:test";
import assert from "node:assert";
import { add, multiply } from "./calculator.js";

test("add returns sum", () => {
  assert.strictEqual(add(2, 3), 5);
});

test("multiply returns product", () => {
  assert.strictEqual(multiply(3, 4), 12);
});
