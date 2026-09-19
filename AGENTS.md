# AGENTS.md

Instructions for any coding agent working in this repository.

**Read [CLAUDE.md](CLAUDE.md) first.** It is the full contract and it is not Claude-specific:
what APPricot is, how to run every gate, and the rules that a change must not break.

The short version, so nothing is broken by accident:

- **Everything runs in Docker.** Never run `cargo`, `pnpm`, `node` or `just` on the host. The gate
  is `docker compose build dev` then `docker compose run --rm dev just check`. Use the host
  `docker compose` CLI only.
- **No host port is published.** Do not add one.
- **The server is untrusted.** The browser client renders every server string as text, never as
  markup, script or navigation. `eslint.config.js` enforces this and
  `packages/client/src/untrusted-server.lint.test.ts` proves the rules fire. Do not weaken a rule.
- **Licences: the project is MIT OR Apache-2.0**, and every linked dependency is on the
  permissive allow list of ADR-0002 §2 (`deny.toml`).
- **No X11 or Wayland type outside `crates/appricot-x11`.** The window model in `appricot-core` is
  backend-agnostic and Wayland-shaped.
- **No borrowed names, no borrowed paths.** This repository names no customer, vendor or sibling
  project, and no path into another checkout. Public prior art and dependencies are named plainly,
  with a checked URL. See CLAUDE.md, "Hard rule: no borrowed names, no borrowed paths" — a
  reviewer greps the diff before a commit.
- **Nothing outside this repository is edited from here.**
- **Do not commit unless asked.** There is no remote.

Documents live under [docs/](docs/); decisions are ADRs under [docs/adr/](docs/adr/README.md), and
an Accepted ADR outranks every other document.
