#!/usr/bin/env node

import net from "node:net";
import path from "node:path";
import process from "node:process";
import { spawn } from "node:child_process";
import { mkdir, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const HOST = "127.0.0.1";
const READY_TIMEOUT_MS = parsePositiveInt(process.env.SMOKE_READY_TIMEOUT_MS, 5 * 60_000);
const POLL_INTERVAL_MS = 500;
const SHUTDOWN_TIMEOUT_MS = 10_000;

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const WEB_UI_DIR = path.resolve(SCRIPT_DIR, "..");
const REPO_ROOT = path.resolve(WEB_UI_DIR, "../../..");
const ARTIFACT_DIR = process.env.SMOKE_ARTIFACT_DIR
  ? path.resolve(process.env.SMOKE_ARTIFACT_DIR)
  : path.join(WEB_UI_DIR, ".smoke-artifacts");

const REPORT_PATH = path.join(ARTIFACT_DIR, "smoke-assertions.txt");
const reportLines = [];
const activeServers = new Set();

function parsePositiveInt(value, fallback) {
  if (!value) {
    return fallback;
  }

  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    return fallback;
  }
  return parsed;
}

function record(status, label, detail = "") {
  const line = detail ? `[${status}] ${label}: ${detail}` : `[${status}] ${label}`;
  reportLines.push(line);
  if (status === "PASS") {
    console.log(line);
    return;
  }
  console.error(line);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function tail(text, maxChars = 1_200) {
  if (!text) {
    return "";
  }
  return text.length > maxChars ? text.slice(-maxChars) : text;
}

function toErrorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

function isWithin(parent, candidate) {
  const relative = path.relative(parent, candidate);
  return relative !== "" && !relative.startsWith("..") && !path.isAbsolute(relative);
}

async function pickOpenPort(host) {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, host, () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("failed to allocate ephemeral port"));
        return;
      }
      const { port } = address;
      server.close((closeError) => {
        if (closeError) {
          reject(closeError);
          return;
        }
        resolve(port);
      });
    });
  });
}

function createServer({ name, port, apiOnly }) {
  const stdoutPath = path.join(ARTIFACT_DIR, `${name}.stdout.log`);
  const stderrPath = path.join(ARTIFACT_DIR, `${name}.stderr.log`);

  const args = [
    "run",
    "--locked",
    "-p",
    "orchestrator-cli",
    "--",
    "--project-root",
    REPO_ROOT,
    "web",
    "serve",
    "--host",
    HOST,
    "--port",
    String(port),
  ];

  if (apiOnly) {
    args.push("--api-only");
  }

  const child = spawn("cargo", args, {
    cwd: REPO_ROOT,
    env: process.env,
    stdio: ["ignore", "pipe", "pipe"],
  });

  const server = {
    name,
    port,
    apiOnly,
    child,
    stdoutPath,
    stderrPath,
    stdout: "",
    stderr: "",
    spawnError: null,
    stopped: false,
  };

  child.stdout?.on("data", (chunk) => {
    server.stdout += chunk.toString();
  });
  child.stderr?.on("data", (chunk) => {
    server.stderr += chunk.toString();
  });
  child.on("error", (error) => {
    server.spawnError = error;
  });

  activeServers.add(server);
  return server;
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve(true);
  }

  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      cleanup();
      resolve(false);
    }, timeoutMs);

    const onExit = () => {
      cleanup();
      resolve(true);
    };

    const cleanup = () => {
      clearTimeout(timer);
      child.off("exit", onExit);
    };

    child.once("exit", onExit);
  });
}

async function stopServer(server) {
  if (!server || server.stopped) {
    return;
  }

  server.stopped = true;
  activeServers.delete(server);

  if (server.child.exitCode === null && server.child.signalCode === null) {
    server.child.kill("SIGTERM");
    const exited = await waitForExit(server.child, SHUTDOWN_TIMEOUT_MS);
    if (!exited) {
      server.child.kill("SIGKILL");
      await waitForExit(server.child, SHUTDOWN_TIMEOUT_MS);
    }
  }

  await Promise.all([
    writeFile(server.stdoutPath, server.stdout, "utf8"),
    writeFile(server.stderrPath, server.stderr, "utf8"),
  ]);
}

