# herdr website

> [!NOTE]
> This is inherited official-upstream website tooling. Herdr Extended does not publish versioned documentation during its release workflow; it builds and promotes retained binaries, then writes only `website/preview.json`.

The homepage is `index.html`. The documentation source is in `src/content/docs/` and is rendered by Astro Starlight.

```bash
bun install
bun run dev
bun run build
```

The build output is `dist/`. Configure Cloudflare Pages to use `website` as the project root and publish `dist`.

For official upstream Herdr, stable docs live in `src/content/docs/`, unreleased docs live in `../docs/next/website/src/content/docs/`, and immutable release snapshots live in `../docs/versions/`.

The following official-upstream command is not part of Herdr Extended release automation:

```bash
node website/scripts/docs-versions.mjs publish <tag>
```

It snapshots tagged upstream docs and promotes the same tagged content to stable before the upstream website deploy. Use `node website/scripts/docs-versions.mjs check` only when maintaining those inherited snapshots.
