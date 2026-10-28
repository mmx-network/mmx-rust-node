#!/usr/bin/env bash
# Publish the already-tested installers from this workflow run, together.
set -euo pipefail

[[ ${GITHUB_REF:-} == refs/heads/main && ${GITHUB_EVENT_NAME:-} != pull_request ]] || {
    printf 'Only main builds can publish wallet releases.\n' >&2; exit 1;
}
[[ ${GITHUB_RUN_NUMBER:-} =~ ^[1-9][0-9]*$ && ${GITHUB_SHA:-} =~ ^[0-9a-f]{40}$ ]] || {
    printf 'Missing or invalid workflow build identity.\n' >&2; exit 1;
}
repo=${GITHUB_REPOSITORY:?Missing repository}
packages=$(cd -- "${1:?Usage: publish-release.sh PACKAGES}" && pwd)
wallet=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=$(node -p 'require(process.argv[1]).version' "$wallet/package.json")
tag=wallet-v${version}-build.${GITHUB_RUN_NUMBER}
assets=(
    "mmx-wallet_${version}_amd64.deb"
    "mmx-wallet_${version}_amd64.AppImage"
    "mmx-wallet_${version}_macos_universal.dmg"
    "mmx-wallet_${version}_x64-setup.exe"
    sha256sums-linux-amd64 sha256sums-macos-universal sha256sums-windows-x64
)
cd -- "$packages"
for file in "${assets[@]}"; do
    [[ -f $file && -s $file && ! -L $file ]] || { printf 'Missing package: %s\n' "$file" >&2; exit 1; }
done
# Compare the complete manifests: a missing checksum must fail too.
sha256sum "${assets[0]}" "${assets[1]}" | diff - sha256sums-linux-amd64
sha256sum "${assets[2]}" | diff - sha256sums-macos-universal
sha256sum "${assets[3]}" | diff - sha256sums-windows-x64

if release=$(gh release view "$tag" --repo "$repo" --json isDraft,targetCommitish 2>/dev/null); then
    target=$(node -p 'JSON.parse(process.argv[1]).targetCommitish' "$release")
    [[ $target == "$GITHUB_SHA" ]] || { printf 'Release belongs to another commit.\n' >&2; exit 1; }
    draft=$(node -p 'JSON.parse(process.argv[1]).isDraft' "$release")
    if [[ $draft == false ]]; then
        printf 'Release already published: https://github.com/%s/releases/tag/%s\n' "$repo" "$tag"
        exit 0
    fi
else
    notes=$(mktemp)
    trap 'rm -f -- "$notes"' EXIT
    cat > "$notes" <<EOF
MMX Wallet ${version}, build ${GITHUB_RUN_NUMBER}.

| Platform | Installer |
| --- | --- |
| Windows 10/11 x64 | [Setup](https://github.com/${repo}/releases/download/${tag}/${assets[3]}) |
| macOS, Apple silicon and Intel | [Universal DMG](https://github.com/${repo}/releases/download/${tag}/${assets[2]}) |
| Linux x86_64 | [deb](https://github.com/${repo}/releases/download/${tag}/${assets[0]}) · [AppImage](https://github.com/${repo}/releases/download/${tag}/${assets[1]}) |

SHA-256 checksum files are included for each platform.

The desktop packages also include the mmxwallet command-line tool. On Linux deb
installations, run mmxwallet directly; AppImage users can run the AppImage with
--cli followed by the wallet command. On Windows it is beside the wallet app;
on macOS it is in MMX Wallet.app/Contents/MacOS/mmxwallet.

Source: [${GITHUB_SHA}](https://github.com/${repo}/commit/${GITHUB_SHA}).
Built and tested by [Wallet #${GITHUB_RUN_NUMBER}](https://github.com/${repo}/actions/runs/${GITHUB_RUN_ID}).
EOF
    gh release create "$tag" --repo "$repo" --target "$GITHUB_SHA" \
        --title "MMX Wallet ${version} (build ${GITHUB_RUN_NUMBER})" \
        --notes-file "$notes" --draft --latest=false
fi

# A failed upload leaves a draft; retrying can replace its partial uploads.
# Published releases are never overwritten, including on workflow reruns.
gh release upload "$tag" "${assets[@]}" --repo "$repo" --clobber
latest=true
latest_tag=$(gh release view --repo "$repo" --json tagName --jq .tagName 2>/dev/null || true)
if [[ $latest_tag =~ ^wallet-v.*-build\.([0-9]+)$ ]] && (( BASH_REMATCH[1] > GITHUB_RUN_NUMBER )); then
    latest=false
fi
gh release edit "$tag" --repo "$repo" --draft=false --latest="$latest"
printf 'Published https://github.com/%s/releases/tag/%s\n' "$repo" "$tag"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
    printf '[Download MMX Wallet %s (build %s)](https://github.com/%s/releases/tag/%s)\n' \
        "$version" "$GITHUB_RUN_NUMBER" "$repo" "$tag" >> "$GITHUB_STEP_SUMMARY"
fi
