// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// https://astro.build/config
export default defineConfig({
	integrations: [
		starlight({
			title: 'Cagent',
			customCss: ['./src/styles/main.css'],
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/Cretezy/cagent' }],
			sidebar: [
				{
					label: 'Start here',
					items: [
						{ label: 'Getting started', slug: 'getting-started' },
						{ label: 'Configuration', slug: 'configuration' },
					],
				},
				{
					label: 'Features',
					items: [
						{ label: 'Providers and models', slug: 'providers-and-models' },
						{ label: 'Modes & Planning', slug: 'modes' },
						{ label: 'Agents and delegation', slug: 'agents' },
						{ label: 'Permissions', slug: 'permissions' },
						{ label: 'Instructions', slug: 'instructions' },
						{ label: 'Skills', slug: 'skills' },
						{ label: 'MCP', slug: 'mcp' },
						{ label: 'Search and fetch', slug: 'search-and-fetch' },
						{ label: 'Composer', slug: 'composer' },
						{ label: 'Interactive reference', slug: 'interactive-reference' },
						{ label: 'Tools', slug: 'tools' },
						{ label: 'Conversations', slug: 'conversations' },
						{ label: 'Context and compaction', slug: 'context-and-compaction' },
						{ label: 'Files and diffs', slug: 'files-and-diffs' },
						{ label: 'Shell and background work', slug: 'shell-and-background-work' },
						{ label: 'Status line', slug: 'status-line' },
						{ label: 'Customize the interface', slug: 'customize-interface' },
						{ label: 'Usage and costs', slug: 'usage-and-costs' },
					],
				},
				{
					label: 'Guides',
					items: [
						{ label: 'Worktrees', slug: 'worktrees' },
						{ label: 'Headless execution', slug: 'headless' },
						{ label: 'ACP integrations', slug: 'acp' },
						{ label: 'CLI arguments', slug: 'cli-arguments' },
						{ label: 'Troubleshooting', slug: 'troubleshooting' },
					],
				},
			],
		}),
	],
});
