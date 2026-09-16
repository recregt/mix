#!/usr/bin/env bash
set -euo pipefail

version="${1:?usage: bump-nix.sh <nix-version> [target ...]}"
shift
if [ "$#" -eq 0 ]; then
    targets=(x86_64-linux aarch64-linux)
else
    targets=("$@")
fi

nixpkgs_rev="${NIXPKGS_REV:-}"
if [ -z "$nixpkgs_rev" ]; then
    echo "resolving latest nixos-unstable" >&2
    nixpkgs_rev="$(git ls-remote https://github.com/NixOS/nixpkgs.git refs/heads/nixos-unstable | cut -f1)"
fi

home_manager_rev="${HOME_MANAGER_REV:-}"
if [ -z "$home_manager_rev" ]; then
    echo "resolving latest home-manager master" >&2
    home_manager_rev="$(git ls-remote https://github.com/nix-community/home-manager.git refs/heads/master | cut -f1)"
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pins_file="$script_dir/../src/lib.rs"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

echo "pub const NIX_VERSION: &str = \"$version\";" > "$tmp_dir/pins.rs"
echo "" >> "$tmp_dir/pins.rs"
echo "pub const NIXPKGS_REV: &str = \"$nixpkgs_rev\";" >> "$tmp_dir/pins.rs"
echo "pub const HOME_MANAGER_REV: &str = \"$home_manager_rev\";" >> "$tmp_dir/pins.rs"
echo "" >> "$tmp_dir/pins.rs"
cat >> "$tmp_dir/pins.rs" <<'EOF'
pub struct TarballPin {
    pub target: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

pub const NIX_TARBALLS: &[TarballPin] = &[
EOF

for target in "${targets[@]}"; do
    url="https://releases.nixos.org/nix/nix-$version/nix-$version-$target.tar.xz"
    file="$tmp_dir/nix-$version-$target.tar.xz"
    echo "fetching $url" >&2
    curl -fsSL -o "$file" "$url"
    hash="$(sha256sum "$file" | awk '{print $1}')"
    cat >> "$tmp_dir/pins.rs" <<EOF
    TarballPin {
        target: "$target",
        url: "$url",
        sha256: "$hash",
    },
EOF
done

cat >> "$tmp_dir/pins.rs" <<'EOF'
];

pub fn pin_for(target: &str) -> Option<&'static TarballPin> {
    NIX_TARBALLS.iter().find(|p| p.target == target)
}
EOF

cp "$tmp_dir/pins.rs" "$pins_file"
echo "wrote $pins_file for Nix $version" >&2
