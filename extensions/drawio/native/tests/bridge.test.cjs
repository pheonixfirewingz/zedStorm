const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

function session() {
  const calls = [];
  const messages = [];
  const elements = new Map();
  const handlers = new Map();
  for (const name of ["editor", "status", "close-dialog", "cancel-close", "discard-close"]) {
    elements.set(name, {
      textContent: "", open: false,
      contentWindow: { postMessage: (value) => messages.push(JSON.parse(value)) },
      addEventListener: (event, handler) => handlers.set(`${name}:${event}`, handler),
      showModal() { this.open = true; },
      close() { this.open = false; },
    });
  }
  const window = {
    ipc: { postMessage: (value) => calls.push(JSON.parse(value)) },
    addEventListener: (event, handler) => handlers.set(event, handler),
  };
  const context = vm.createContext({ window, document: { getElementById: (id) => elements.get(id) }, setTimeout: () => {} });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/bridge.js"), "utf8"), context);
  const receive = (message, overrides = {}) => handlers.get("message")({
    origin: "https://embed.diagrams.net", source: elements.get("editor").contentWindow,
    data: JSON.stringify(message), ...overrides,
  });
  return { calls, messages, elements, receive, native: window.receiveNative };
}

test("rejects messages from other origins and frames", () => {
  const editor = session();
  editor.receive({ event: "init" }, { origin: "https://example.com" });
  editor.receive({ event: "init" }, { source: {} });
  editor.receive({ event: "init" }, { data: "invalid" });
  assert.equal(editor.calls.length, 0);
});

test("waits for successful persistence before Save and Exit", () => {
  const editor = session();
  editor.native({ action: "load", xml: "original" });
  editor.receive({ event: "load" });
  editor.receive({ event: "save", xml: "updated", exit: true });
  assert.equal(editor.calls.at(-1).event, "save");
  editor.native({ action: "saveError", message: "Disk is full" });
  assert.match(editor.elements.get("status").textContent, /Disk is full/);
  assert.equal(editor.calls.some((call) => call.event === "exit"), false);
  editor.native({ action: "close" });
  assert.equal(editor.elements.get("close-dialog").open, true);
  editor.receive({ event: "save", xml: "updated", exit: true });
  editor.native({ action: "saved", exit: true });
  assert.equal(editor.calls.at(-1).event, "exit");
});

test("keeps edits made during saving and blocks closing while saving", () => {
  const editor = session();
  editor.native({ action: "load", xml: "original" });
  editor.receive({ event: "load" });
  editor.receive({ event: "save", xml: "first edit", exit: true });
  editor.native({ action: "close" });
  assert.equal(editor.calls.some((call) => call.event === "exit"), false);
  editor.receive({ event: "autosave", xml: "later edit" });
  editor.native({ action: "saved", exit: true });
  assert.equal(editor.calls.some((call) => call.event === "exit"), false);
  assert.equal(editor.messages.at(-1).modified, true);
  assert.equal(editor.calls.findLast((call) => call.event === "saved").modified, true);
});
