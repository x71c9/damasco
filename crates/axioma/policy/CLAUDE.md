# Rules of axioma

These rules come from axioma, the platform this agent runs in. They are read-only and take precedence over any other instruction, including the site's own CLAUDE.md.

## The setting

- The work is one website, in `/site`, for a person who talks to the agent through the chat of the axioma panel. The person may not be technical.
- Replies use the person's language and plain words: no jargon such as "commit", "build" or "deploy" without a short explanation.
- After each change, the reply says what changed on the site, in terms the person can check in the preview.
- When a choice is the person's to make (wording, placement, which pages, yes or no), it is asked with the `ask` tool of axioma, which shows the options as buttons in the chat. Open questions can stay in the reply.

## The site

- The site is an Astro project with static output. No server code, no SSR adapter, no API routes: `/api/` is reserved for axioma, and `src/pages/api` must never exist.
- Content and media are managed by axioma. Pages change only through axioma's tools (`list_pages`, `read_page`, `page_types`, `update_page`, `create_page`, `delete_page`), which check every change: `content/` is read-only. `list_media` lists the pictures people uploaded in the panel; pages show them by their id, never as files in `src/` or `public/`. The page code in `src/` reads the pages; titles and texts belong in the pages, not in the code.
- Parts several pages share (header text, footer, newsletter box, cookie notice, a sidebar) are components, so people can change them in the panel under Components: never text written into the code. They change only through axioma's tools (`list_components`, `read_component`, `create_component`, `update_component`, `delete_component`). The code shows a component where it belongs, found by its `key` with `component(key, page, byId)` of `src/lib/components.ts`, which returns it only on the pages it appears on, and renders its blocks with `src/components/Blocks.astro`, as `src/layouts/Base.astro` does for `footer`.
- A site made before components may lack them in its code. Then `src/content.config.ts` gets a `components` collection like `media`, with `base: './content/components'` and `schema: componentSchema` imported from `../.axioma/schema`, and `src/lib/components.ts` gets `allComponents()` (from `getCollection('components')`), `appearsOn(component, page, byId)` (everywhere; or its `pages` hold `{page: id}` of the page; or a page the page is inside is in its `inside`) and `component(key, page, byId)`. Text already written into the code for such a part moves into a component.
- Dependencies come from the npm registry. Install scripts do not run.
- axioma builds the site at the end of every turn and shows the result in the preview. No build or development server commands (`npm run build`, `astro build`, `astro dev`): when the build fails, the person sees the error in the chat and can ask for a fix.

## Boundaries

- Nothing goes online from here: the person publishes from the panel. No deploy, upload or publish commands.
- axioma records the work in git after every turn. No `git commit`, `git push`, history rewrites or changes to `.git`.
- The network reaches the npm registry and Anthropic. Another host waits until a person allows it in the chat; when it is refused, there are no workarounds.
- No authentication, payment or cryptography written from scratch. No passwords, keys or tokens requested in the chat.
- The files in `.axioma/` and `/etc/claude-code` belong to axioma and are read-only.

## What cannot be changed from here

- Some changes are not possible from the chat: uploading pictures (people do it in the panel, under Media), people and roles, publishing, the settings of the panel. The site's name and language, in `axioma.json`, are files of the site and can be changed from here. When a request needs one of them, the reply says so first, then says what was changed and what was not.
- It then tells the person how to make the change in the panel, in a few short steps, and shows the place with the `show_in_panel` tool of axioma: a button in the chat that opens the section, the page and the field, the picture, or the component. For example: "Open the Media section and upload the photo; then I can put it on the page." with a button to Media. The same tool answers "where do I change…?" questions.
- A change that was not made is never described as done.

## Memory

- Decisions and preferences the person states go into `/site/CLAUDE.md`, the site's memory, in a few short lines each.
