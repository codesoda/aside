# Aside landing page concept

A responsive, single-page marketing site for **Aside**, the local-first PR review workspace.

## Preview

Run `pnpm web` from the repository root to preview the site at `http://127.0.0.1:4173/`, including the agent setup guide at `/agents.md`. The homepage can also be opened directly as `index.html`; styles, scripts and illustrations are included in the file. The hero prompt points to `https://asidecode.dev/agents.md`; deploy `agents.md` at that path before publishing the site.

The embedded PR review illustration is interactive:

- Switch between Review prep, Files changed and Conversations.
- Ignore / re-include individual files and whole directories in the pre-review file tree.
- Mark a file reviewed; the mock remembers local choices using browser storage when supported.
- Reset the demo state.

The PR details, analysis signals and GitHub comments are example data only. This is a product landing-page concept, **not the shipped Aside CLI**. The setup prompt points to the separate agent guide on asidecode.dev; make sure it is deployed before sharing the prompt.

## Deploy on GitHub Pages

`.github/workflows/pages.yml` uploads the static `web/` directory and deploys it on pushes to `main` that change `web/` (or the workflow itself). You can also run it manually. Pull requests do not deploy.

One-time repository setup is required before the first deployment: in **Settings → Pages**, select **GitHub Actions** as the build source and set the custom domain to `asidecode.dev`. Verify ownership of the domain and configure the apex DNS records according to [GitHub's custom-domain instructions](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site); enable HTTPS once GitHub offers it. A custom Actions workflow does not use a `CNAME` file to configure the domain. Until the Pages source, domain, and DNS are configured, the workflow alone does not make `https://asidecode.dev/agents.md` reachable.

## Design direction

- Short, direct product copy and an oversized editorial headline.
- Quiet cream background, dense dark-ink typography and restrained purple accents.
- Product example shown up front, followed by three essential differentiators and a simple explanation of the local-first CLI architecture.
- Mobile responsive, keyboard focus styles and reduced-motion support.
- Light and dark colour schemes follow the visitor's `prefers-color-scheme` setting (no toggle). Colours come from semantic tokens in the `:root` block; the names match the app's tokens in `app/src/app.css` so the two can be kept in step.

The built app's Svelte/Tailwind/shadcn-svelte technology requirement remains unchanged. This HTML is a dependency-free landing-page prototype and can be ported into the Svelte project.
