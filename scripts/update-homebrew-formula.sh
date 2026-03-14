#!/usr/bin/env bash
set -euo pipefail

# Generate a Homebrew formula for siphone in a tap repo.
#
# Usage:
#   ./scripts/update-homebrew-formula.sh v0.1.0 /path/to/homebrew-tap/Formula/siphone.rb
#   ./scripts/update-homebrew-formula.sh 0.1.0 ./Formula/siphone.rb
#
# Environment overrides:
#   GITHUB_OWNER=xmppjingle
#   GITHUB_REPO=sipr

if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <version|tag> <formula-path>" >&2
  exit 1
fi

RAW_VERSION="$1"
FORMULA_PATH="$2"

if [[ "${RAW_VERSION}" == v* ]]; then
  VERSION="${RAW_VERSION#v}"
  TAG="${RAW_VERSION}"
else
  VERSION="${RAW_VERSION}"
  TAG="v${RAW_VERSION}"
fi

OWNER="${GITHUB_OWNER:-xmppjingle}"
REPO="${GITHUB_REPO:-sipr}"
URL="https://github.com/${OWNER}/${REPO}/archive/refs/tags/${TAG}.tar.gz"

TMP_ARCHIVE="$(mktemp)"
trap 'rm -f "${TMP_ARCHIVE}"' EXIT

curl -fsSL "${URL}" -o "${TMP_ARCHIVE}"
SHA256="$(shasum -a 256 "${TMP_ARCHIVE}" | awk '{print $1}')"

mkdir -p "$(dirname "${FORMULA_PATH}")"

cat > "${FORMULA_PATH}" <<EOF
class Siphone < Formula
  desc "SIP CLI softphone application"
  homepage "https://github.com/${OWNER}/${REPO}"
  url "${URL}"
  sha256 "${SHA256}"
  license "Apache-2.0"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--path", "siphone", *std_cargo_args
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/siphone --version")
  end
end
EOF

echo "Updated ${FORMULA_PATH} for ${TAG} (${SHA256})"
