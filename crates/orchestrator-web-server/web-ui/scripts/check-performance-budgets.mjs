#!/usr/bin/env node

import { readFileSync, statSync } from "node:fs";
import { basename, relative, resolve } from "node:path";
import { gzipSync } from "node:zlib";

const JS_GZIP_BUDGET_BYTES = 110 * 1024;
const CSS_GZIP_BUDGET_BYTES = 36 * 1024;

const embeddedDirPath = resolve(import.meta.dirname, "..", "..", "embedded");
const embeddedIndexPath = resolve(embeddedDirPath, "index.html");
const failures = [];

const embeddedIndexSource = readEmbeddedIndexSource(embeddedIndexPath, failures);
const scriptAssetPaths = embeddedIndexSource
  ? extractReferencedAssets(
      embeddedIndexSource,
      /<script\b[^>]*\bsrc=["']([^"']+\.js(?:[?#][^"']*)?)["'][^>]*><\/script>/g,
    )
  : [];
const stylesheetAssetPaths = embeddedIndexSource
  ? extractReferencedAssets(
      embeddedIndexSource,
      /<link\b[^>]*\brel=["']stylesheet["'][^>]*\bhref=["']([^"']+\.css(?:[?#][^"']*)?)["'][^>]*>/g,
    )
  : [];

const jsEntryAsset = pickEntryAsset(scriptAssetPaths, ".js");
const cssEntryAsset = pickEntryAsset(stylesheetAssetPaths, ".css");

if (!jsEntryAsset) {
  failures.push("Missing referenced JS entry asset in embedded/index.html");
}

if (!cssEntryAsset) {
  failures.push("Missing referenced CSS entry asset in embedded/index.html");
}

if (jsEntryAsset) {
  const jsResult = buildAssetResult(jsEntryAsset, JS_GZIP_BUDGET_BYTES);
  if (jsResult.kind === "error") {
    failures.push(jsResult.message);
  } else {
    reportAssetResult("JS", jsResult.result);
    if (jsResult.result.isOverBudget) {
      failures.push(
        `JS entry asset is over budget (${formatBytes(jsResult.result.gzipBytes)} > ${formatBytes(JS_GZIP_BUDGET_BYTES)})`,
      );
    }
  }
}

if (cssEntryAsset) {
  const cssResult = buildAssetResult(cssEntryAsset, CSS_GZIP_BUDGET_BYTES);
  if (cssResult.kind === "error") {
    failures.push(cssResult.message);
  } else {
    reportAssetResult("CSS", cssResult.result);
    if (cssResult.result.isOverBudget) {
      failures.push(
        `CSS entry asset is over budget (${formatBytes(cssResult.result.gzipBytes)} > ${formatBytes(CSS_GZIP_BUDGET_BYTES)})`,
      );
    }
  }
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.error(`[budget:fail] ${failure}`);
  }
  process.exit(1);
}

console.log("[budget:ok] Embedded entry assets meet gzip budgets");

function readEmbeddedIndexSource(indexPath, failures) {
  try {
    return readFileSync(indexPath, "utf8");
  } catch (error) {
    failures.push(
      `Unable to read embedded/index.html at ${indexPath}. Run \`npm run build\` before budget checks.`,
    );
    if (error instanceof Error && error.message) {
      failures.push(`embedded/index.html read error: ${error.message}`);
    }
    return null;
  }
}

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
  if (!isInsideDirectory(embeddedDirPath, absoluteAssetPath)) {
    return {
      kind: "error",
      message: `Referenced asset resolves outside embedded directory: ${assetPath}`,
    };
  }

  try {
    const rawBytes = statSync(absoluteAssetPath).size;
    const gzipBytes = gzipSync(readFileSync(absoluteAssetPath), { level: 9 }).byteLength;

    return {
      kind: "ok",
      result: {
        assetPath,
        rawBytes,
        gzipBytes,
        budgetBytes,
        isOverBudget: gzipBytes > budgetBytes,
      },
    };
  } catch (_error) {
    return {
      kind: "error",
      message: `Referenced asset is missing or unreadable: ${assetPath}`,
    };
  }
}

function normalizeAssetPath(assetPath) {
  const [withoutQuery] = assetPath.split(/[?#]/, 1);
  return withoutQuery.startsWith("/") ? withoutQuery.slice(1) : withoutQuery;
}

function isInsideDirectory(parentPath, childPath) {
  const relativePath = relative(parentPath, childPath);
  return relativePath === "" || (!relativePath.startsWith("..") && !relativePath.startsWith("/"));
}

function reportAssetResult(assetType, result) {
  console.log(
    `[budget:check] ${assetType} ${result.assetPath} raw=${formatBytes(result.rawBytes)} gzip=${formatBytes(result.gzipBytes)} budget=${formatBytes(result.budgetBytes)}`,
  );
}

function formatBytes(bytes) {
  return `${bytes} B (${(bytes / 1024).toFixed(2)} KiB)`;
}
