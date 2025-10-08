#!/usr/bin/env python3
import toml
import os

REPO_ROOT = "cloned-router"
CUSTOM_PLUGIN_PATH = "custom-plugin"
RUST_VERSION = os.environ.get("RUST_VERSION", "latest")

# --- Update Cargo.toml workspace members ---
cargo_toml_path = os.path.join(REPO_ROOT, "Cargo.toml")
cargo_data = toml.load(cargo_toml_path)

members = set(cargo_data.get("workspace", {}).get("members", []))
members.add("custom-plugin")
members.add("custom-plugin/router-with-plugin")
cargo_data["workspace"]["members"] = sorted(members)

with open(cargo_toml_path, "w") as f:
    toml.dump(cargo_data, f)
print("✅ Added custom-plugin and router-with-plugin to workspace members")

# --- Patch rust-toolchain.toml ---
rust_toolchain_path = os.path.join(REPO_ROOT, "rust-toolchain.toml")
if os.path.exists(rust_toolchain_path):
    lines = []
    with open(rust_toolchain_path) as f:
        for line in f:
            if line.strip().startswith("channel"):
                lines.append(f'channel = "{RUST_VERSION}"\n')
            else:
                lines.append(line)
    with open(rust_toolchain_path, "w") as f:
        f.writelines(lines)
    print(f"✅ Patched rust-toolchain.toml to use rust:{RUST_VERSION}")

# --- Patch apollo-router/config.toml ---
config_toml_path = os.path.join(REPO_ROOT, "apollo-router", "config.toml")
if os.path.exists(config_toml_path):
    lines = []
    with open(config_toml_path) as f:
        for line in f:
            if line.strip().startswith("rust ="):
                lines.append(f'rust = "{RUST_VERSION}"\n')
            else:
                lines.append(line)
    with open(config_toml_path, "w") as f:
        f.writelines(lines)
    print(f"✅ Patched apollo-router/config.toml to use rust:{RUST_VERSION}")

# --- Patch Dockerfile.repo ---
dockerfile_path = os.path.join(REPO_ROOT, "dockerfiles", "diy", "dockerfiles", "Dockerfile.repo")
if os.path.exists(dockerfile_path):
    lines = []
    with open(dockerfile_path) as f:
        for line in f:
            if line.strip().startswith("FROM rust:"):
                lines.append(f"FROM rust:{RUST_VERSION} as build\n")
            elif line.strip().startswith("RUN cargo install --path"):
                lines.append("RUN cargo install --path custom-plugin/router-with-plugin\n")
            else:
                lines.append(line)
    with open(dockerfile_path, "w") as f:
        f.writelines(lines)
    print(f"✅ Patched Dockerfile.repo to use rust:{RUST_VERSION}")