# Security policy

## Reporting a vulnerability

Report it privately, through GitHub's private vulnerability reporting: open the repository's
**Security** tab and choose **Report a vulnerability**
([GitHub's guide](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability),
checked 2026-09-23). The report reaches the maintainer and nobody else.

**Do not open a public issue, pull request or discussion** for a vulnerability.

Private vulnerability reporting is a repository setting, and GitHub offers it on public
repositories. It must be turned on when this repository is made public.

A useful report says:

- which component: the streamer, the wire codec, the browser client, the React bindings, or the
  dev tooling;
- the version or commit;
- what an attacker controls, and what they gain;
- the steps or the input that shows it.

## Supported versions

APPricot has not had a release. Every crate and package is `0.0.0`, and only `main` is
supported: a fix lands there, and there are no older branches to backport it to.

## What is in scope

[The threat model](docs/security/threat-model.md) says what APPricot defends and against whom.
Two rules shape what counts as a vulnerability:

- **The server is untrusted by the client**
  ([ADR-0003](docs/adr/0003-untrusted-server-client.md), Proposed). The streamer shares the
  streamed application's sandbox, so the client treats it as compromised with it. Any way for a
  server message to become markup, script, a URL or a navigation in the host page, or to make
  the client allocate without a bound, is a vulnerability.
- **The streamer listens on loopback or a unix socket only**, and serves nothing before the
  stream token. A way to reach it from another address, or without the token, is a
  vulnerability.

The streamed application itself, and anything it does inside its own sandbox, is not APPricot's
to fix.
