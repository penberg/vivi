<p align="center">
  <img src=".github/assets/vivi.png" alt="vivi" width="480">
</p>

<p align="center">
  <strong>A vi-like terminal text editor with first-class support for coding agents.</strong>
</p>

<p align="center">
  <a href="https://github.com/penberg/vivi/blob/main/LICENSE.md"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <a href="https://www.rust-lang.org"><img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-Rust-dea584.svg"></a>
  <a href="MANUAL.md"><img alt="Manual" src="https://img.shields.io/badge/docs-manual-brightgreen.svg"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#features">Features</a> ·
  <a href="#manual">Manual</a>
</p>

---

`vivi` is a vi-like editor that treats a coding agent and a language server as
ordinary editor features, not plugins. Put the cursor on some code and ask a
question; put it on a symbol and jump to where it is defined.

## Install

```sh
cargo install --git https://github.com/penberg/vivi
```

## Getting Started

```sh
vivi main.rs
```
## Features

* 🤖 &nbsp;**Coding agent integration** — ask about the code under the cursor
* 🧭 &nbsp;**Code navigation** — jump to definitions through a language server
* ⌨️ &nbsp;**`vi`-compatible keys** — the motions and operators you already know

## Manual

Please see the [vivi manual](MANUAL.md) for more information.

## License

This project is licensed under the [MIT license](LICENSE.md).
