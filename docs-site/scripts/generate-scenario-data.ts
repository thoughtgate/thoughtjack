/**
 * Generates scenario data JSON from OATF YAML files in the scenarios submodule.
 * Run before docs build: `npx tsx scripts/generate-scenario-data.ts`
 *
 * Output: src/data/scenarios.json
 */

import fs from 'node:fs';
import path from 'node:path';
import yaml from 'js-yaml';

const LIBRARY_DIR = path.resolve(__dirname, '../../scenarios/library');
const OUTPUT_PATH = path.resolve(__dirname, '../src/data/scenarios.json');

interface ScenarioEntry {
  id: string;
  name: string;
  description: string;
  severity: string;
  protocols: string[];
  status: string;
  tags: string[];
}

function getProtocol(mode: string): string {
  if (mode.startsWith('mcp_')) return 'MCP';
  if (mode.startsWith('a2a_')) return 'A2A';
  if (mode.startsWith('ag_ui_')) return 'AG-UI';
  return mode.toUpperCase();
}

function walkYaml(dir: string): string[] {
  const results: string[] = [];
  if (!fs.existsSync(dir)) return results;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) results.push(...walkYaml(full));
    else if (entry.name.endsWith('.yaml')) results.push(full);
  }
  return results;
}

function main() {
  const files = walkYaml(LIBRARY_DIR);
  const scenarios: ScenarioEntry[] = [];

  for (const file of files) {
    const doc = yaml.load(fs.readFileSync(file, 'utf-8')) as any;
    if (!doc?.attack?.id) continue;

    const attack = doc.attack;
    if (attack.status === 'draft') continue; // skip drafts

    const protocols = new Set<string>();
    const exec = attack.execution;
    if (exec?.actors) {
      for (const actor of exec.actors) {
        if (actor.mode) protocols.add(getProtocol(actor.mode));
      }
    } else if (exec?.mode) {
      protocols.add(getProtocol(exec.mode));
    }

    const desc = (attack.description ?? '').trim();

    scenarios.push({
      id: attack.id,
      name: attack.name ?? attack.id,
      description: desc.length > 200 ? desc.slice(0, 200) + '...' : desc,
      severity: attack.severity?.level ?? 'unknown',
      protocols: [...protocols],
      status: attack.status ?? 'experimental',
      tags: Array.isArray(attack.classification?.tags) ? attack.classification.tags : [],
    });
  }

  scenarios.sort((a, b) => a.id.localeCompare(b.id));

  fs.mkdirSync(path.dirname(OUTPUT_PATH), { recursive: true });
  fs.writeFileSync(OUTPUT_PATH, JSON.stringify(scenarios, null, 2));
  console.log(`Generated ${OUTPUT_PATH} with ${scenarios.length} scenarios`);
}

main();
