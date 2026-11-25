#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * List, close and select broker tabs.
 *
 * Obscura scopes targets to a WebSocket connection, so "tabs" here means the
 * broker's tabs. There is no way to see tabs from another connection.
 */

import { brokerRequest, ensureBroker } from "./broker-client.js";
import { parseFlags } from "./cli.js";

const { positional, has, unknown } = parseFlags(process.argv.slice(2), { values: [] });

if (unknown.length > 0) {
  console.error(`✗ Unknown option: ${unknown.join(", ")}`);
  process.exit(1);
}

const command = positional[0];

try {
  await ensureBroker();
  const { tabs } = await brokerRequest({ op: "tabs" });

  if (command === "close") {
    const tabId = positional[1];
    if (!tabId) {
      console.error("✗ tabs.js close <tabId>");
      process.exit(1);
    }
    const result = await brokerRequest({ op: "closeTab", tabId });
    console.log(`✓ Closed ${result.closed}`);
    process.exit(0);
  }

  if (command === "use") {
    const tabId = positional[1];
    if (!tabId) {
      console.error("✗ tabs.js use <tabId>");
      process.exit(1);
    }
    const result = await brokerRequest({ op: "select", tabId });
    console.log(`✓ Default tab is now ${result.defaultTabId}`);
    process.exit(0);
  }

  if (tabs.length === 0) {
    console.log("No open tabs. Run nav.js to create one.");
    process.exit(0);
  }

  for (const tab of tabs) {
    console.log(`${tab.default ? "*" : " "} ${tab.tabId}  ${tab.url}`);
  }
  if (has("--json")) console.log(JSON.stringify(tabs, null, 2));
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
}
