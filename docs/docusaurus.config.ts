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
          editUrl:
            'https://github.com/Nodirbek-Abdulaxadov/jwc-lang/edit/main/docs/',
        },
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/docusaurus-social-card.jpg',
    navbar: {
      title: 'JWC',
      logo: {
        alt: 'JWC Logo',
        src: 'img/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'tutorialSidebar',
          position: 'left',
          label: 'Docs',
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
            {label: 'Getting started', to: '/docs/getting-started'},
            {label: 'Language tour', to: '/docs/language-tour'},
            {label: 'Standard library', to: '/docs/stdlib'},
          ],
        },
        {
          title: 'More',
          items: [
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
