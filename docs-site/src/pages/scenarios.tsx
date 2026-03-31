import React, {useState} from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';

import styles from './scenarios.module.css';
import scenarioData from '../data/scenarios.json';

type Protocol = 'MCP' | 'A2A' | 'AG-UI';
type Severity = 'critical' | 'high' | 'medium' | 'low' | 'unknown';

interface Scenario {
  id: string;
  name: string;
  description: string;
  severity: Severity;
  protocols: string[];
  status: string;
  tags: string[];
}

const scenarios: Scenario[] = scenarioData as Scenario[];

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
        <SeverityBadge severity={scenario.severity as Severity} />
        {scenario.protocols.map((p) => (
          <span key={p} className={styles.scenarioProtocol}>{p}</span>
        ))}
      </div>
      <h3 className={styles.scenarioName}>{scenario.name}</h3>
      <p className={styles.scenarioDesc}>{scenario.description}</p>
      <code className={styles.scenarioCmd}>
        thoughtjack scenarios run {scenario.id.toLowerCase()}
      </code>
    </div>
  );
}

export default function Scenarios(): React.ReactElement {
  const [filter, setFilter] = useState<Protocol | 'All'>('All');

  const filtered = filter === 'All'
    ? scenarios
    : scenarios.filter((s) => s.protocols.includes(filter));

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
              ThoughtJack ships with {scenarios.length} built-in attack scenarios across MCP, A2A, and AG-UI
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
                {tab.label} ({tab.value === 'All' ? scenarios.length : scenarios.filter(s => s.protocols.includes(tab.value)).length})
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
              that ThoughtJack ships with.
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
