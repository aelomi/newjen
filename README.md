# Newjen

Newjen is a native macOS agent development environment built with Rust and GPUI. It runs Claude Code and Codex in parallel threads, in your project or isolated git worktrees. Approve their tools, review their changes, and land their work from one window.

## Install

On an Apple Silicon Mac with macOS 11 Big Sur or later, run these commands once.

```sh
brew tap aelomi/newjen https://github.com/aelomi/newjen
brew trust --tap aelomi/newjen
brew install --cask aelomi/newjen/newjen
```

Homebrew 7 requires explicit trust for third-party taps, including URL taps. Older Homebrew versions without `brew trust` can skip that command.
The tap includes pre-releases. Open Newjen from Applications after installation.
Install and sign in to Claude Code or Codex separately to use that provider.

Newjen is unsigned by Apple and is not notarized. It has an ad-hoc signature.
The cask removes quarantine on install and upgrade so Gatekeeper does not block the downloaded app.
Trust the tap only if you trust its code and releases.

## Update

Quit Newjen, then run these commands.

```sh
brew update
brew upgrade --cask aelomi/newjen/newjen
```

## Uninstall

Uninstall preserves your threads and settings.

```sh
brew uninstall --cask aelomi/newjen/newjen
```

Add `--zap` to also remove the default data folder, `~/Library/Application Support/Newjen`, and macOS state.
Custom `NEWJEN_HOME` folders, projects, git worktrees (the `.newjen-worktrees` folder beside your repository), and agent CLI data are left alone.

See [CONTRIBUTING.md](CONTRIBUTING.md) to build from source.

## License

MIT. See [`LICENSE`](LICENSE).
