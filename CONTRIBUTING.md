# Contributing to Fastpotify

Fastpotify is a native Navidrome/OpenSubsonic client. Changes should improve
the desktop app without adding a browser, fallback catalogues, or a hosted
Fastpotify service.

## Before opening an issue

Search open and closed issues first. For a bug, use the bug form and include
the requested log and exact steps to reproduce it. Reports without enough
information to investigate may be closed.

For a feature, explain the user problem. Discuss large changes in an issue
before writing code. Existing code does not guarantee that a feature fits the
project.

Some boundaries come from OpenSubsonic, Navidrome, and the local audio stack:

- Music and artwork must come from the server the listener signed into.
  Substituting media from YouTube, Piped, `yt-dlp`, or another catalogue is
  out of scope.
- Do not claim gapless playback, normalization, lossless output, or codec
  support unless Fastpotify's complete streaming and output pipeline supports
  and tests it.
- Server credentials and salted request authentication must never enter logs,
  public media references, artwork cache keys, or desktop-control metadata.
- Fastpotify will not embed a browser engine, add telemetry, or introduce a
  Fastpotify-operated service.

Duplicate, out-of-scope, or incomplete issues may be closed with a short
explanation.

## Design principles

1. **Native and fast.** Startup time, idle work, memory use, and binary size
   are product features. Keep the UI thread free of network and disk waits.
2. **Focused.** Prefer a complete, coherent workflow over a collection of
   settings, modes, and speculative features.
3. **Honest integrations.** Use the documented OpenSubsonic API for what the
   active server supports. Do not scrape private endpoints, invent remote
   playback semantics, or silently replace one media source with another.
4. **Cross-platform by default.** Linux, macOS, and Windows are supported
   products. Platform-specific code must be isolated and the other targets
   must keep compiling.
5. **Small dependency surface.** Reuse the standard library and existing
   crates where practical. A new dependency needs a concrete benefit worth
   its build time, binary size, maintenance, and security cost.
6. **Visible failure, private data.** Errors should be actionable, rate limits
   should be respected, and credentials must never appear in logs. Network
   behaviour belongs in the documentation.

## Pull requests

Keep each pull request to one change. Explain why it belongs in Fastpotify,
what changed, and how you tested it. Avoid unrelated formatting, refactors,
generated prose, and large mechanical rewrites.

The same rules apply to hand-written and AI-assisted changes. The author must
understand every line and answer review comments with specific reasoning.

Code changes should include tests for behaviour that can regress. UI changes
should include before/after screenshots or a short recording and should use
demo mode where possible. User-visible behaviour, settings, files, or network
access must be documented in the same pull request.

Run the same checks CI runs before submitting:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo test --locked --all-targets --all-features
cargo test --locked --all-features --doc
RUSTDOCFLAGS='-D warnings' cargo doc --locked --all-features --no-deps
(cd docs && bundle exec jekyll build)
```

Linux needs the development packages listed in the README; `nix develop`
provides the complete development environment. CI repeats the test suite on
Linux, macOS, and Windows. Passing CI is required, but does not replace review
for correctness, product fit, maintainability, or security.

By contributing, you agree that your contribution is licensed under the
project's MIT License.