async function stopAllServers() {
  await Promise.all(Array.from(activeServers, (server) => stopServer(server)));
}

function assertCondition(condition, label, detail) {
  if (!condition) {
    throw new Error(`${label}: ${detail}`);
  }
  record("PASS", label, detail);
}

async function fetchAndParse(url) {
  const response = await fetch(url, {
    headers: {
      accept: "application/json, text/html",
    },
  });

  const contentType = response.headers.get("content-type") ?? "";
  const text = await response.text();

  let json = null;
  if (contentType.includes("application/json")) {
    try {
      json = JSON.parse(text);
    } catch (error) {
      throw new Error(`failed to parse JSON from ${url}: ${error}`);
    }
  }

  return { response, contentType, text, json };
}

function getServerExitDetail(server) {
  if (server.child.exitCode === null && server.child.signalCode === null) {
    return null;
  }

  const reasonParts = [];
  if (server.child.exitCode !== null) {
    reasonParts.push(`code ${server.child.exitCode}`);
  }
  if (server.child.signalCode !== null) {
    reasonParts.push(`signal ${server.child.signalCode}`);
  }

  const logParts = [];
  const stdoutTail = tail(server.stdout);
  const stderrTail = tail(server.stderr);
  if (stdoutTail) {
    logParts.push(`stdout tail:\n${stdoutTail}`);
  }
  if (stderrTail) {
    logParts.push(`stderr tail:\n${stderrTail}`);
  }

  const reason = reasonParts.join(", ");
  const logs = logParts.length > 0 ? ` ${logParts.join("\n")}` : "";
  return `${reason}.${logs}`;
}

async function waitForReady(server) {
  const endpoint = `http://${HOST}:${server.port}/api/v1/system/info`;
  const deadline = Date.now() + READY_TIMEOUT_MS;
  let lastReadinessError = null;
  let lastReadinessObservation = null;

  while (Date.now() < deadline) {
    if (server.spawnError) {
      throw new Error(`${server.name} spawn failed: ${server.spawnError}`);
    }

    const exitDetail = getServerExitDetail(server);
    if (exitDetail) {
      throw new Error(`${server.name} exited before readiness check (${exitDetail})`);
    }

    try {
      const { response, contentType, text, json } = await fetchAndParse(endpoint);
      if (response.status === 200 && json?.schema === "ao.cli.v1" && json?.ok === true) {
        record("PASS", `${server.name} readiness`, endpoint);
        return;
      }
      if (contentType.includes("application/json")) {
        lastReadinessObservation = `status ${response.status}, schema ${json?.schema ?? "<missing>"}, ok ${json?.ok ?? "<missing>"}`;
      } else {
        lastReadinessObservation = `status ${response.status}, content-type ${contentType || "<missing>"}, body tail ${tail(text, 200)}`;
      }
    } catch (error) {
      lastReadinessError = toErrorMessage(error);
      // Server startup in progress; retry until timeout.
    }

    await sleep(POLL_INTERVAL_MS);
  }

  const details = [];
  if (lastReadinessObservation) {
    details.push(`last readiness observation: ${tail(lastReadinessObservation, 400)}`);
  }
  if (lastReadinessError) {
    details.push(`last readiness error: ${tail(lastReadinessError, 400)}`);
  }
  const detail = details.length > 0 ? ` ${details.join("; ")}` : "";
  throw new Error(`${server.name} did not become ready within ${READY_TIMEOUT_MS}ms.${detail}`);
}

async function assertUiRoutes(baseUrl) {
  const routes = ["/", "/dashboard", "/projects", "/reviews/handoff"];

  for (const route of routes) {
    const url = `${baseUrl}${route}`;
    const { response, contentType } = await fetchAndParse(url);
    assertCondition(response.status === 200, `route status ${route}`, `expected 200, got ${response.status}`);
    assertCondition(
      contentType.startsWith("text/html"),
      `route content-type ${route}`,
      `expected text/html, got ${contentType || "<missing>"}`,
    );
  }
}

