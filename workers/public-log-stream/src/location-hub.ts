import { DurableObject } from "cloudflare:workers";
import { buildLocs } from "./aggregate.mjs";

export interface Env {
  LOCATION_HUB: DurableObjectNamespace<LocationHub>;
  EMIT_INTERVAL_MS: string;
  MAX_PRODUCTS_PER_COLO: string;
  CORS_ORIGIN: string;
}

// Posted by the proxy per successful public-product GET. Only the datacenter
// (colo) and the product are used; requester geolocation is intentionally
// never forwarded to viewers.
export interface LocationEvent {
  colo?: string;
  account_id?: string;
  product_id?: string;
}

// One datacenter's activity within the current window.
interface ColoStats {
  n: number;
  products: Map<string, number>;
}

export class LocationHub extends DurableObject<Env> {
  private colos = new Map<string, ColoStats>();
  private requestCount = 0;
  private alarmScheduled = false;
  private sentIdle = false;
  private emitInterval: number;
  private maxProducts: number;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.emitInterval = parseInt(env.EMIT_INTERVAL_MS) || 500;
    this.maxProducts = parseInt(env.MAX_PRODUCTS_PER_COLO) || 5;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === "/ws") {
      return this.handleWebSocket(request);
    }

    if (url.pathname === "/location" && request.method === "POST") {
      return this.handleLocation(request);
    }

    return new Response("Not found", { status: 404 });
  }

  private handleWebSocket(request: Request): Response {
    const upgradeHeader = request.headers.get("Upgrade");
    if (upgradeHeader !== "websocket") {
      return new Response("Expected WebSocket upgrade", { status: 426 });
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);

    this.ctx.acceptWebSocket(server);
    this.ensureAlarm();

    return new Response(null, { status: 101, webSocket: client });
  }

  private async handleLocation(request: Request): Promise<Response> {
    const event: LocationEvent = await request.json();

    // No datacenter → can't place it on the globe; drop it.
    if (!event.colo) {
      return new Response("ok");
    }

    const colo = this.colos.get(event.colo) ?? { n: 0, products: new Map() };
    colo.n++;
    const product = `${event.account_id ?? ""}/${event.product_id ?? ""}`;
    colo.products.set(product, (colo.products.get(product) ?? 0) + 1);
    this.colos.set(event.colo, colo);

    this.requestCount++;
    this.sentIdle = false;
    this.ensureAlarm();
    return new Response("ok");
  }

  private async ensureAlarm(): Promise<void> {
    if (!this.alarmScheduled) {
      const currentAlarm = await this.ctx.storage.getAlarm();
      if (currentAlarm == null) {
        await this.ctx.storage.setAlarm(Date.now() + this.emitInterval);
      }
      this.alarmScheduled = true;
    }
  }

  async alarm(): Promise<void> {
    this.alarmScheduled = false;
    const clients = this.ctx.getWebSockets();

    // Nobody watching → let the alarm stop (re-armed on next connect/POST).
    if (clients.length === 0) {
      this.colos.clear();
      this.requestCount = 0;
      return;
    }

    const hasActivity = this.colos.size > 0;

    // Emit while there's activity, plus one final tick when going idle so the
    // globe can fade out, then stay quiet until activity resumes.
    if (hasActivity || !this.sentIdle) {
      const message = JSON.stringify({
        type: "tick",
        requestsPerWindow: this.requestCount,
        viewers: clients.length,
        locations: buildLocs(this.colos, this.maxProducts),
      });

      for (const ws of clients) {
        try {
          ws.send(message);
        } catch {
          // Cleaned up in webSocketClose
        }
      }

      this.sentIdle = !hasActivity;
    }

    // Reset for the next window and keep the alarm running while clients remain.
    this.colos.clear();
    this.requestCount = 0;
    await this.ctx.storage.setAlarm(Date.now() + this.emitInterval);
    this.alarmScheduled = true;
  }

  async webSocketMessage(
    _ws: WebSocket,
    _message: string | ArrayBuffer,
  ): Promise<void> {
    // Clients don't send messages; ignore
  }

  async webSocketClose(
    ws: WebSocket,
    code: number,
    reason: string,
  ): Promise<void> {
    ws.close(code, reason);
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    ws.close(1011, "WebSocket error");
  }
}
