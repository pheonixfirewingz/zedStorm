// ZedStorm Chromium Extension - Web Search Bridge & Tab Manager

const DEFAULT_PORT = 42124;
const DEFAULT_ENGINE = "bing";
const DEFAULT_AUTO_CONNECT = false;

let socket = null;
let isConnecting = false;
let aiTabId = null; // Dedicated isolated tab for AI search/navigation in background
const approvedTabs = new Set(); // Per-session approved tab IDs for debugging/takeover

async function getSettings() {
  return new Promise((resolve) => {
    chrome.storage.local.get(["port", "engine", "autoConnect"], (items) => {
      resolve({
        port: items.port ? parseInt(items.port, 10) : DEFAULT_PORT,
        engine: items.engine || DEFAULT_ENGINE,
        autoConnect: items.autoConnect !== undefined ? items.autoConnect : DEFAULT_AUTO_CONNECT
      });
    });
  });
}

// Clean up approved/dedicated tabs when closed
chrome.tabs.onRemoved.addListener((tabId) => {
  if (tabId === aiTabId) {
    aiTabId = null;
  }
  approvedTabs.delete(tabId);
});

function disconnect() {
  if (socket) {
    try {
      socket.close();
    } catch (_) {}
    socket = null;
  }
  isConnecting = false;
  chrome.storage.local.set({ status: "disconnected" });
}

function connect() {
  if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
    return;
  }
  isConnecting = true;
  chrome.storage.local.set({ status: "connecting" });

  getSettings().then(({ port }) => {
    const url = `ws://127.0.0.1:${port}`;
    try {
      socket = new WebSocket(url);

      socket.onopen = () => {
        isConnecting = false;
        console.log(`[ZedStorm Bridge] Connected to ZedStorm on ${url}`);
        chrome.storage.local.set({ status: "connected" });
      };

      socket.onmessage = async (event) => {
        try {
          const msg = JSON.parse(event.data);
          if (msg.action === "search") {
            const settings = await getSettings();
            handleSearch(msg.id, msg.query, msg.max_results || 5, settings.engine);
          } else if (msg.action === "inspect_tab" || msg.action === "debug_tab" || msg.action === "takeover_tab") {
            handleTabTakeoverRequest(msg.id, msg.tab_id);
          }
        } catch (e) {
          console.error("[ZedStorm Bridge] Message handling error:", e);
        }
      };

      socket.onclose = () => {
        isConnecting = false;
        socket = null;
        chrome.storage.local.set({ status: "disconnected" });
      };

      socket.onerror = () => {
        isConnecting = false;
        socket = null;
        chrome.storage.local.set({ status: "disconnected" });
      };
    } catch (_) {
      isConnecting = false;
      socket = null;
      chrome.storage.local.set({ status: "disconnected" });
    }
  });
}

// Keep connection alive while OPEN
chrome.alarms.create("keepAlive", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(() => {
  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "ping" }));
  }
});

setInterval(() => {
  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "ping" }));
  }
}, 15000);

// Messages from popup
chrome.runtime.onMessage.addListener((request, sender, sendResponse) => {
  if (request.action === "connect") {
    connect();
    sendResponse({ status: "connecting" });
  } else if (request.action === "disconnect") {
    disconnect();
    sendResponse({ status: "disconnected" });
  } else if (request.action === "getStatus") {
    const status = socket && socket.readyState === WebSocket.OPEN
      ? "connected"
      : isConnecting ? "connecting" : "disconnected";
    sendResponse({ status, hasAiTab: aiTabId !== null });
  }
  return true;
});

// Dedicated AI tab: opened and kept strictly in the background (never steals user focus)
async function getOrCreateAiTab(url) {
  if (aiTabId !== null) {
    try {
      const tab = await chrome.tabs.get(aiTabId);
      if (tab) {
        // Reuse dedicated AI tab in background
        await chrome.tabs.update(aiTabId, { url, active: false });
        return aiTabId;
      }
    } catch (_) {
      aiTabId = null;
    }
  }

  // Create isolated background tab (active: false ensures user tab is never interrupted)
  const newTab = await chrome.tabs.create({
    url: url,
    active: false,
    pinned: false
  });
  aiTabId = newTab.id;
  return aiTabId;
}

