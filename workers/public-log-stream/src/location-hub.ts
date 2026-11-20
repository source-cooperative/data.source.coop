import { DurableObject } from "cloudflare:workers";
import {
  SPREAD_SLOTS,
  addBatchSpread,
  buildLocs,
  dueSlots,
  hasPending,
  pruneSlots,
  replayWindow,
  totalRequests,
} from "./aggregate.mjs";

export interface Env {
  LOCATION_HUB: DurableObjectNamespace<LocationHub>;
  EMIT_INTERVAL_MS: string;
  MAX_PRODUCTS_PER_COLO: string;
  CORS_ORIGIN: string;
}

// Posted by the proxy: one batch of public-product GETs seen by one isolate,
// i.e. one datacenter. `products` maps "account/product" to its request count.
// The bare `account_id`/`product_id` form is one request from an older proxy.
// Only the datacenter and the products are used; requester geolocation is
// intentionally never forwarded to viewers.
export interface LocationEvent {
  colo?: string;
  products?: Record<string, number>;
  account_id?: string;
  product_id?: string;
}

// One datacenter's activity within one tick.
interface ColoStats {
  n: number;
  products: Map<string, number>;
}

export class LocationHub extends DurableObject<Env> {
  // Activity queued per tick, keyed by absolute tick index (see tickOf). A
  // batch is spread over the next SPREAD_SLOTS ticks so viewers get a steady
  // trickle instead of one burst per proxy flush. Lost on eviction, which only
  // forfeits the last few seconds of a decorative display.
  private slots = new Map<number, Map<string, ColoStats>>();
  private cursor = 0;
  private lastEmitted: number;
  private sentIdle = false;
  private emitInterval: number;
  private maxProducts: number;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.emitInterval = parseInt(env.EMIT_INTERVAL_MS) || 500;
    this.maxProducts = parseInt(env.MAX_PRODUCTS_PER_COLO) || 5;
    this.lastEmitted = this.tickOf(Date.now());
  }

  private tickOf(ms: number): number {
    return Math.floor(ms / this.emitInterval);
  }

  private nextBoundary(ms: number): number {
    return (this.tickOf(ms) + 1) * this.emitInterval;
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

  private async handleWebSocket(request: Request): Promise<Response> {
    const upgradeHeader = request.headers.get("Upgrade");
    if (upgradeHeader !== "websocket") {
      return new Response("Expected WebSocket upgrade", { status: 426 });
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);

    this.ctx.acceptWebSocket(server);
    this.replay(server);
    await this.ensureAlarm();

    return new Response(null, { status: 101, webSocket: client });
  }

  // Show a new viewer the last few seconds at once, so the globe is not empty
  // until the next live tick. Only ticks already emitted, so it never overlaps
  // what the viewer receives live. Same shape as a tick plus `replay`, so
  // clients that read only ticks need no change. Does not touch `sentIdle`.
  private replay(server: WebSocket): void {
    const recent = replayWindow(this.slots, this.lastEmitted, SPREAD_SLOTS);
    if (recent.size === 0) return;
    try {
      server.send(
        JSON.stringify({
          type: "tick",
          replay: true,
          windowMs: SPREAD_SLOTS * this.emitInterval,
          requestsPerWindow: totalRequests(recent),
          viewers: this.ctx.getWebSockets().length,
          locations: buildLocs(recent, this.maxProducts),
        }),
      );
    } catch {
      // The socket closed already; cleaned up in webSocketClose
    }
  }

  private async handleLocation(request: Request): Promise<Response> {
    const event: LocationEvent = await request.json();

    const { total, cursor } = addBatchSpread(
      this.slots,
      event,
      this.tickOf(Date.now()),
      SPREAD_SLOTS,
      this.cursor,
    );
    this.cursor = cursor;
    if (total === 0) return new Response("ok");

    this.sentIdle = false;
    await this.ensureAlarm();
    return new Response("ok");
  }

  // Storage is the source of truth for a pending alarm; a cached flag can drift.
  // An alarm long past due is not going to fire, so it is replaced, not trusted.
  // Alarms are aligned to tick boundaries so slots are emitted on their tick.
  private async ensureAlarm(): Promise<void> {
    const now = Date.now();
    const at = await this.ctx.storage.getAlarm();
    if (at == null || at < now - 10 * this.emitInterval) {
      await this.ctx.storage.setAlarm(this.nextBoundary(now));
    }
  }

  async alarm(): Promise<void> {
    const now = Date.now();
    const tick = this.tickOf(now);
    const clients = this.ctx.getWebSockets();

    // Everything now due, merged, so an alarm that fires late catches up on
    // the ticks it missed instead of emitting them one by one.
    const due = dueSlots(this.slots, this.lastEmitted, tick);
    this.lastEmitted = Math.max(this.lastEmitted, tick);
    pruneSlots(this.slots, tick - SPREAD_SLOTS);
    const pending = hasPending(this.slots, tick);

    if (clients.length > 0) {
      if (due.size > 0) {
        this.send(clients, due);
        this.sentIdle = false;
      } else if (!pending && !this.sentIdle) {
        // Going idle: one empty tick so the globe can fade out, then quiet
        // until activity resumes. Not while later ticks still hold activity.
        this.send(clients, due);
        this.sentIdle = true;
      }
    }

    // Keep ticking while someone watches or activity is still queued; with
    // neither, let the alarm stop (re-armed on the next connect or post).
    if (clients.length > 0 || pending) {
      await this.ctx.storage.setAlarm(this.nextBoundary(now));
    }
  }

  private send(clients: WebSocket[], colos: Map<string, ColoStats>): void {
    const message = JSON.stringify({
      type: "tick",
      requestsPerWindow: totalRequests(colos),
      viewers: clients.length,
      locations: buildLocs(colos, this.maxProducts),
    });

    for (const ws of clients) {
      try {
        ws.send(message);
      } catch {
        // Cleaned up in webSocketClose
      }
    }
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
