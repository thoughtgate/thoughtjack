import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';

import styles from './scenarios.module.css';

export default function Scenarios(): React.ReactElement {
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

          <section className={styles.cta}>
            <h2>Run scenarios from the CLI</h2>
            <pre className={styles.ctaCode}>
              <code>
                {'# List all built-in scenarios\n'}
                {'thoughtjack scenarios list\n\n'}
                {'# Run a scenario against your agent\n'}
                {'thoughtjack scenarios run oatf-002 --mcp-server 127.0.0.1:8080\n\n'}
                {'# View the YAML for customization\n'}
                {'thoughtjack scenarios show oatf-002 > my-scenario.yaml'}
              </code>
            </pre>

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
