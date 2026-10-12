(() => {
  const origin = "https://embed.diagrams.net";
  const token = "__BRIDGE_TOKEN__";
  const editor = document.getElementById("editor");
  const status = document.getElementById("status");
  const dialog = document.getElementById("close-dialog");
  let savedXml = "";
  let currentXml = "";
  let pendingSave = null;
  let initialized = false;
  let editorReady = false;
  const native = (message) => window.ipc.postMessage(JSON.stringify({ ...message, token }));
  const send = (message) => editor.contentWindow.postMessage(JSON.stringify(message), origin);
  const close = (modified = false) => {
    if (pendingSave) {
      status.textContent = "Wait for the diagram to finish saving before closing.";
    } else if (modified || currentXml !== savedXml) {
      if (!dialog.open) dialog.showModal();
    } else {
      native({ event: "exit" });
    }
  };
  document.getElementById("cancel-close").addEventListener("click", () => dialog.close());
  document.getElementById("discard-close").addEventListener("click", () => native({ event: "exit" }));
  window.addEventListener("message", (event) => {
    if (event.origin !== origin || event.source !== editor.contentWindow) return;
    let message;
    try { message = JSON.parse(event.data); } catch { return; }
    if (!message || typeof message !== "object") return;
    switch (message.event) {
      case "init":
        editorReady = true;
        native({ event: "init" });
        break;
      case "load":
        initialized = true;
        status.textContent = "Select Save to write changes to your .drawio file.";
        break;
      case "autosave":
        if (typeof message.xml === "string") {
          currentXml = message.xml;
          native({ event: "modified", modified: currentXml !== savedXml });
        }
        break;
      case "save":
        if (!initialized || pendingSave || typeof message.xml !== "string") return;
        pendingSave = { xml: message.xml, exit: message.exit === true };
        currentXml = message.xml;
        status.textContent = "Saving…";
        native({ event: "save", ...pendingSave });
        break;
      case "exit":
        close(message.modified === true);
        break;
    }
  });
  window.receiveNative = (message) => {
    switch (message.action) {
      case "load":
        savedXml = currentXml = message.xml;
        send(message);
        break;
      case "saved": {
        if (!pendingSave) return;
        savedXml = pendingSave.xml;
        const shouldExit = pendingSave.exit && currentXml === savedXml;
        pendingSave = null;
        status.textContent = "Saved.";
        send({ action: "status", message: "Saved", modified: currentXml !== savedXml });
        native({ event: "saved", modified: currentXml !== savedXml });
        if (shouldExit) native({ event: "exit" });
        break;
      }
      case "saveError":
        pendingSave = null;
        status.textContent = `Could not save: ${message.message}`;
        send({ action: "dialog", title: "Could not save diagram", message: message.message, button: "OK", modified: true });
        break;
      case "close": close(); break;
      case "requestSave": send({ action: "invokeAction", actionName: "save" }); break;
    }
  };
  setTimeout(() => {
    if (!editorReady) status.textContent = "diagrams.net has not loaded. Check your internet connection and reopen the diagram.";
  }, 20000);
})();
