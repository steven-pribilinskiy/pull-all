// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// Project site served from a subpath on GitHub Pages.
export default defineConfig({
  site: 'https://steven-pribilinskiy.github.io',
  base: '/pull-all',
  integrations: [
    starlight({
      title: 'pull-all',
      description:
        'Interactive multi-repo git pull dashboard — a Rust/ratatui TUI that pulls every repo in a directory in parallel.',
      // Show each page's git last-modified date in the footer — a visible staleness signal when
      // a page lags behind code churn. Needs full git history (fetch-depth: 0) in the deploy.
      lastUpdated: true,
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/steven-pribilinskiy/pull-all',
        },
      ],
      customCss: ['./src/styles/custom.css'],
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'What is pull-all?', slug: 'index' },
            { label: 'Installation', slug: 'start/installation' },
            { label: 'Usage', slug: 'start/usage' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Keybindings', slug: 'guides/keybindings' },
            { label: 'Repo page & diff modal', slug: 'guides/repo-page' },
            { label: 'Columns & glyphs', slug: 'guides/columns-and-glyphs' },
            { label: 'Repo groups', slug: 'guides/groups' },
            { label: 'Directory tree', slug: 'guides/tree-view' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'CLI flags & env', slug: 'reference/cli' },
            { label: 'Exit codes', slug: 'reference/exit-codes' },
            { label: 'Sibling builds', slug: 'reference/siblings' },
            { label: 'Architecture', slug: 'reference/architecture' },
          ],
        },
      ],
    }),
  ],
});
