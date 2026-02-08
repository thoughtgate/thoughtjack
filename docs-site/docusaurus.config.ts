import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'ThoughtJack',
  tagline: 'Adversarial MCP server for security testing',
  url: 'https://thoughtjack.io',
  baseUrl: '/',

  organizationName: 'thoughtgate',
  projectName: 'thoughtjack',

  onBrokenLinks: 'warn',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  markdown: {
    mermaid: true,
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  themes: ['@docusaurus/theme-mermaid'],

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    navbar: {
      title: 'ThoughtJack',
      items: [
        {
          type: 'dropdown',
          label: 'Documentation',
          position: 'left',
          items: [
            {label: 'Tutorials', to: '/docs/tutorials'},
            {label: 'How-To Guides', to: '/docs/how-to'},
            {label: 'Reference', to: '/docs/reference'},
            {label: 'Explanation', to: '/docs/explanation'},
          ],
        },
        {
          label: 'Attack Library',
          to: '/docs/scenarios',
          position: 'left',
        },
        {
          type: 'dropdown',
          label: 'Coverage',
          position: 'left',
          items: [
            {label: 'MITRE ATT&CK', to: '/docs/coverage/mitre-matrix'},
            {label: 'OWASP MCP Top 10', to: '/docs/coverage/owasp-mcp'},
            {label: 'Attack Surface', to: '/docs/coverage/mcp-attack-surface'},
          ],
        },
        {
          href: 'https://github.com/thoughtgate/thoughtjack',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            {label: 'Tutorials', to: '/docs/tutorials'},
            {label: 'Reference', to: '/docs/reference'},
          ],
        },
        {
          title: 'More',
          items: [
            {label: 'GitHub', href: 'https://github.com/thoughtgate/thoughtjack'},
          ],
        },
      ],
      copyright: `Copyright ${new Date().getFullYear()} ThoughtJack. Built with Docusaurus.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['yaml', 'rust', 'bash'],
    },
    mermaid: {
      theme: {light: 'default', dark: 'dark'},
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
