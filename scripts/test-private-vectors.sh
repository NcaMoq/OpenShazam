#!/bin/sh
set -eu

if [ -z "${SHZ_PRIVATE_VECTOR_ROOT:-}" ]; then
  printf '%s\n' 'Set SHZ_PRIVATE_VECTOR_ROOT to the authorized research root containing vectors/.' >&2
  exit 2
fi

cargo test -p sigx-format --test private_oracle -- --ignored
