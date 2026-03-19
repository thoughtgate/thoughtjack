import React, {useState} from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';

import styles from './scenarios.module.css';

type Protocol = 'MCP' | 'A2A' | 'AG-UI';
type Severity = 'critical' | 'high' | 'medium' | 'low';

interface Scenario {
  id: string;
  name: string;
  description: string;
  severity: Severity;
  protocol: Protocol;
}

const scenarios: Scenario[] = [
  {
    id: 'oatf-001',
    name: 'Tool Description Prompt Injection',
    description: 'Injects adversarial instructions into MCP tool descriptions to manipulate agent behavior during tool discovery.',
    severity: 'critical',
    protocol: 'MCP',
  },
  {
    id: 'oatf-002',
    name: 'Tool Definition Rug Pull',
    description: 'Builds trust with benign tool definitions, then swaps them for malicious versions mid-session via tools/list_changed.',
    severity: 'critical',
    protocol: 'MCP',
  },
  {
    id: 'oatf-004',
    name: 'Tool Response Injection',
    description: 'Returns crafted tool call responses containing embedded instructions that hijack the agent\'s subsequent actions.',
    severity: 'critical',
    protocol: 'MCP',
  },
  {
    id: 'oatf-005',
    name: 'Confused Deputy via Tool Invocation',
    description: 'Tricks the agent into invoking privileged tools on behalf of the attacker by abusing trust relationships.',
    severity: 'critical',
    protocol: 'MCP',
  },
  {
    id: 'oatf-006',
    name: 'Data Exfiltration via Tool Calls',
    description: 'Manipulates the agent into sending sensitive data to attacker-controlled endpoints through tool call parameters.',
    severity: 'critical',
    protocol: 'MCP',
  },
  {
    id: 'oatf-007',
    name: 'MCP Server Supply Chain Attack',
    description: 'Simulates a compromised MCP server package that activates malicious behavior after an update or delay.',
    severity: 'critical',
    protocol: 'MCP',
  },
  {
    id: 'oatf-008',
    name: 'Cross-Agent Prompt Injection',
    description: 'Injects adversarial payloads through A2A message exchanges to compromise downstream agents in multi-agent systems.',
    severity: 'critical',
    protocol: 'A2A',
  },
  {
    id: 'oatf-009',
    name: 'Agent Card Spoofing / Impersonation',
    description: 'Presents a falsified Agent Card to impersonate trusted agents, gaining unauthorized access to sensitive operations.',
    severity: 'high',
    protocol: 'A2A',
  },
  {
    id: 'oatf-010',
    name: 'Goal Hijacking / Instruction Override',
    description: 'Overrides the agent\'s original goal through injected instructions that redirect its actions toward attacker objectives.',
    severity: 'critical',
    protocol: 'MCP',
  },
  {
    id: 'oatf-011',
    name: 'AG-UI Message List Injection',
    description: 'Injects adversarial content into AG-UI message streams to manipulate the agent\'s conversation context and actions.',
    severity: 'critical',
    protocol: 'AG-UI',
  },
];

const filterTabs: Array<{label: string; value: Protocol | 'All'}> = [
  {label: 'All', value: 'All'},
  {label: 'MCP', value: 'MCP'},
  {label: 'A2A', value: 'A2A'},
  {label: 'AG-UI', value: 'AG-UI'},
];

function SeverityBadge({severity}: {severity: Severity}): React.ReactElement {
  return (
    <span className={`tj-severity-badge tj-severity-badge--${severity}`}>
      {severity}
    </span>
  );
}

function ScenarioCard({scenario}: {scenario: Scenario}): React.ReactElement {
  return (
    <div className={styles.scenarioCard}>
      <div className={styles.scenarioCardHeader}>
        <span className={styles.scenarioId}>{scenario.id}</span>
        <SeverityBadge severity={scenario.severity} />
        <span className={styles.scenarioProtocol}>{scenario.protocol}</span>
      </div>
      <h3 className={styles.scenarioName}>{scenario.name}</h3>
      <p className={styles.scenarioDesc}>{scenario.description}</p>
      <code className={styles.scenarioCmd}>
        thoughtjack scenarios run {scenario.id}
      </code>
    </div>
  );
}

export default function Scenarios(): React.ReactElement {
  const [filter, setFilter] = useState<Protocol | 'All'>('All');

  const filtered = filter === 'All'
    ? scenarios
    : scenarios.filter((s) => s.protocol === filter);

  return (
    <Layout
      title="Scenario Library"
      description="ThoughtJack ships with built-in OATF attack scenarios for MCP, A2A, and AG-UI protocols."
    >
      <div className={styles.page}>
        <div className="container">
          <header className={styles.header}>
            <h1>Scenario Library</h1>
            <p className={styles.subtitle}>
              ThoughtJack ships with 10 built-in attack scenarios across MCP, A2A, and AG-UI
              protocols. Each scenario is an{' '}
              <a href="https://oatf.dev" target="_blank" rel="noopener noreferrer">
                OATF
              </a>{' '}
              document ready to run from the CLI.
            </p>
          </header>

          <div className={styles.filterTabs}>
            {filterTabs.map((tab) => (
              <button
                key={tab.value}
                className={`${styles.filterTab} ${filter === tab.value ? styles.filterTabActive : ''}`}
                onClick={() => setFilter(tab.value)}
                type="button"
              >
                {tab.label}
              </button>
            ))}
          </div>

          <div className={styles.scenarioGrid}>
            {filtered.map((scenario) => (
              <ScenarioCard key={scenario.id} scenario={scenario} />
            ))}
          </div>

          <section className={styles.cta}>
            <h2>Browse the full registry</h2>
            <p className={styles.subtitle}>
              The canonical scenario registry is maintained at{' '}
              <a href="https://oatf.dev" target="_blank" rel="noopener noreferrer">
                <strong>oatf.dev</strong>
              </a>
              , auto-generated from the same{' '}
              <a
                href="https://github.com/oatf-spec/scenarios"
                target="_blank"
                rel="noopener noreferrer"
              >
                git submodule
              </a>{' '}
              that ThoughtJack ships with. Visit it for detailed scenario documentation,
              indicator definitions, and MITRE ATT&CK mappings.
            </p>

            <div className={styles.ctaLinks}>
              <a
                className="button button--primary button--lg"
                href="https://oatf.dev"
                target="_blank"
                rel="noopener noreferrer"
              >
                Browse scenarios on oatf.dev
              </a>
              <Link className="button button--outline button--lg" to="/docs/tutorials/getting-started">
                Getting Started Tutorial
              </Link>
            </div>
          </section>
        </div>
      </div>
    </Layout>
  );
}
