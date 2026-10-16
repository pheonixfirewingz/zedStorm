# ZedStorm Web Search Extension (Chromium)

Optional browser extension to provide live web search capabilities to ZedStorm's native Context MCP server.

## Compatibility
Works with any Chromium-based browser:
- Google Chrome
- Brave
- Microsoft Edge
- Chromium
- Vivaldi
- Arc
- Opera

## Key Features & Safety

### 1. Tab Isolation (Your Tabs are Never Touched)
The extension manages an isolated background tab (`active: false`) for AI search requests. It will **never** navigate away from, close, or modify the tab you are currently browsing.

### 2. Tab Takeover Permission Gate
If ZedStorm requests permission to inspect or debug an active user tab (e.g. for web debugging), the request is blocked by default unless you explicitly check **"Allow AI to inspect active tab for debugging"** in the extension popup.

### 3. Silent Mode (Zero Startup Errors)
By default, the extension starts in silent mode and only connects when you click **"Connect to ZedStorm"** in the popup (or if you enable "Auto-connect when browser opens"). It will never spam failed connection attempts or trigger red error badges in `chrome://extensions` when ZedStorm is closed.

## Installation
1. Open your browser's extension management page:
   - Chrome / Chromium: `chrome://extensions`
   - Brave: `brave://extensions`
   - Edge: `edge://extensions`
2. Enable **Developer mode** (toggle in the top-right corner).
3. Click **Load unpacked**.
4. Select this folder (`assets/browser-extension`).
5. Click the extension icon in your toolbar and click **"Connect to ZedStorm"** whenever you want to enable web search in your editor.