async function assertSystemInfoEnvelope(baseUrl) {
  const url = `${baseUrl}/api/v1/system/info`;
  const { response, json } = await fetchAndParse(url);

  assertCondition(response.status === 200, "system/info status", `expected 200, got ${response.status}`);
  assertCondition(json?.schema === "ao.cli.v1", "system/info schema", `expected ao.cli.v1, got ${json?.schema}`);
  assertCondition(json?.ok === true, "system/info ok", `expected true, got ${json?.ok}`);
}

async function assertApiOnlyDeepLinkRejection(baseUrl) {
  const url = `${baseUrl}/dashboard`;
  const { response, json } = await fetchAndParse(url);

  assertCondition(
    response.status === 404,
    "api_only deep-link status",
    `expected 404, got ${response.status}`,
  );
  assertCondition(
    json?.schema === "ao.cli.v1",
    "api_only deep-link schema",
    `expected ao.cli.v1, got ${json?.schema}`,
  );
  assertCondition(json?.ok === false, "api_only deep-link ok", `expected false, got ${json?.ok}`);
  assertCondition(
    json?.error?.code === "not_found",
    "api_only deep-link error code",
    `expected not_found, got ${json?.error?.code}`,
  );
  assertCondition(
    json?.error?.exit_code === 3,
    "api_only deep-link exit_code",
    `expected 3, got ${json?.error?.exit_code}`,
  );
}

async function runUiSmoke() {
  const port = await pickOpenPort(HOST);
  const server = createServer({ name: "server-ui", port, apiOnly: false });
  const baseUrl = `http://${HOST}:${port}`;

  try {
    await waitForReady(server);
    await assertUiRoutes(baseUrl);
    await assertSystemInfoEnvelope(baseUrl);
  } finally {
    await stopServer(server);
  }
}

async function runApiOnlySmoke() {
  const port = await pickOpenPort(HOST);
  const server = createServer({ name: "server-api-only", port, apiOnly: true });
  const baseUrl = `http://${HOST}:${port}`;

  try {
    await waitForReady(server);
    await assertApiOnlyDeepLinkRejection(baseUrl);
  } finally {
    await stopServer(server);
  }
}

async function writeReport() {
  const output = reportLines.length > 0 ? `${reportLines.join("\n")}\n` : "";
  await writeFile(REPORT_PATH, output, "utf8");
}

async function prepareArtifactDir() {
  const artifactPath = path.resolve(ARTIFACT_DIR);
  const rootPath = path.parse(artifactPath).root;
  if (artifactPath === rootPath) {
    throw new Error(`refusing to clean root path as artifact dir: ${artifactPath}`);
  }

  if (artifactPath === REPO_ROOT || artifactPath === WEB_UI_DIR) {
    throw new Error(`refusing to clean protected path as artifact dir: ${artifactPath}`);
  }

  if (!isWithin(REPO_ROOT, artifactPath)) {
    throw new Error(`artifact dir must be inside repository root: ${artifactPath}`);
  }

  await rm(artifactPath, { recursive: true, force: true });
  await mkdir(artifactPath, { recursive: true });
}

async function main() {
  let artifactDirReady = false;

  try {
    await prepareArtifactDir();
    artifactDirReady = true;
    record("PASS", "repo root", REPO_ROOT);
    record("PASS", "artifact dir", ARTIFACT_DIR);

    await runUiSmoke();
    await runApiOnlySmoke();
    record("PASS", "smoke suite", "all assertions passed");
  } catch (error) {
    const message = toErrorMessage(error);
    record("FAIL", "smoke suite", message);
    throw error;
  } finally {
    await stopAllServers();
    if (artifactDirReady) {
      await writeReport();
    }
  }
}

main().catch(() => {
  process.exitCode = 1;
});
