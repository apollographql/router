#!/usr/bin/env python3
import os
import toml

# Caminho base: estamos rodando de dentro de cloned-router/custom-plugin
ROOT_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ROUTER_DIR = os.path.join(ROOT_DIR, "apollo-router")
DOCKERFILE_PATH = os.path.join(ROOT_DIR, "dockerfiles", "diy", "dockerfiles", "Dockerfile.repo")
WORKSPACE_TOML = os.path.join(ROOT_DIR, "Cargo.toml")
TOOLCHAIN_FILE = os.path.join(ROOT_DIR, "rust-toolchain.toml")
CONFIG_TOML = os.path.join(ROUTER_DIR, "config.toml")

def patch_workspace():
    with open(WORKSPACE_TOML, "r") as f:
        data = toml.load(f)

    members = set(data.get("workspace", {}).get("members", []))
    members.add("custom-plugin")
    members.add("custom-plugin/router-with-plugin")
    data["workspace"]["members"] = sorted(members)

    with open(WORKSPACE_TOML, "w") as f:
        toml.dump(data, f)

    print("✅ Patched workspace Cargo.toml")

def patch_rust_version():
    rust_version = os.environ.get("RUST_VERSION", "1.90.0")

    if os.path.exists(TOOLCHAIN_FILE):
        lines = []
        with open(TOOLCHAIN_FILE, "r") as f:
            for line in f:
                if line.strip().startswith("channel"):
                    lines.append(f'channel = "{rust_version}"\n')
                else:
                    lines.append(line)
        with open(TOOLCHAIN_FILE, "w") as f:
            f.writelines(lines)

    if os.path.exists(CONFIG_TOML):
        lines = []
        with open(CONFIG_TOML, "r") as f:
            for line in f:
                if line.strip().startswith("rust = "):
                    lines.append(f'rust = "{rust_version}"\n')
                else:
                    lines.append(line)
        with open(CONFIG_TOML, "w") as f:
            f.writelines(lines)

    print(f"✅ Patched Rust version to {rust_version}")

def patch_dockerfile():
    if not os.path.exists(DOCKERFILE_PATH):
        print("⚠️ Dockerfile.repo not found, skipping")
        return

    rust_version = os.environ.get("RUST_VERSION", "1.90.0")

    lines = []
    for_dockerfile = []
    with open(DOCKERFILE_PATH, "r") as f:
        for line in f:
            stripped = line.strip()
            if stripped.startswith("FROM rust:"):
                lines.append(f"FROM rust:{rust_version} as build\n")
            elif "cargo install --path" in stripped:
                lines.append("RUN cargo install --path custom-plugin/router-with-plugin\n")
            else:
                lines.append(line)

    # Garante ARG CARGO_BIN
    if not any("ARG CARGO_BIN" in l for l in lines):
        for i, l in enumerate(lines):
            if l.strip().startswith("FROM") and "as build" in l:
                lines.insert(i + 1, "ARG CARGO_BIN=custom-plugin/router-with-plugin\n")
                break

    with open(DOCKERFILE_PATH, "w") as f:
        f.writelines(lines)

    print("✅ Patched Dockerfile.repo (Rust version + CARGO_BIN)")

if __name__ == "__main__":
    patch_workspace()
    patch_rust_version()
    patch_dockerfile()