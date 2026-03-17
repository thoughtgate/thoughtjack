import React from 'react';
import clsx from 'clsx';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';

import styles from './index.module.css';

function Hero(): React.ReactElement {
  const {siteConfig} = useDocusaurusContext();
  return (
    <header className={styles.heroBanner}>
      <div className="container">
        <h1 className={styles.heroTitle}>{siteConfig.title}</h1>
        <p className={styles.heroSubtitle}>{siteConfig.tagline}</p>
        <div className={styles.buttons}>
          <Link className={styles.ctaButton} to="/docs/tutorials">
            Get Started
          </Link>
        </div>
      </div>
    </header>
  );
}

interface QuickLinkProps {
  title: string;
  description: string;
  to: string;
}

function QuickLink({title, description, to}: QuickLinkProps): React.ReactElement {
  return (
    <div className={clsx('col col--3', styles.quickLink)}>
      <Link to={to} className={styles.quickLinkCard}>
        <h3>{title}</h3>
        <p>{description}</p>
      </Link>
    </div>
  );
}

const quickLinks: QuickLinkProps[] = [
  {
    title: 'Tutorials',
    description: 'Step-by-step guides to get started with ThoughtJack.',
    to: '/docs/tutorials',
  },
  {
    title: 'How-To Guides',
    description: 'Task-oriented recipes for common operations.',
    to: '/docs/how-to',
  },
  {
    title: 'Reference',
    description: 'Configuration schema, CLI, and API reference.',
    to: '/docs/reference',
  },
  {
    title: 'Explanation',
    description: 'Architecture, engine design, and attack concepts.',
    to: '/docs/explanation',
  },
];

export default function Home(): React.ReactElement {
  const {siteConfig} = useDocusaurusContext();
  return (
    <Layout description={siteConfig.tagline}>
      <Hero />
      <main>
        <section className={styles.quickLinks}>
          <div className="container">
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
