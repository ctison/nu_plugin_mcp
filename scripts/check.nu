#!/usr/bin/env nu

# Runs one Cargo step and converts a non-zero exit code into a Nushell error.
def run-step [label: string, arguments: list<string>] {
  print $"==> ($label)"
  ^cargo ...$arguments
  if $env.LAST_EXIT_CODE != 0 {
    error make {msg: $"($label) failed"}
  }
}

# Runs formatting, linting, compilation, and all tests for the plugin.
def main [] {
  run-step "format" [fmt --check]
  run-step "clippy" [clippy --all-targets --all-features -- -D warnings]
  run-step "build" [build]
  run-step "test" [test --all-targets]
}

