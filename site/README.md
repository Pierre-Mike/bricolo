# Project site

The one-page site for bricolo, published to GitHub Pages at https://pierre-mike.github.io/bricolo/. It is plain HTML, CSS and JavaScript with no build step and no external requests.

The `pages` workflow (`.github/workflows/pages.yml`) copies this folder and `docs/screenshots/` into one artifact and deploys it on every push to `main` that touches either. To turn it on once, open the repository's **Settings**, then **Pages**, and set **Source** to **GitHub Actions**.

To preview it locally:

```sh
just site      # http://localhost:8080
```
