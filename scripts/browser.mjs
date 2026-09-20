import { chromium } from "playwright";

/**
 * Launches the headless browser every end-to-end check drives. HIVE_CHROMIUM_PATH names an already
 * installed Chromium for a host whose Playwright browser cache does not hold the pinned revision.
 */
export function launchBrowser() {
  const executablePath = process.env.HIVE_CHROMIUM_PATH;
  return chromium.launch({ headless: true, ...(executablePath ? { executablePath } : {}) });
}
