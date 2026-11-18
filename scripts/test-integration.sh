#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd "${script_dir}/.." && pwd)"
compose_file="${repo_dir}/compose.test.yaml"
compose_project="channel-bot-test"
test_postgres_port="${TEST_POSTGRES_PORT:-55432}"

export TEST_POSTGRES_PORT="${test_postgres_port}"
export TEST_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:${test_postgres_port}/postgres"

cleanup() {
  docker compose --project-name "${compose_project}" -f "${compose_file}" down --volumes
}
trap cleanup EXIT

docker compose --project-name "${compose_project}" -f "${compose_file}" up --detach --wait postgres
cargo test --locked --manifest-path "${repo_dir}/Cargo.toml" --test integration "$@"
