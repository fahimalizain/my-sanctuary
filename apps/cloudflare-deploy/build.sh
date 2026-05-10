#!/bin/bash
set -e

VERSION=$(node -p "require('../../package.json').version")
echo "Building cloudflare-deploy v${VERSION}..."

rm -rf dist build .wrangler
cp -r ../../dist/apps/web ./dist
go run github.com/syumai/workers/cmd/workers-assets-gen@latest -mode go .
GOOS=js GOARCH=wasm go build -ldflags "-X main.version=${VERSION}" -o build/app.wasm .
