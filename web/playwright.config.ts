import { defineConfig, devices } from '@playwright/test';

// The smoke test runs the built page (web/out) against tests/mock-server.mjs,
// a stand-in for `tenx web` — no tmux, no Rust. Build first: `pnpm build`.
export default defineConfig({
  testDir: 'tests',
  timeout: 30_000,
  // One retry in CI: the touch-gesture tests replay synthetic pointer events
  // and are timing-sensitive on a busy runner. Locally a failure shows at once.
  retries: process.env.CI ? 1 : 0,
  use: { baseURL: 'http://127.0.0.1:7071' },
  webServer: {
    command: 'node tests/mock-server.mjs 7071',
    url: 'http://127.0.0.1:7071/',
    reuseExistingServer: !process.env.CI,
  },
  projects: [
    { name: 'desktop', use: { ...devices['Desktop Chrome'], viewport: { width: 1440, height: 900 } } },
    { name: 'phone', use: { ...devices['Pixel 7'] } },
  ],
});
