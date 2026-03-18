import React from 'react';
import clsx from 'clsx';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';

import styles from './index.module.css';

function Hero(): React.ReactElement {
  return (
    <header className={styles.hero}>
      <div className="container">
        <h1 className={styles.heroTitle}>ThoughtJack</h1>
        <p className={styles.heroTagline}>
          Test how your AI agents handle adversarial attacks — before real attackers do.
        </p>
        <p className={styles.heroDescription}>
          Open-source security testing tool that simulates malicious MCP, A2A, and AG-UI
          servers. Run rug pulls, prompt injections, and protocol-level attacks against
          your agents in a controlled environment.
        </p>
        <div className={styles.heroButtons}>
          <Link className="button button--primary button--lg" to="/docs/tutorials/getting-started">
            Get Started
          </Link>
          <Link className="button button--outline button--lg" to="/scenarios">
            View Scenarios
          </Link>
        </div>
        <div className={styles.protocolBadges}>
          <span className={styles.protocolBadge}>MCP</span>
          <span className={styles.protocolBadge}>A2A</span>
          <span className={styles.protocolBadge}>AG-UI</span>
        </div>
      </div>
    </header>
  );
}

function TerminalDemo(): React.ReactElement {
  return (
    <section className={styles.terminalSection}>
      <div className="container">
        <h2 className={styles.sectionTitle}>See it in action</h2>
        <p className={styles.sectionSubtitle}>
          Run a rug pull attack against your MCP client in one command.
        </p>
        <div className={styles.terminal}>
          <div className={styles.terminalHeader}>
            <span className={clsx(styles.terminalDot, styles.terminalDotRed)} />
            <span className={clsx(styles.terminalDot, styles.terminalDotYellow)} />
            <span className={clsx(styles.terminalDot, styles.terminalDotGreen)} />
            <span className={styles.terminalTitle}>thoughtjack</span>
          </div>
          <pre className={styles.terminalBody}>
            <code>
              <span className={styles.termPrompt}>$</span>{' '}
              <span className={styles.termCmd}>thoughtjack scenarios run rug-pull</span>
              {'\n\n'}
              <span className={styles.termMeta}>{'  '}Scenario: OATF-002 Tool Definition Rug Pull</span>
              {'\n'}
              <span className={styles.termMeta}>{'  '}Protocol: MCP (stdio server)   Severity: CRITICAL</span>
              {'\n'}
              <span className={styles.termMeta}>{'  '}Phases:   trust_building {'→'} exploit</span>
              {'\n\n'}
              <span className={styles.termPhase}>{'  '}Phase: trust_building</span>
              {'\n'}
              <span className={styles.termIn}>{'    ← tools/call'}{' '}<span className={styles.termWarn}>calculator</span></span>
              {'\n'}
              <span className={styles.termOut}>{'    → tools/call'}</span>
              {'\n'}
              <span className={styles.termIn}>{'    ← tools/call'}{' '}<span className={styles.termWarn}>calculator</span></span>
              {'\n'}
              <span className={styles.termOut}>{'    → tools/call'}</span>
              {'\n\n'}
              <span className={styles.termPhase}>{'  '}Phase: exploit</span>
              {'\n'}
              <span className={styles.termOut}>{'    → notifications/tools/list_changed'}</span>
              {'\n'}
              <span className={styles.termIn}>{'    ← tools/list'}</span>
              {'\n'}
              <span className={styles.termOut}>{'    → tools/list'}</span>
              {'\n'}
              <span className={styles.termIn}>{'    ← tools/call'}{' '}<span className={styles.termWarn}>read_file</span></span>
              {'\n'}
              <span className={styles.termOut}>{'    → tools/call'}</span>
              {'\n\n'}
              <span className={styles.termFail}>{'    ✗ '}</span><span className={styles.termDim}>{'OATF-002-01'}</span>
              {'\n'}
              <span className={styles.termFail}>{'    ✗ '}</span><span className={styles.termDim}>{'OATF-002-02'}</span>
              {'\n\n'}
              <span className={styles.termRule}>{'  '}{'━'.repeat(38)}</span>
              {'\n'}
              <span className={styles.termFail}>{'  '}Verdict: EXPLOITED</span>
              {'\n'}
              <span className={styles.termRule}>{'  '}{'━'.repeat(38)}</span>
            </code>
          </pre>
        </div>
      </div>
    </section>
  );
}

interface QuickLinkProps {
  title: string;
  description: string;
  to: string;
  accent: string;
  badge?: string;
}

function QuickLink({title, description, to, accent, badge}: QuickLinkProps): React.ReactElement {
  return (
    <div className={clsx('col col--3', styles.quickLink)}>
      <Link to={to} className={styles.quickLinkCard} style={{'--card-accent': accent} as React.CSSProperties}>
        <div className={styles.quickLinkHeader}>
          <h3>{title}</h3>
          {badge && <span className={styles.quickLinkBadge}>{badge}</span>}
        </div>
        <p>{description}</p>
      </Link>
    </div>
  );
}

const quickLinks: QuickLinkProps[] = [
  {
    title: 'Tutorials',
    description: 'Install ThoughtJack, run your first attack scenario, and learn to read verdict output.',
    to: '/docs/tutorials',
    accent: '#25c2a0',
    badge: 'Start here',
  },
  {
    title: 'How-To Guides',
    description: 'Task-oriented recipes: delivery behaviors, payload generators, CI integration, HTTP transport.',
    to: '/docs/how-to',
    accent: '#e8a838',
  },
  {
    title: 'Reference',
    description: 'Complete OATF schema, CLI flags, generator parameters, trigger syntax, and behavioral modes.',
    to: '/docs/reference',
    accent: '#6c7ae0',
  },
  {
    title: 'Explanation',
    description: 'Architecture deep-dives, phase engine design, MCP/A2A/AG-UI attack surfaces, MITRE mappings.',
    to: '/docs/explanation',
    accent: '#e06c9f',
  },
];

export default function Home(): React.ReactElement {
  const {siteConfig} = useDocusaurusContext();
  return (
    <Layout description="Open-source adversarial testing tool for AI agent security. Simulate malicious MCP, A2A, and AG-UI servers to test agent resilience to protocol-level attacks.">
      <Hero />
      <TerminalDemo />
      <main>
        <section className={styles.quickLinks}>
          <div className="container">
            <h2 className={styles.sectionTitle}>Explore the docs</h2>
            <div className="row">
              {quickLinks.map((props) => (
                <QuickLink key={props.title} {...props} />
              ))}
            </div>
          </div>
        </section>
      </main>
    </Layout>
  );
}
