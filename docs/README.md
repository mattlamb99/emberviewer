# emberviewer docs site

This folder is a small, self-contained static website for **emberviewer**, designed to be
served by **GitHub Pages**. It has no build step and no external dependencies — plain HTML
and CSS that also work when opened directly from disk.

## Files

| File | Purpose |
|------|---------|
| `index.html` | Landing page: hero, why, features, screenshots, getting started, docs link, footer. |
| `protocol.html` | A primer on the Ember+ protocol and how emberviewer maps to it. |
| `protocol.md` | The same primer as Markdown (renders nicely on GitHub; the HTML version is what the site links to). |
| `style.css` | All site styling. Orange Ember+ accent (`#d9772b`), light theme with `prefers-color-scheme` dark support. |
| `README.md` | This file. |

The favicon and logo are inline SVG / data-URIs — there are **no external CDN requests**, so the
site works fully offline and ships no trackers.

## Enabling GitHub Pages

1. Push this `docs/` folder to the **`main`** branch.
2. On GitHub, open the repository's **Settings → Pages**.
3. Under **Build and deployment → Source**, choose **"Deploy from a branch"**.
4. Set **Branch** to **`main`** and the folder to **`/docs`**, then click **Save**.
5. Wait a minute for the first deploy. The site appears at
   `https://<owner>.github.io/<repo>/` (for the placeholder repo, `https://m-l2.github.io/emberviewer/`).

## Custom domain (optional)

GitHub Pages can serve from your own domain. To use one:

1. In **Settings → Pages → Custom domain**, enter the domain (e.g. `emberviewer.dev`). GitHub
   writes a `CNAME` file into this folder for you.
2. Add the matching DNS records at your registrar (a `CNAME` record pointing to
   `<owner>.github.io`, or `A`/`AAAA` records to GitHub's Pages IPs for an apex domain).
3. Enable **Enforce HTTPS** once the certificate is provisioned.

No `CNAME` file is included here — add one only if you set up a custom domain.

## Updating placeholders

The links currently point at the placeholder repository **`m-l2/emberviewer`**. Search and
replace `m-l2/emberviewer` (and the Pages URL `m-l2.github.io/emberviewer`) with the real
owner/repo once it is known. The **Download** button and Releases links target
`https://github.com/<owner>/<repo>/releases`.
