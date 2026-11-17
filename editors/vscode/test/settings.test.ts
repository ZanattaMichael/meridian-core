import assert from "node:assert/strict";
import { test } from "node:test";
import {
  expand,
  initializationOptions,
  serverCommand,
  startFailure,
} from "../src/settings";

const defaults = { path: "meridian", pluginDirs: [], lintEnabled: true, sortFields: true };

test("the default command is `meridian lsp`", () => {
  assert.deepEqual(serverCommand(defaults, "/work", "/home/me"), {
    command: "meridian",
    args: ["lsp"],
  });
});

test("an empty path falls back to the default", () => {
  assert.equal(serverCommand({ ...defaults, path: "  " }, undefined, undefined).command, "meridian");
});

test("the workspace folder and the home directory are expanded", () => {
  assert.equal(expand("${workspaceFolder}/target/debug/meridian", "/work", "/home/me"), "/work/target/debug/meridian");
  assert.equal(expand("~/bin/meridian", "/work", "/home/me"), "/home/me/bin/meridian");
  assert.equal(expand("~user/bin", "/work", "/home/me"), "~user/bin");
  assert.equal(expand("${workspaceFolder}/x", undefined, "/home/me"), "${workspaceFolder}/x");
});

test("the options match what the server reads", () => {
  const options = initializationOptions(
    { path: "meridian", pluginDirs: ["${workspaceFolder}/plugins", " ", "~/p"], lintEnabled: false, sortFields: false },
    "/work",
    "/home/me",
  );
  assert.deepEqual(options, {
    pluginDirs: ["/work/plugins", "/home/me/p"],
    lint: false,
    sortFields: false,
  });
});

test("a missing command is explained", () => {
  const error = Object.assign(new Error("spawn meridian ENOENT"), { code: "ENOENT" });
  assert.match(startFailure("meridian", error), /could not find the `meridian` command/);
  assert.match(startFailure("meridian", new Error("boom")), /boom/);
});
