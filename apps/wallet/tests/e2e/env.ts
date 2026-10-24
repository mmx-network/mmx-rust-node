/** Ports of the end-to-end setup (override with E2E_RPC_PORT / E2E_APP_PORT); below the usual ephemeral port range. */
export const RPC_PORT = Number(process.env.E2E_RPC_PORT ?? 31700);
export const APP_PORT = Number(process.env.E2E_APP_PORT ?? 31773);
export const RPC_URL = `http://127.0.0.1:${RPC_PORT}`;
export const APP_URL = `http://127.0.0.1:${APP_PORT}/`;
