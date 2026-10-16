document.addEventListener("DOMContentLoaded", () => {
  const portInput = document.getElementById("port");
  const engineSelect = document.getElementById("engine");
  const autoConnectCheck = document.getElementById("autoConnect");
  const toggleBtn = document.getElementById("toggleBtn");
  const statusDot = document.getElementById("statusDot");
  const statusText = document.getElementById("statusText");

  let currentStatus = "disconnected";

  function setStatus(status) {
    currentStatus = status;
    if (status === "connected") {
      statusDot.className = "status-dot connected";
      statusText.textContent = "Connected to ZedStorm";
      toggleBtn.textContent = "Disconnect";
      toggleBtn.className = "disconnect-btn";
    } else if (status === "connecting") {
      statusDot.className = "status-dot connecting";
      statusText.textContent = "Connecting to ZedStorm...";
      toggleBtn.textContent = "Connecting...";
      toggleBtn.className = "";
    } else {
      statusDot.className = "status-dot";
      statusText.textContent = "Disconnected (ZedStorm closed)";
      toggleBtn.textContent = "Connect to ZedStorm";
      toggleBtn.className = "";
    }
  }

  // Load current settings
  chrome.storage.local.get(["port", "engine", "autoConnect", "status"], (items) => {
    if (items.port) portInput.value = items.port;
    if (items.engine) engineSelect.value = items.engine;
    if (items.autoConnect !== undefined) autoConnectCheck.checked = items.autoConnect;
    setStatus(items.status || "disconnected");
  });

  // Query background for live status
  chrome.runtime.sendMessage({ action: "getStatus" }, (response) => {
    if (response && response.status) {
      setStatus(response.status);
    }
  });

  // Listen for storage changes from background
  chrome.storage.onChanged.addListener((changes) => {
    if (changes.status) {
      setStatus(changes.status.newValue);
    }
  });

  // Save settings on change
  function saveSettings() {
    const port = parseInt(portInput.value, 10) || 42124;
    const engine = engineSelect.value || "bing";
    const autoConnect = autoConnectCheck.checked;
    chrome.storage.local.set({ port, engine, autoConnect });
  }

  portInput.addEventListener("change", saveSettings);
  engineSelect.addEventListener("change", saveSettings);
  autoConnectCheck.addEventListener("change", saveSettings);

  // Toggle button click
  toggleBtn.addEventListener("click", () => {
    saveSettings();
    if (currentStatus === "connected") {
      chrome.runtime.sendMessage({ action: "disconnect" }, (resp) => {
        setStatus(resp?.status || "disconnected");
      });
    } else {
      setStatus("connecting");
      chrome.runtime.sendMessage({ action: "connect" }, (resp) => {
        setStatus(resp?.status || "connecting");
      });
    }
  });
});
