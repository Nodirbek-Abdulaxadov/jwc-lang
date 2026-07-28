import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'JWC',
  tagline: 'A backend-first language for SQL-native business-logic APIs',
  favicon: 'img/favicon.ico',

  url: 'https://jwc.1kb.uz',
  baseUrl: '/',

  organizationName: 'Nodirbek-Abdulaxadov',
  projectName: 'jwc-lang',

  onBrokenLinks: 'throw',
  onBrokenMarkdownLinks: 'warn',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          routeBasePath: '/',          // serve docs at site root, no /docs/ prefix
          editUrl:
            'https://github.com/Nodirbek-Abdulaxadov/jwc-lang/edit/main/docs/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/jwc-social-card.png',
    navbar: {
      title: 'JWC',
      logo: {
        alt: 'JWC hummingbird logo',
        src: 'img/logo.png',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'tutorialSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          href: 'https://registry-jwc.1kb.uz/',
          label: 'Registry',
          position: 'right',
        },
        {
          href: 'https://github.com/Nodirbek-Abdulaxadov/jwc-lang',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Learn',
          items: [
            {label: 'Getting started', to: '/getting-started/install'},
            {label: 'Language', to: '/language/syntax'},
            {label: 'Data (SQL)', to: '/data/dbcontext'},
            {label: 'Backend', to: '/backend/routes'},
            {label: 'CLI reference', to: '/cli/overview'},
          ],
        },
        {
          title: 'Tools',
          items: [
            {label: 'Registry', href: 'https://registry-jwc.1kb.uz/'},
            {label: 'GitHub', href: 'https://github.com/Nodirbek-Abdulaxadov/jwc-lang'},
            {label: 'Roadmap', href: 'https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/ROADMAP.md'},
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} JWC. Built with Docusaurus.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['rust', 'sql', 'bash', 'json'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
