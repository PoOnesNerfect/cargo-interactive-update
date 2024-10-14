# cargo-interactive-update

Update your direct dependencies interactively to the latest version via crates.io

## Installation

Install the cargo extension by installing it from crates.io:

```bash
cargo install cargo-interactive-update
```

## Usage

Run the cargo extension:

```bash
cargo interactive-update
```

It will then parse the `Cargo.toml` file to get the direct dependencies and check them via crates.io.

If there are outdated dependencies, it will display them and let you select which ones to update, similar to the following:

```
4 out of the 5 direct dependencies are outdated

Dependencies (1 selected):
● crossterm   2024-08-01 0.28.0  -> 2024-08-01 0.28.1   https://github.com/crossterm-rs/crossterm - A crossplatform terminal library for manipulating terminals.
○ curl        2022-07-22 0.4.44  -> 2024-09-30 0.4.47   https://github.com/alexcrichton/curl-rust - Rust bindings to libcurl for making HTTP requests
○ semver      2024-02-19 1.0.22  -> 2024-05-07 1.0.23   https://github.com/dtolnay/semver - Parser and evaluator for Cargo's flavor of Semantic Versioni
○ serde_json  2024-08-23 1.0.127 -> 2024-09-04 1.0.128  https://github.com/serde-rs/json - A JSON serialization file format


Use arrow keys to navigate, <a> to select all, <i> to invert, <space> to select/deselect, <enter> to update, <esc>/<q> to exit
```

After selecting the dependencies to update, it will run `cargo add` to update the dependencies.

## License

This project is licensed under the MIT license.