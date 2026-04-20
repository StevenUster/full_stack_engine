#!/bin/bash

# Exit on error
set -e

# Extract version from framework/Cargo.toml
VERSION=$(grep "^version =" framework/Cargo.toml | cut -d '"' -f 2)

if [ -z "$VERSION" ]; then
    echo "Error: Could not find version in framework/Cargo.toml"
    exit 1
fi

echo "🚀 Building version: $VERSION"

# Build the images using podman
podman build \
    -t ghcr.io/stevenuster/full_stack_engine:latest \
    -t ghcr.io/stevenuster/full_stack_engine:$VERSION .

echo "✅ Build successful. Pushing images to GHCR..."

# Push the images
podman push ghcr.io/stevenuster/full_stack_engine:latest
podman push ghcr.io/stevenuster/full_stack_engine:$VERSION

echo "🎉 Successfully built and pushed version $VERSION"
