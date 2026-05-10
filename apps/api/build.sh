#!/bin/bash
set -e

VERSION=$(node -p "require('../../package.json').version")
echo "Building api v${VERSION}..."

CGO_ENABLED=0 go build -ldflags "-X main.version=${VERSION}" -o ../../dist/apps/api .
