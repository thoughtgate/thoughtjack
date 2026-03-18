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

  themes: [
    '@docusaurus/theme-mermaid',
    [
      '@easyops-cn/docusaurus-search-local',
      {
        hashed: true,
        language: ['en'],
        highlightSearchTermsOnTargetPage: true,
        explicitSearchResultPath: true,
      },
    ],
  ],

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
        sitemap: {
          changefreq: 'weekly',
          priority: 0.6,
          filename: 'sitemap.xml',
          createSitemapItems: async (params) => {
            const {defaultCreateSitemapItems, ...rest} = params;
            const items = await defaultCreateSitemapItems(rest);
            return items.map((item) => {
              if (item.url.endsWith('/')) {
                return {...item, priority: 1.0, changefreq: 'daily'};
              }
              if (
                item.url.includes('/docs/tutorials') && !item.url.includes('/docs/tutorials/') ||
                item.url.includes('/docs/how-to') && !item.url.includes('/docs/how-to/') ||
                item.url.includes('/docs/reference') && !item.url.includes('/docs/reference/') ||
                item.url.includes('/docs/explanation') && !item.url.includes('/docs/explanation/')
              ) {
                return {...item, priority: 0.8};
              }
              return item;
            });
          },
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/og-image.png',
    metadata: [
      {name: 'description', content: 'Open-source adversarial testing tool for AI agent security. Simulate malicious MCP, A2A, and AG-UI servers to test agent resilience to protocol-level attacks.'},
      {name: 'keywords', content: 'MCP security, AI agent security, adversarial testing, protocol security, OATF, ThoughtJack, MCP attack, agent testing tool'},
      {name: 'twitter:card', content: 'summary_large_image'},
    ],
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
        {label: 'Scenarios', to: '/scenarios', position: 'left'},
        {
          type: 'html',
          position: 'left',
          value: '<span class="navbar-version">v0.5</span>',
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
          title: 'Documentation',
          items: [
            {label: 'Tutorials', to: '/docs/tutorials'},
            {label: 'How-To Guides', to: '/docs/how-to'},
            {label: 'Reference', to: '/docs/reference'},
            {label: 'Explanation', to: '/docs/explanation'},
          ],
        },
        {
          title: 'Resources',
          items: [
            {label: 'Scenario Library', to: '/scenarios'},
            {label: 'OATF Specification', href: 'https://oatf.io'},
          ],
        },
        {
          title: 'Community',
          items: [
            {label: 'GitHub', href: 'https://github.com/thoughtgate/thoughtjack'},
            {label: 'Issues', href: 'https://github.com/thoughtgate/thoughtjack/issues'},
            {label: 'Discussions', href: 'https://github.com/thoughtgate/thoughtjack/discussions'},
          ],
        },
      ],
      copyright: `Copyright ${new Date().getFullYear()} ThoughtJack. Built with Docusaurus.`,
    },
    colorMode: {
      defaultMode: 'dark',
      respectPrefersColorScheme: true,
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
    {
      tagName: 'script',
      attributes: {type: 'application/ld+json'},
      innerHTML: JSON.stringify({
        '@context': 'https://schema.org',
        '@type': 'SoftwareApplication',
        name: 'ThoughtJack',
        description: 'Open-source adversarial testing tool for AI agent security. Simulate malicious MCP, A2A, and AG-UI servers to test agent resilience to protocol-level attacks.',
        url: 'https://thoughtjack.io',
        applicationCategory: 'SecurityApplication',
        operatingSystem: 'Linux, macOS, Windows',
        programmingLanguage: 'Rust',
        license: 'https://github.com/thoughtgate/thoughtjack/blob/main/LICENSE',
        offers: {
          '@type': 'Offer',
          price: '0',
          priceCurrency: 'USD',
        },
      }),
    },
    {
      tagName: 'link',
      attributes: {
        rel: 'canonical',
        href: 'https://thoughtjack.io',
      },
    },
  ],
};

export default config;
