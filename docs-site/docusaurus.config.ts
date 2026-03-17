import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'ThoughtJack',
  tagline: 'Adversarial agent security testing tool',
  url: 'https://thoughtjack.io',
  baseUrl: '/',

  organizationName: 'thoughtgate',
  projectName: 'thoughtjack',

  onBrokenLinks: 'throw',
  trailingSlash: false,

  scripts: [
    {
      src: 'https://www.googletagmanager.com/gtag/js?id=G-1X0RR1611Q',
      async: true,
    },
  ],

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
        {label: 'Documentation', to: '/docs/tutorials', position: 'left'},
        {label: 'Reference', to: '/docs/reference', position: 'left'},
        {label: 'About', to: '/docs/explanation', position: 'left'},
        {
          href: 'https://github.com/thoughtgate/thoughtjack',
          position: 'right',
          className: 'header-github-link',
          'aria-label': 'GitHub repository',
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
    colorMode: {
      defaultMode: 'dark',
      disableSwitch: true,
      respectPrefersColorScheme: false,
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

  headTags: [
    {
      tagName: 'script',
      attributes: {},
      innerHTML: `
        window.dataLayer = window.dataLayer || [];
        function gtag(){dataLayer.push(arguments);}
        gtag('js', new Date());
        gtag('config', 'G-1X0RR1611Q');
      `,
    },
  ],
};

export default config;
