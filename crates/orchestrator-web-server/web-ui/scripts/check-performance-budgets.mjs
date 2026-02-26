#!/usr/bin/env node

import { readFileSync, statSync } from "node:fs";
import { basename, resolve } from "node:path";
import { gzipSync } from "node:zlib";

const JS_GZIP_BUDGET_BYTES = 110 * 1024;
const CSS_GZIP_BUDGET_BYTES = 8 * 1024;

const embeddedDirPath = resolve(import.meta.dirname, "..", "..", "embedded");
const embeddedIndexPath = resolve(embeddedDirPath, "index.html");
const embeddedIndexSource = readFileSync(embeddedIndexPath, "utf8");

const scriptAssetPaths = extractReferencedAssets(
  embeddedIndexSource,
  /<script\b[^>]*\bsrc="([^"]+\.js)"[^>]*><\/script>/g,
);
const stylesheetAssetPaths = extractReferencedAssets(
  embeddedIndexSource,
  /<link\b[^>]*\brel="stylesheet"[^>]*\bhref="([^"]+\.css)"[^>]*>/g,
);

const jsEntryAsset = pickEntryAsset(scriptAssetPaths, ".js");
const cssEntryAsset = pickEntryAsset(stylesheetAssetPaths, ".css");

const failures = [];

if (!jsEntryAsset) {
  failures.push("Missing referenced JS entry asset in embedded/index.html");
}

if (!cssEntryAsset) {
  failures.push("Missing referenced CSS entry asset in embedded/index.html");
}

if (jsEntryAsset) {
  const jsResult = buildAssetResult(jsEntryAsset, JS_GZIP_BUDGET_BYTES);
  reportAssetResult("JS", jsResult);
  if (jsResult.isOverBudget) {
    failures.push(
      `JS entry asset is over budget (${formatBytes(jsResult.gzipBytes)} > ${formatBytes(JS_GZIP_BUDGET_BYTES)})`,
    );
  }
}

if (cssEntryAsset) {
  const cssResult = buildAssetResult(cssEntryAsset, CSS_GZIP_BUDGET_BYTES);
  reportAssetResult("CSS", cssResult);
  if (cssResult.isOverBudget) {
    failures.push(
      `CSS entry asset is over budget (${formatBytes(cssResult.gzipBytes)} > ${formatBytes(CSS_GZIP_BUDGET_BYTES)})`,
    );
  }
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.error(`[budget:fail] ${failure}`);
  }
  process.exit(1);
}

console.log("[budget:ok] Embedded entry assets meet gzip budgets");

function extractReferencedAssets(source, pattern) {
  const referencedAssets = [];
  let match = pattern.exec(source);

  while (match) {
    referencedAssets.push(match[1]);
    match = pattern.exec(source);
  }

  return Array.from(new Set(referencedAssets));
}

function pickEntryAsset(assetPaths, extension) {
  if (assetPaths.length === 0) {
    return null;
  }

  const namedEntry = assetPaths.find((assetPath) => {
    const fileName = basename(assetPath);
    return fileName.startsWith("index-") && fileName.endsWith(extension);
  });

  return namedEntry ?? assetPaths[0];
}

function buildAssetResult(assetPath, budgetBytes) {
  const relativeAssetPath = normalizeAssetPath(assetPath);
  const absoluteAssetPath = resolve(embeddedDirPath, relativeAssetPath);
  const rawBytes = statSync(absoluteAssetPath).size;
  const gzipBytes = gzipSync(readFileSync(absoluteAssetPath), { level: 9 }).byteLength;

  return {
    assetPath,
    rawBytes,
    gzipBytes,
    budgetBytes,
    isOverBudget: gzipBytes > budgetBytes,
  };
}

function normalizeAssetPath(assetPath) {
  return assetPath.startsWith("/") ? assetPath.slice(1) : assetPath;
}

function reportAssetResult(assetType, result) {
  console.log(
    `[budget:check] ${assetType} ${result.assetPath} raw=${formatBytes(result.rawBytes)} gzip=${formatBytes(result.gzipBytes)} budget=${formatBytes(result.budgetBytes)}`,
  );
}

function formatBytes(bytes) {
  return `${bytes} B (${(bytes / 1024).toFixed(2)} KiB)`;
}
