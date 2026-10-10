#!/bin/sh
# Build cleat.vsix without npm/vsce: a .vsix is a zip with a manifest.
# Usage: editors/vscode/package.sh && code --install-extension editors/vscode/cleat.vsix --force
set -eu
here=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/.*"version": "\(.*\)".*/\1/p' "$here/package.json" | head -1)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/extension"
cp -R "$here/package.json" "$here/extension.js" "$here/language-configuration.json" "$here/syntaxes" "$work/extension/"

cat > "$work/extension.vsixmanifest" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
  <Metadata>
    <Identity Language="en-US" Id="cleat" Version="$version" Publisher="cleat-lang" />
    <DisplayName>Cleat</DisplayName>
    <Description xml:space="preserve">Cleat language support</Description>
    <Categories>Programming Languages</Categories>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.80.0" />
    </Properties>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code" />
  </Installation>
  <Dependencies />
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true" />
  </Assets>
</PackageManifest>
EOF

cat > "$work/[Content_Types].xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension=".json" ContentType="application/json" />
  <Default Extension=".js" ContentType="application/javascript" />
  <Default Extension=".vsixmanifest" ContentType="text/xml" />
</Types>
EOF

rm -f "$here/cleat.vsix"
(cd "$work" && zip -qr "$here/cleat.vsix" "[Content_Types].xml" extension.vsixmanifest extension)
echo "built $here/cleat.vsix"