// User tab takeover/debug request: brings up the tab and asks user with an alert on that tab
async function handleTabTakeoverRequest(id, requestedTabId) {
  try {
    let targetTab = null;

    if (requestedTabId) {
      try {
        targetTab = await chrome.tabs.get(requestedTabId);
      } catch (_) {}
    }

    if (!targetTab) {
      // Find the currently active user tab
      const [activeTab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
      targetTab = activeTab;
    }

    if (!targetTab) {
      throw new Error("No browser tab available to inspect.");
    }

    // If it's the AI's own tab, it can run in the background without prompting
    if (targetTab.id === aiTabId) {
      if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({
          id: id,
          approved: true,
          tab: { id: targetTab.id, url: targetTab.url, title: targetTab.title },
          error: null
        }));
      }
      return;
    }

    // If this tab was already approved by the user this session:
    if (approvedTabs.has(targetTab.id)) {
      if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({
          id: id,
          approved: true,
          tab: { id: targetTab.id, url: targetTab.url, title: targetTab.title },
          error: null
        }));
      }
      return;
    }

    // 1. Bring up the tab so the user sees it
    await chrome.tabs.update(targetTab.id, { active: true });
    if (targetTab.windowId) {
      await chrome.windows.update(targetTab.windowId, { focused: true });
    }

    // 2. Ask user for permission via an in-tab alert dialog on that exact tab
    const [execResult] = await chrome.scripting.executeScript({
      target: { tabId: targetTab.id },
      func: () => {
        const host = window.location.hostname || window.location.href;
        return window.confirm(
          `ZedStorm AI is requesting permission to inspect and debug this tab (${host}).\n\nAllow ZedStorm access?`
        );
      }
    });

    const userAllowed = execResult && execResult.result === true;

    if (userAllowed) {
      approvedTabs.add(targetTab.id);
      if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({
          id: id,
          approved: true,
          tab: { id: targetTab.id, url: targetTab.url, title: targetTab.title },
          error: null
        }));
      }
    } else {
      if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({
          id: id,
          approved: false,
          tab: null,
          error: "Permission denied by user on tab alert."
        }));
      }
    }
  } catch (err) {
    console.error("[ZedStorm Bridge] Tab takeover error:", err);
    if (socket && socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({
        id: id,
        approved: false,
        tab: null,
        error: err.message || "Failed to prompt tab for takeover"
      }));
    }
  }
}

// Search handler using background isolation (never disturbs the user)
async function handleSearch(id, query, maxResults, engine) {
  try {
    let results = [];
    if (engine === "duckduckgo") {
      try {
        results = await searchDuckDuckGo(query, maxResults);
      } catch (ddgErr) {
        results = await searchBing(query, maxResults);
      }
    } else {
      results = await searchBing(query, maxResults);
    }

    if (socket && socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({
        id: id,
        results: results,
        error: null
      }));
    }
  } catch (error) {
    if (socket && socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({
        id: id,
        results: [],
        error: error.message || "Search error"
      }));
    }
  }
}

async function searchBing(query, maxResults) {
  const url = `https://www.bing.com/search?q=${encodeURIComponent(query)}`;
  const resp = await fetch(url, {
    headers: {
      "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
      "User-Agent": navigator.userAgent
    }
  });
  if (!resp.ok) {
    throw new Error(`Bing HTTP error ${resp.status}`);
  }
  const html = await resp.text();
  return parseBingHtml(html, maxResults);
}

function parseBingHtml(html, maxResults) {
  const results = [];
  const algoRegex = /<li class="b_algo"[^>]*>([\s\S]*?)<\/li>/gi;
  let match;

  while ((match = algoRegex.exec(html)) !== null && results.length < maxResults) {
    const block = match[1];
    const titleMatch = /<h2[^>]*><a[^>]+href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/i.exec(block);
    if (!titleMatch) continue;

    let targetUrl = titleMatch[1];
    const uMatch = /[?&]u=a1([A-Za-z0-9_-]+)/.exec(targetUrl);
    if (uMatch) {
      try {
        let base64 = uMatch[1].replace(/-/g, "+").replace(/_/g, "/");
        while (base64.length % 4) base64 += "=";
        targetUrl = atob(base64);
      } catch (_) {}
    }

    const rawTitle = titleMatch[2].replace(/<[^>]+>/g, "").trim();
    const snippetMatch = /<p[^>]*>([\s\S]*?)<\/p>/i.exec(block);
    const snippet = snippetMatch ? snippetMatch[1].replace(/<[^>]+>/g, "").trim() : "";

    if (rawTitle && targetUrl) {
      results.push({
        title: decodeHtmlEntities(rawTitle),
        url: targetUrl,
        snippet: decodeHtmlEntities(snippet)
      });
    }
  }
  return results;
}

async function searchDuckDuckGo(query, maxResults) {
  const url = `https://html.duckduckgo.com/html/?q=${encodeURIComponent(query)}`;
  const resp = await fetch(url, {
    headers: {
      "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
      "User-Agent": navigator.userAgent
    }
  });
  if (!resp.ok) {
    throw new Error(`DuckDuckGo HTTP error ${resp.status}`);
  }
  const html = await resp.text();
  const results = [];
  const resultRegex = /<div class="result__body"[^>]*>([\s\S]*?)<\/div>/gi;
  let match;
  while ((match = resultRegex.exec(html)) !== null && results.length < maxResults) {
    const block = match[1];
    const linkMatch = /<a class="result__url"[^>]+href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/i.exec(block);
    const snippetMatch = /<a class="result__snippet"[^>]*>([\s\S]*?)<\/a>/i.exec(block);
    if (linkMatch) {
      results.push({
        title: decodeHtmlEntities(linkMatch[2].replace(/<[^>]+>/g, "").trim()),
        url: linkMatch[1],
        snippet: snippetMatch ? decodeHtmlEntities(snippetMatch[1].replace(/<[^>]+>/g, "").trim()) : ""
      });
    }
  }
  return results;
}

function decodeHtmlEntities(str) {
  return str
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#x27;/g, "'")
    .replace(/&nbsp;/g, " ");
}

// Only auto-connect if explicitly enabled in settings (default: false = silent, zero errors)
getSettings().then(({ autoConnect }) => {
  if (autoConnect) {
    connect();
  } else {
    chrome.storage.local.set({ status: "disconnected" });
  }
});
