#!/usr/bin/env bash
set -euo pipefail

version="${1:?usage: bump-nix.sh <nix-version> [target ...]}"
shift
if [ "$#" -eq 0 ]; then
    targets=(x86_64-linux aarch64-linux)
else
    targets=("$@")
fi

inputs=(
    "NIXPKGS NixOS nixpkgs nixos-unstable"
    "HOME_MANAGER nix-community home-manager master"
)

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pins_file="$script_dir/../src/lib.rs"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

answered() {
    nix --extra-experimental-features nix-command eval --raw --impure --expr "toString (builtins.fromJSON (builtins.readFile $1)).$2"
}

echo "pub const NIX_VERSION: &str = \"$version\";" > "$tmp_dir/pins.rs"
echo "" >> "$tmp_dir/pins.rs"

for input in "${inputs[@]}"; do
    read -r name owner repo branch <<< "$input"
    override="${name}_REV"
    reference="github:$owner/$repo/${!override:-$branch}"
    echo "resolving $reference" >&2
    answer="$tmp_dir/$name.json"
    nix --extra-experimental-features 'nix-command flakes' \
        flake prefetch --refresh --json "$reference" > "$answer"
    cat >> "$tmp_dir/pins.rs" <<EOF
pub const ${name}_OWNER: &str = "$owner";
pub const ${name}_REPO: &str = "$repo";
pub const ${name}_BRANCH: &str = "$branch";
pub const ${name}_REV: &str = "$(answered "$answer" locked.rev)";
pub const ${name}_NAR_HASH: &str = "$(answered "$answer" hash)";
pub const ${name}_LAST_MODIFIED: u64 = $(answered "$answer" locked.lastModified);
EOF
done

cat >> "$tmp_dir/pins.rs" <<'EOF'

pub struct TarballPin {
    pub target: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub const NIX_TARBALLS: &[TarballPin] = &[
EOF

for target in "${targets[@]}"; do
    url="https://releases.nixos.org/nix/nix-$version/nix-$version-$target.tar.xz"
    file="$tmp_dir/nix-$version-$target.tar.xz"
    echo "fetching $url" >&2
    curl -fsSL -o "$file" "$url"
    hash="$(sha256sum "$file" | awk '{print $1}')"
    size="$(stat -c %s "$file")"
    cat >> "$tmp_dir/pins.rs" <<EOF
    TarballPin {
        target: "$target",
        url: "$url",
        sha256: "$hash",
        size: $size,
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
