import "dotenv/config";
import { Hono } from "hono";
import { serve } from "@hono/node-server";
import { serveStatic } from "@hono/node-server/serve-static";
import { cors } from "hono/cors";
import { logger } from "hono/logger";
import { auth } from "./auth/index.js";
import { registerProjectRoutes } from "./routes/projects.js";
import { registerTaskRoutes } from "./routes/tasks.js";
import { registerRequirementRoutes } from "./routes/requirements.js";
import { registerSyncRoutes } from "./routes/sync.js";
import { registerMetricsRoutes } from "./routes/metrics.js";
import fs from "fs";
import path from "path";

const app = new Hono();

app.use("/*", logger());
app.use(
  "/*",
  cors({
    origin: process.env.CORS_ORIGINS?.split(",") || [
      "http://localhost:5175",
      "http://localhost:3100",
    ],
    credentials: true,
  })
);

app.all("/api/auth/*", (c) => {
  return auth.handler(c.req.raw);
});

registerProjectRoutes(app);
registerTaskRoutes(app);
registerRequirementRoutes(app);
registerSyncRoutes(app);
registerMetricsRoutes(app);

app.get("/health", (c) => c.json({ ok: true }));

app.use("/assets/*", serveStatic({ root: "./web/dist" }));

app.get("*", (c) => {
  const reqPath = new URL(c.req.url).pathname;
  if (reqPath.startsWith("/api/")) {
    return c.json({ error: "Not found" }, 404);
  }
  const indexPath = path.resolve("./web/dist/index.html");
  try {
    const html = fs.readFileSync(indexPath, "utf-8");
    return c.html(html);
  } catch {
    return c.text("Not found", 404);
  }
});

const port = parseInt(process.env.PORT || "3100");

console.log(`ao-sync listening on http://localhost:${port}`);

serve({ fetch: app.fetch, port, hostname: "0.0.0.0" });
