# Versioned documentation

> [!NOTE]
> These are inherited official-upstream Herdr snapshots. The Herdr Extended build/promote workflow does not create or publish versioned documentation.

This directory contains immutable documentation snapshots for stable Herdr releases.

Do not edit snapshot files manually. They must match the release tag recorded in `manifest.json`. Validate them with:

```bash
node website/scripts/docs-versions.mjs check
```

Official upstream release CI creates snapshots from tagged `docs/next` content. `website/scripts/prepare-docs.mjs` renders them at `/docs/<version>/` while keeping `/docs/` on upstream stable documentation and `/docs/preview/` on unreleased upstream work.

The historical backfill starts at v0.5.11, the first release that included the Astro/Starlight documentation site.
