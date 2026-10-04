/**
 * Hemp0x Commander Constants
 * Centralized configuration values to avoid magic numbers/strings.
 */

// Application version (single source of truth)
export const APP_VERSION = "v2.1.0";

// Asset creation fees (in HEMP)
export const ASSET_CREATION_FEE = "0.25";
export const SUB_ASSET_CREATION_FEE = "0.25";
export const NFT_CREATION_FEE = "0.25";

// Network defaults
export const DEFAULT_RPC_PORT = 42068;
export const DEFAULT_P2P_PORT = 42069;

// Explorer URL
export const EXPLORER_URL = "https://explorer.hemp0x.com";

// Polling intervals (ms)
export const DASHBOARD_POLL_INTERVAL = 5000;
export const DASHBOARD_POLL_INTERVAL_SLOW = 8000;

// Console limits
export const CONSOLE_MAX_LINES = 1000;
