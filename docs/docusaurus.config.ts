import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'JWC',
  tagline: 'A backend-first language for SQL-native business-logic APIs',
  favicon: 'img/favicon.ico',

  url: 'https://jwc.1kb.uz',
  baseUrl: '/',

  organizationName: 'just-web-code',
  projectName: 'jwc-lang',

  // Every canonical tag and every sitemap entry is emitted without a
  // trailing slash, so say so explicitly and keep nginx serving the same
  // shape. One URL per page is the whole point: the slashed form used to
  // be a second live URL that 301'd back here.
  trailingSlash: false,

  onBrokenLinks: 'throw',
  onBrokenMarkdownLinks: 'warn',
  // A dead in-page anchor is a dead link to a crawler too; two of them sat
  // in the build output as warnings for long enough to prove that warning
  // isn't enough.
  onBrokenAnchors: 'throw',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          // The 1.0 documentation. `archive-0.9/` is still in the repo and
          // still describes the language deployed 0.9.x binaries implement,
          // but it is no longer served: every code sample on it fails to lex
          // against this compiler, which is worse than no page at all.
          //
          // `docs/reference/removed.md` is the bridge — what each 0.9.x
          // construct became — so a reader arriving from the old site is not
          // left guessing.
          path: 'docs',
          // `README.md` is the note explaining the tree, not a page.
          // Docusaurus routes a folder's README like its index, so it would
          // claim `/` — the slug `intro.md` declares — and the build would
          // answer "Duplicate routes found … non-deterministic routing
          // behavior".
          //
          // `exclude` REPLACES the plugin default rather than adding to it,
          // so the four default patterns are restated below.
          exclude: [
            '**/_*.{js,jsx,ts,tsx,md,mdx}',
            '**/_*/**',
            '**/*.test.{js,jsx,ts,tsx}',
            '**/__tests__/**',
            'README.md',
          ],
          sidebarPath: './sidebars.ts',
          routeBasePath: '/',          // serve docs at site root, no /docs/ prefix
          editUrl:
            'https://github.com/just-web-code/jwc-lang/edit/main/docs/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
        sitemap: {
          // `lastmod` from git history gives crawlers a real recrawl signal;
          // without it every entry looks equally stale.
          lastmod: 'date',
          changefreq: 'weekly',
          priority: 0.5,
        },
      } satisfies Preset.Options,
    ],
  ],

  // JSON-LD. Search engines have no other way to learn that "JWC" here is a
  // programming language and not the three-letter acronym they already rank.
  headTags: [
    {
      tagName: 'script',
      attributes: {type: 'application/ld+json'},
      innerHTML: JSON.stringify({
        '@context': 'https://schema.org',
        '@type': 'SoftwareApplication',
        name: 'JWC',
        alternateName: 'Just Web Code',
        applicationCategory: 'DeveloperApplication',
        operatingSystem: 'Linux, Windows, macOS',
        description:
          'A backend-first programming language with first-class HTTP routes, ' +
          'entities that compile to SQL, and Postgres execution.',
        url: 'https://jwc.1kb.uz',
        image: 'https://jwc.1kb.uz/img/jwc-social-card.png',
        codeRepository: 'https://github.com/just-web-code/jwc-lang',
        programmingLanguage: 'Rust',
        // No `license` / `offers` here on purpose: the repo carries no
        // LICENSE file yet (Cargo.toml: "workspace-private until a license
        // decision lands"), and structured data is a claim, not decoration.
      }),
    },
  ],

  themeConfig: {
    image: 'img/jwc-social-card.png',
    metadata: [
      {
        name: 'keywords',
        content:
          'jwc, jwc language, just web code, backend language, postgres, ' +
          'sql, http routes, entities, orm alternative, rest api, crud',
      },
      {name: 'twitter:card', content: 'summary_large_image'},
    ],
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
          href: 'https://github.com/just-web-code/jwc-lang',
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
            {label: 'Data (SQL)', to: '/data/schema'},
            {label: 'Backend', to: '/backend/routing'},
            {label: 'CLI reference', to: '/cli/'},
          ],
        },
        {
          title: 'Tools',
          items: [
            {label: 'Registry', href: 'https://registry-jwc.1kb.uz/'},
            {label: 'GitHub', href: 'https://github.com/just-web-code/jwc-lang'},
            {label: 'Roadmap', href: 'https://github.com/just-web-code/jwc-lang/blob/main/ROADMAP.md'},
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
