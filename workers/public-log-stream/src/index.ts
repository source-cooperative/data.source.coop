import { LocationHub } from "./location-hub";
import type { Env } from "./location-hub";

export { LocationHub };

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const corsHeaders = getCorsHeaders(env.CORS_ORIGIN, request);

    // Handle CORS preflight
    if (request.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: corsHeaders });
    }

    // Route to the single global LocationHub instance
    const id = env.LOCATION_HUB.idFromName("global");
    const stub = env.LOCATION_HUB.get(id);

    if (url.pathname === "/ws") {
      return stub.fetch(request);
    }

    if (url.pathname === "/location" && request.method === "POST") {
      const response = await stub.fetch(request);
      return new Response(response.body, {
        status: response.status,
        headers: corsHeaders,
      });
    }

    if (url.pathname === "/health") {
      return new Response("ok", { headers: corsHeaders });
    }

    return new Response("Not found", { status: 404, headers: corsHeaders });
  },
};

function getCorsHeaders(
  origin: string,
  request: Request
): Record<string, string> {
  return {
    "Access-Control-Allow-Origin": origin,
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    "Access-Control-Allow-Headers":
      request.headers.get("Access-Control-Request-Headers") ??
      "Content-Type, Authorization",
  };
}
