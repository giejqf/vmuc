#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# pagesize survey: runs common userspace inside a guest (as root) and
# prints "ok NAME" or "FAIL NAME: last line of output" per item, so a 4 KiB
# and a 16 KiB guest can be compared line by line (experiment E3).
# Installs packages and pulls container images: needs the network.
set -u
export DEBIAN_FRONTEND=noninteractive
r() {
	name=$1
	shift
	if out=$("$@" 2>&1); then
		echo "ok   ${name}"
	else
		echo "FAIL ${name}: $(printf '%s\n' "${out}" | tail -n 1)"
	fi
}
echo "page size $(getconf PAGESIZE)"
r "apt-get update" apt-get update -qq
r "apt-get install" apt-get install -y -qq --no-install-recommends gcc libc6-dev \
	python3-numpy nodejs golang-go redis-server postgresql jq ripgrep git docker.io
r "python3 stdlib" python3 -c "import ssl, sqlite3, json, hashlib"
r "numpy" python3 -c "import numpy; assert numpy.ones(1000).sum() == 1000"
r "gcc build and run" sh -c 'printf "int main(void){return 0;}" >/tmp/t.c && gcc -O2 /tmp/t.c -o /tmp/t && /tmp/t'
r "node" node -e "require('crypto').randomBytes(8)"
r "go build and run" sh -c 'cd /tmp && printf "package main\nfunc main(){}\n" >m.go && go run m.go'
r "redis-server (Ubuntu)" sh -c 'redis-server --port 6390 --save "" --daemonize yes && sleep 1 && redis-cli -p 6390 ping && redis-cli -p 6390 shutdown nosave'
r "postgresql (Ubuntu)" sh -c 'rm -rf /tmp/pg && mkdir /tmp/pg && chown postgres /tmp/pg && su postgres -c "/usr/lib/postgresql/16/bin/initdb -D /tmp/pg -A trust" >/dev/null && su postgres -c "/usr/lib/postgresql/16/bin/pg_ctl -D /tmp/pg -o \"-p 5439 -k /tmp\" -w start" >/dev/null && psql -h /tmp -p 5439 -U postgres -c "select 1" >/dev/null; s=$?; su postgres -c "/usr/lib/postgresql/16/bin/pg_ctl -D /tmp/pg -m fast stop" >/dev/null; exit $s'
r "jq" sh -c 'echo "{\"a\":1}" | jq .a'
r "ripgrep" rg --version
r "git" sh -c 'cd /tmp && rm -rf g && git init -q g && cd g && git -c user.name=x -c user.email=x@x commit -q --allow-empty -m x'
r "snap" snap version
r "docker info" docker info
img() {
	r "docker $1" docker run --rm "$1" sh -c "$2"
}
img alpine:3.20 "apk --version"
img debian:12-slim "apt-get --version >/dev/null"
img ubuntu:24.04 "apt-get --version >/dev/null"
img python:3.12-slim "python -c 'import json, ssl'"
img node:22-slim "node -e 1"
img golang:1.23-alpine "go version"
img redis:7 "redis-server --port 6391 --save '' --daemonize yes && sleep 1 && redis-cli -p 6391 ping"
img postgres:16 "su postgres -c 'initdb -D /tmp/pg -A trust >/dev/null' && echo initdb-ok"
img nginx:1.27 "nginx -t"
img mariadb:11 "mariadbd --version"
img rust:1-slim "rustc --version"
img eclipse-temurin:21-jre "java -version"
# Known risks on 16 KiB pages: allocators and runtimes built with a 4 KiB
# page compiled in (tcmalloc, jemalloc, JIT engines, bbolt).
img mongo:7 "mongod --dbpath /tmp --port 27018 --fork --logpath /tmp/log >/dev/null && mongosh --quiet --port 27018 --eval 'db.t.insertOne({a: 1}).acknowledged'"
img envoyproxy/envoy:v1.32-latest "envoy --version"
img clickhouse/clickhouse-server:24.8 "clickhouse local -q 'select 1'"
r "docker quay.io/coreos/etcd:v3.5.17" docker run --rm --entrypoint /usr/local/bin/etcd quay.io/coreos/etcd:v3.5.17 --version
img denoland/deno:2.1.4 "deno eval 1"
img oven/bun:1 "bun -e 1"
img ghcr.io/astral-sh/uv:python3.12-bookworm-slim "uv --version"
