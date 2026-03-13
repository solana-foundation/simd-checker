build-programs:
    #!/usr/bin/env bash
    set -euo pipefail
    for dir in tests/*/; do
        if [ -f "$dir/Cargo.toml" ]; then
            echo "Building SBF program: $dir"
            cargo-build-sbf --tools-version v1.52 --manifest-path "$dir/Cargo.toml"
        fi
    done

run *ARGS: build-programs
    cargo run -p runner -- {{ARGS}}
