#!/usr/bin/env bash
set -euo pipefail

version="${1:?usage: bump-nix.sh <nix-version> [target ...]}"
shift
if [ "$#" -eq 0 ]; then
    targets=(x86_64-linux aarch64-linux)
else
    targets=("$@")
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pins_file="$script_dir/../src/bootstrap/pins.rs"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

echo "pub const NIX_VERSION: &str = \"$version\";" > "$tmp_dir/pins.rs"
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
