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

The newest version in the tree is **0.1.0**, and a `v*` tag is what publishes it — the streamer
image and the two npm packages ([README](README.md#consuming-appricot-the-released-artefacts)).
`main` is supported alongside it: a fix lands there and goes out in the next release, and there are
no older branches to backport it to. A report should name the version, or the commit, it applies
to.

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
