import { DurableObject } from "cloudflare:workers";

export interface Env {
  LOCATION_HUB: DurableObjectNamespace<LocationHub>;
  MAX_BROADCASTS_PER_SECOND: string;
  CORS_ORIGIN: string;
}

export interface LocationEvent {
  lat: number;
  lon: number;
  city?: string;
  country?: string;
  colo?: string;
}

interface Stats {
  requestCount: number;
  broadcastCount: number;
  windowStart: number;
}

export class LocationHub extends DurableObject<Env> {
  private stats: Stats = {
    requestCount: 0,
    broadcastCount: 0,
    windowStart: Date.now(),
  };
  private alarmScheduled = false;
  private maxBroadcasts: number;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.maxBroadcasts = parseInt(env.MAX_BROADCASTS_PER_SECOND) || 50;
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
    const location: LocationEvent = await request.json();

    const now = Date.now();
    if (now - this.stats.windowStart >= 1000) {
      this.stats = { requestCount: 0, broadcastCount: 0, windowStart: now };
    }

    this.stats.requestCount++;

    // Sample: only broadcast if under the ceiling
    if (this.stats.broadcastCount < this.maxBroadcasts) {
      this.stats.broadcastCount++;
      const message = JSON.stringify({ type: "location", data: location });
      for (const ws of this.ctx.getWebSockets()) {
        try {
          ws.send(message);
        } catch {
          // Client disconnected, cleaned up in webSocketClose
        }
      }
    }

    this.ensureAlarm();
    return new Response("ok");
  }

  private async ensureAlarm(): Promise<void> {
    if (!this.alarmScheduled) {
      const currentAlarm = await this.ctx.storage.getAlarm();
      if (currentAlarm == null) {
        await this.ctx.storage.setAlarm(Date.now() + 1000);
      }
      this.alarmScheduled = true;
    }
  }

  async alarm(): Promise<void> {
    this.alarmScheduled = false;
    const clients = this.ctx.getWebSockets();

    if (clients.length > 0) {
      const statsMessage = JSON.stringify({
        type: "stats",
        data: {
          requestsPerSecond: this.stats.requestCount,
          broadcastsPerSecond: this.stats.broadcastCount,
          viewers: clients.length,
        },
      });

      for (const ws of clients) {
        try {
          ws.send(statsMessage);
        } catch {
          // Cleaned up in webSocketClose
        }
      }

      // Reset for next window
      this.stats = {
        requestCount: 0,
        broadcastCount: 0,
        windowStart: Date.now(),
      };

      // Keep alarm running while clients are connected
      await this.ctx.storage.setAlarm(Date.now() + 1000);
      this.alarmScheduled = true;
    }
  }

  async webSocketMessage(
    _ws: WebSocket,
    _message: string | ArrayBuffer
  ): Promise<void> {
    // Clients don't send messages; ignore
  }

  async webSocketClose(
    ws: WebSocket,
    code: number,
    reason: string
  ): Promise<void> {
    ws.close(code, reason);
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    ws.close(1011, "WebSocket error");
  }
}
