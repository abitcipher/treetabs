import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.',
  timeout: 60_000,
  workers: 1,
  reporter: [['list']],
  globalSetup: './global-setup.js',
});
