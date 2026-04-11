# superhq.ai

Marketing site for [SuperHQ](https://github.com/superhq-ai/superhq). Built with [Astro](https://astro.build).

## Develop

```sh
bun install
bun run dev
```

Open http://localhost:4321.

## Build

```sh
bun run build
```

Static output is written to `dist/`.

## Deploy

Deployed automatically via GitHub Actions on pushes to `main` that touch `website/**`.
See `.github/workflows/deploy-website.yml` at the repo root.

The custom domain `superhq.ai` is configured via `public/CNAME`.

## Content sources

All copy on this site is sourced from the repo root `README.md` and `Cargo.toml` — no
invented claims, no placeholder marketing copy. If a fact isn't documented there, it's
not on the site.
