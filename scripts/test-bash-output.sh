#!/usr/bin/env bash

set -euo pipefail

iterations="${1:-10}"

for ((iteration = 1; iteration <= iterations; iteration++)); do
    printf 'Iteration: %d\n' "$iteration"
    for ((repeat = 1; repeat <= 10; repeat++)); do
        printf '%d\n' "$iteration"
    done
    printf 'Sleeping for 1s\n'
    sleep 1
done
