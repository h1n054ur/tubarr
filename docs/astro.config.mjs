// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// The site is a project Pages site, served at https://h1n054ur.github.io/tubarr/, so every page
// lives under /tubarr.
const owner = 'h1n054ur';
const repo = 'tubarr';

export default defineConfig({
	site: `https://${owner}.github.io`,
	base: `/${repo}`,
	integrations: [
		starlight({
			title: 'Tubarr',
			logo: {
				light: './src/assets/tubarr-logo-horizontal-light-notagline.svg',
				dark: './src/assets/tubarr-logo-horizontal-dark-notagline.svg',
				alt: 'Tubarr',
				replacesTitle: true,
			},
			customCss: ['./src/styles/brand.css'],
			description: 'YouTube as a source for Sonarr and Radarr, played in Jellyfin without storing a thing.',
			social: [{ icon: 'github', label: 'GitHub', href: `https://github.com/${owner}/${repo}` }],
			editLink: { baseUrl: `https://github.com/${owner}/${repo}/edit/main/docs/` },
			sidebar: [
				{
					label: 'Getting started',
					items: ['getting-started/install', 'getting-started/quick-start', 'getting-started/first-run'],
				},
				{
					label: 'Setup',
					items: ['setup/configuration', 'setup/sonarr-and-radarr', 'setup/prowlarr', 'setup/jellyfin'],
				},
				{
					label: 'Using Tubarr',
					items: ['using/mappings-and-matching', 'using/podcasts', 'using/pins-and-unmatched'],
				},
				{
					label: 'Operations',
					items: ['operations/sizing', 'operations/monitoring', 'operations/where-to-run', 'operations/backups-and-upgrades', 'operations/troubleshooting'],
				},
				{
					label: 'Reference',
					items: ['reference/admin-api'],
				},
				{
					label: 'About',
					items: ['about/faq', 'about/privacy', 'about/terms'],
				},
			],
		}),
	],
});
