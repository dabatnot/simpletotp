#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
cargo build --release --locked

bin_dir="${HOME}/.local/bin"
apps_dir="${HOME}/.local/share/applications"
autostart_dir="${HOME}/.config/autostart"
install -Dm755 target/release/simpletotp "${bin_dir}/.simpletotp.new"
mv -f "${bin_dir}/.simpletotp.new" "${bin_dir}/simpletotp"
mkdir -p "${apps_dir}" "${autostart_dir}"

cat > "${apps_dir}/io.github.dabatnot.simpletotp.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=SimpleTOTP
Comment=Codes de connexion temporaires
Exec=${bin_dir}/simpletotp
Icon=dialog-password
Terminal=false
Categories=Utility;
EOF

cat > "${autostart_dir}/io.github.dabatnot.simpletotp.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=SimpleTOTP
Exec=${bin_dir}/simpletotp --background
Icon=dialog-password
Terminal=false
X-GNOME-Autostart-enabled=true
EOF

echo "SimpleTOTP est installé et démarrera à la prochaine connexion."
