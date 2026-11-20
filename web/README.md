# Aside landing page concept

A responsive, single-page marketing site for **Aside**, the local-first PR review workspace.

## Preview

Run `pnpm web` from the repository root to preview the site at `http://127.0.0.1:4173/`, including the agent setup guide at `/agents.md`. The homepage can also be opened directly as `index.html`; styles, scripts and illustrations are included in the file. Before production deployment, replace the `https://asidedomain/agents.md` placeholder in the hero prompt with the actual production domain and serve `agents.md` at that domain's `/agents.md` path.

The embedded PR review illustration is interactive:

- Switch between Review prep, Files changed and Conversations.
- Ignore / re-include individual files and whole directories in the pre-review file tree.
- Mark a file reviewed; the mock remembers local choices using browser storage when supported.
- Reset the demo state.

The PR details, analysis signals and GitHub comments are example data only. This is a product landing-page concept, **not the shipped Aside CLI**. The setup prompt points to the separate agent guide; do not use its placeholder domain as a live URL.

## Design direction

- Short, direct product copy and an oversized editorial headline.
- Quiet cream background, dense dark-ink typography and restrained purple accents.
- Product example shown up front, followed by three essential differentiators and a simple explanation of the local-first CLI architecture.
- Mobile responsive, keyboard focus styles and reduced-motion support.

The built app's Svelte/Tailwind/shadcn-svelte technology requirement remains unchanged. This HTML is a dependency-free landing-page prototype and can be ported into the Svelte project.
