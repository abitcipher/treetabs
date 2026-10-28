import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.',
  timeout: 60_000,
  workers: 1,
  // Two canvas-click tests fail now and then under load: one retry on CI.
  retries: process.env.CI ? 1 : 0,
  reporter: [['list']],
  globalSetup: './global-setup.js',
});
