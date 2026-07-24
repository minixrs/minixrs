---
name: mdbook-preview
description: Install the pinned mdBook 0.5.3 prebuilt binary and build/serve the book/ docs locally with live reload
---

mdBook isn't installed by default. Install the **prebuilt** binary pinned to CI's 0.5.3
(`cargo install mdbook` compiles slowly from source) — for Apple Silicon:
`curl -fsSL https://github.com/rust-lang/mdBook/releases/download/v0.5.3/mdbook-v0.5.3-aarch64-apple-darwin.tar.gz | tar xz && mv mdbook ~/.cargo/bin/`.

Build: `mdbook build book` (output `book/book/` is gitignored).
Live preview with reload: `mdbook serve book -n 127.0.0.1 -p 3000`.
